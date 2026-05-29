//! A dependency-free HTTP/1.1 server exposing the operator console.
//!
//! Three routes, all on one loopback-friendly port:
//!
//! | Method + path     | Purpose                                                        |
//! |-------------------|----------------------------------------------------------------|
//! | `GET /`           | The self-contained browser dashboard (HTML + vanilla JS).      |
//! | `GET /events`     | Server-Sent Events: the live [`OperatorEvent`] stream.         |
//! | `POST /guidance`  | Afferent ingress — an [`OperatorInput`] becomes a sensory packet. |
//! | `GET /healthz`    | Liveness probe.                                                |
//!
//! It is hand-rolled on `std::net` (thread-per-connection) precisely so the
//! `console` crate pulls in **no** third-party HTTP stack — keeping the
//! workspace's supply-chain audit (`deny.toml`) and build times unchanged.
//!
//! # Security
//!
//! - Bind to loopback (`127.0.0.1`) by default; the container maps only the
//!   loopback port to the host, mirroring the Ollama daemon.
//! - An optional bearer token gates every route except `/healthz`. Browsers
//!   using `EventSource` can't set headers, so the token is also accepted as a
//!   `?token=` query parameter for `GET /events`.
//! - Inbound guidance is validated against the agent's `HumanGuidance` policy
//!   bounds by `packetize_text_checked` before it ever enters the queue, and is
//!   then still arbitrated by the Striatal Gate. The console cannot preempt the
//!   kernel.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use console_proto::{json, OperatorEvent, Priority};
use senses::{SensoryBridge, SensoryBridgeError, SensoryPriority};

use crate::hub::ConsoleHub;

/// Configuration for [`ConsoleServer`].
#[derive(Clone)]
pub struct ServerConfig {
    /// Bind address, e.g. `127.0.0.1:8088`.
    pub addr: String,
    /// Optional bearer token. When `Some`, all routes except `/healthz`
    /// require it (header `Authorization: Bearer <t>` or `?token=<t>`).
    pub token: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            addr: "127.0.0.1:8088".to_string(),
            token: None,
        }
    }
}

impl ServerConfig {
    /// Build a config from `ANIMA_CONSOLE_ADDR` / `ANIMA_CONSOLE_TOKEN`,
    /// falling back to loopback `:8088` and no token.
    pub fn from_env() -> Self {
        Self {
            addr: std::env::var("ANIMA_CONSOLE_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:8088".to_string()),
            token: std::env::var("ANIMA_CONSOLE_TOKEN")
                .ok()
                .filter(|t| !t.is_empty()),
        }
    }
}

/// The operator console HTTP/SSE server.
pub struct ConsoleServer {
    hub: Arc<ConsoleHub>,
    bridge: SensoryBridge,
    config: ServerConfig,
}

/// Map a protocol priority onto the `senses` priority enum.
fn to_sensory_priority(p: Priority) -> SensoryPriority {
    match p {
        Priority::Low => SensoryPriority::Low,
        Priority::Normal => SensoryPriority::Normal,
        Priority::High => SensoryPriority::High,
        Priority::Critical => SensoryPriority::Critical,
    }
}

impl ConsoleServer {
    /// Create a server over a shared hub and sensory bridge.
    ///
    /// The `bridge` should be a clone of the bridge the agent's lifecycle owns
    /// (`SensoryBridge` is `Clone` and shares its queue), so POSTed guidance
    /// lands in the very queue the somatic loop drains.
    pub fn new(hub: Arc<ConsoleHub>, bridge: SensoryBridge, config: ServerConfig) -> Self {
        Self {
            hub,
            bridge,
            config,
        }
    }

    /// Bind the listener (so the caller learns the resolved local address) and
    /// return it without yet serving. Useful for tests that need the OS-chosen
    /// port from `127.0.0.1:0`.
    pub fn bind(&self) -> std::io::Result<TcpListener> {
        let addr =
            self.config.addr.to_socket_addrs()?.next().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "no address")
            })?;
        TcpListener::bind(addr)
    }

    /// Serve forever on an already-bound listener. Blocks the calling thread.
    pub fn serve(self: Arc<Self>, listener: TcpListener) {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let server = Arc::clone(&self);
            std::thread::Builder::new()
                .name("anima-console-conn".into())
                .spawn(move || {
                    let _ = server.handle(stream);
                })
                .ok();
        }
    }

    /// Bind and serve on a background thread, returning the resolved address.
    pub fn spawn(self) -> std::io::Result<(std::net::SocketAddr, std::thread::JoinHandle<()>)> {
        let listener = self.bind()?;
        let addr = listener.local_addr()?;
        let server = Arc::new(self);
        let handle = std::thread::Builder::new()
            .name("anima-console-server".into())
            .spawn(move || server.serve(listener))
            .expect("spawn console server");
        Ok((addr, handle))
    }

    fn handle(&self, stream: TcpStream) -> std::io::Result<()> {
        let mut reader = BufReader::new(stream.try_clone()?);

        // ── Request line ──────────────────────────────────────────────────
        let mut request_line = String::new();
        if reader.read_line(&mut request_line)? == 0 {
            return Ok(());
        }
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let target = parts.next().unwrap_or("/").to_string();
        let (path, query) = match target.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (target, String::new()),
        };

        // ── Headers ───────────────────────────────────────────────────────
        let mut content_length = 0usize;
        let mut auth_header: Option<String> = None;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                break;
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':') {
                let key = k.trim().to_ascii_lowercase();
                let value = v.trim();
                match key.as_str() {
                    "content-length" => content_length = value.parse().unwrap_or(0),
                    "authorization" => auth_header = Some(value.to_string()),
                    _ => {}
                }
            }
        }

        let mut out = stream;

        // ── CORS preflight ─────────────────────────────────────────────────
        if method == "OPTIONS" {
            return write_cors_preflight(&mut out);
        }

        // ── Auth (everything except the health probe) ──────────────────────
        if path != "/healthz" && !self.authorised(auth_header.as_deref(), &query) {
            return write_response(
                &mut out,
                401,
                "Unauthorized",
                "text/plain; charset=utf-8",
                b"unauthorized\n",
            );
        }

        match (method.as_str(), path.as_str()) {
            ("GET", "/healthz") => {
                write_response(&mut out, 200, "OK", "text/plain; charset=utf-8", b"ok\n")
            }
            ("GET", "/") | ("GET", "/index.html") => write_response(
                &mut out,
                200,
                "OK",
                "text/html; charset=utf-8",
                crate::DASHBOARD_HTML.as_bytes(),
            ),
            ("GET", "/events") => self.serve_events(out),
            ("POST", "/guidance") => self.serve_guidance(&mut reader, content_length, &mut out),
            _ => write_response(
                &mut out,
                404,
                "Not Found",
                "text/plain; charset=utf-8",
                b"not found\n",
            ),
        }
    }

    fn authorised(&self, auth_header: Option<&str>, query: &str) -> bool {
        let Some(expected) = self.config.token.as_deref() else {
            return true; // no token configured → open (loopback dev default)
        };
        if let Some(h) = auth_header {
            if let Some(tok) = h.strip_prefix("Bearer ") {
                if tok == expected {
                    return true;
                }
            }
        }
        // EventSource can't set headers — accept ?token= as well.
        query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .any(|(k, v)| k == "token" && v == expected)
    }

    /// Stream the live event feed as Server-Sent Events. Blocks this
    /// connection's thread until the client disconnects.
    fn serve_events(&self, mut out: TcpStream) -> std::io::Result<()> {
        let head = "HTTP/1.1 200 OK\r\n\
             Content-Type: text/event-stream\r\n\
             Cache-Control: no-cache\r\n\
             Connection: keep-alive\r\n\
             Access-Control-Allow-Origin: *\r\n\
             \r\n";
        out.write_all(head.as_bytes())?;
        out.flush()?;

        let sub = self.hub.subscribe();
        // Replay the snapshot so a freshly-opened dashboard has immediate state.
        for event in &sub.snapshot {
            if write_sse(&mut out, event).is_err() {
                self.hub.unsubscribe(sub.id());
                return Ok(());
            }
        }

        loop {
            match sub.rx.recv_timeout(Duration::from_secs(15)) {
                Ok(event) => {
                    if write_sse(&mut out, &event).is_err() {
                        break;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // Idle keep-alive so proxies and the client don't time out.
                    let beat = OperatorEvent::Heartbeat {
                        uptime_secs: self.hub.uptime_secs(),
                    };
                    if write_sse(&mut out, &beat).is_err() {
                        break;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        self.hub.unsubscribe(sub.id());
        Ok(())
    }

    /// Accept an [`OperatorInput`] and inject it into the sensory bridge.
    fn serve_guidance(
        &self,
        reader: &mut BufReader<TcpStream>,
        content_length: usize,
        out: &mut TcpStream,
    ) -> std::io::Result<()> {
        // Cap the body to a sane size to bound memory regardless of policy.
        const MAX_BODY: usize = 64 * 1024;
        let to_read = content_length.min(MAX_BODY);
        let mut body = vec![0u8; to_read];
        reader.read_exact(&mut body)?;
        let body = String::from_utf8_lossy(&body);

        let Some(input) = json::input_from_line(&body) else {
            return write_json(
                out,
                400,
                "Bad Request",
                br#"{"ok":false,"error":"invalid OperatorInput JSON"}"#,
            );
        };

        // E6.6: when `force` is set, route through `packetize_text_forced` so
        // vita's somatic loop can record an audited GateOverride::OperatorForced
        // entry.  Policy bounds still apply — the operator is a potentially-
        // compromised channel (threat model §5 in 11-operator-interface.md).
        let result = if let Some(reason) = input.force.as_deref() {
            self.bridge
                .packetize_text_forced(input.text.clone(), reason)
        } else {
            self.bridge
                .packetize_text_checked(input.text.clone(), to_sensory_priority(input.priority))
        };

        match result {
            Ok(()) => {
                // Echo the accepted guidance into the event feed so every
                // connected operator sees what was injected (and by implication,
                // that it is now subject to the gate, not executed directly).
                let detail = if input.force.is_some() {
                    format!("[FORCED:Critical] {}", truncate(&input.text, 200))
                } else {
                    format!(
                        "[{}] {}",
                        priority_label(to_sensory_priority(input.priority)),
                        truncate(&input.text, 200)
                    )
                };
                self.hub.publish(OperatorEvent::Audit {
                    kind: "OperatorGuidance".to_string(),
                    detail,
                });
                write_json(out, 202, "Accepted", br#"{"ok":true}"#)
            }
            Err(SensoryBridgeError::PolicyViolation { reason }) => {
                let body = format!(r#"{{"ok":false,"error":{}}}"#, json_string(&reason));
                write_json(out, 422, "Unprocessable Entity", body.as_bytes())
            }
            Err(SensoryBridgeError::InvalidInput) => write_json(
                out,
                400,
                "Bad Request",
                br#"{"ok":false,"error":"invalid input"}"#,
            ),
        }
    }
}

fn priority_label(p: SensoryPriority) -> &'static str {
    match p {
        SensoryPriority::Low => "Low",
        SensoryPriority::Normal => "Normal",
        SensoryPriority::High => "High",
        SensoryPriority::Critical => "Critical",
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}

/// Minimal JSON string escaping for the few server-generated strings.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn write_sse(out: &mut TcpStream, event: &OperatorEvent) -> std::io::Result<()> {
    let line = json::event_to_line(event);
    out.write_all(b"data: ")?;
    out.write_all(line.as_bytes())?;
    out.write_all(b"\n\n")?;
    out.flush()
}

fn write_response(
    out: &mut TcpStream,
    code: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let header = format!(
        "HTTP/1.1 {code} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Connection: close\r\n\
         \r\n",
        body.len()
    );
    out.write_all(header.as_bytes())?;
    out.write_all(body)?;
    out.flush()
}

fn write_json(out: &mut TcpStream, code: u16, reason: &str, body: &[u8]) -> std::io::Result<()> {
    write_response(out, code, reason, "application/json; charset=utf-8", body)
}

fn write_cors_preflight(out: &mut TcpStream) -> std::io::Result<()> {
    let header = "HTTP/1.1 204 No Content\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
         Access-Control-Allow-Headers: Authorization, Content-Type\r\n\
         Content-Length: 0\r\n\
         Connection: close\r\n\
         \r\n";
    out.write_all(header.as_bytes())?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use senses::HumanGuidance;

    fn start() -> (std::net::SocketAddr, Arc<ConsoleHub>, SensoryBridge) {
        let hub = Arc::new(ConsoleHub::new());
        let bridge = SensoryBridge::new(HumanGuidance::new("test"));
        let server = ConsoleServer::new(
            hub.clone(),
            bridge.clone(),
            ServerConfig {
                addr: "127.0.0.1:0".into(),
                token: None,
            },
        );
        let (addr, _h) = server.spawn().expect("spawn");
        (addr, hub, bridge)
    }

    fn http_request(addr: std::net::SocketAddr, raw: &str) -> String {
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(raw.as_bytes()).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut buf = String::new();
        let _ = s.read_to_string(&mut buf);
        buf
    }

    #[test]
    fn healthz_returns_ok() {
        let (addr, _hub, _bridge) = start();
        let resp = http_request(
            addr,
            "GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(resp.contains("200 OK"), "resp: {resp}");
        assert!(resp.contains("ok"));
    }

    #[test]
    fn root_serves_dashboard_html() {
        let (addr, _hub, _bridge) = start();
        let resp = http_request(
            addr,
            "GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(resp.contains("text/html"));
        assert!(resp.contains("AnimaOS"), "dashboard should mention AnimaOS");
    }

    #[test]
    fn post_guidance_lands_in_the_bridge() {
        let (addr, _hub, bridge) = start();
        let body = r#"{"text":"please summarise the overnight logs","priority":"High"}"#;
        let raw = format!(
            "POST /guidance HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let resp = http_request(addr, &raw);
        assert!(resp.contains("202 Accepted"), "resp: {resp}");

        let pkt = bridge.next_prioritized_packet().expect("packet enqueued");
        assert_eq!(pkt.priority, SensoryPriority::High);
        assert!(matches!(pkt.packet, senses::SensoryPacket::Text(t) if t.contains("overnight")));
    }

    #[test]
    fn post_guidance_rejects_policy_violation() {
        let hub = Arc::new(ConsoleHub::new());
        let bridge = SensoryBridge::new(HumanGuidance {
            policy_hint: "strict".into(),
            max_text_length: Some(4),
            blocked_prefixes: vec![],
        });
        let server = ConsoleServer::new(
            hub,
            bridge,
            ServerConfig {
                addr: "127.0.0.1:0".into(),
                token: None,
            },
        );
        let (addr, _h) = server.spawn().unwrap();

        let body = r#"{"text":"way too long for the policy"}"#;
        let raw = format!(
            "POST /guidance HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let resp = http_request(addr, &raw);
        assert!(
            resp.contains("422"),
            "expected policy rejection, got: {resp}"
        );
    }

    #[test]
    fn post_guidance_with_force_produces_critical_forced_packet() {
        // E6.6: POST /guidance with "force" set must produce a forced packet
        // (gate_override_reason set, priority Critical) via packetize_text_forced.
        let (addr, _hub, bridge) = start();
        let body = r#"{"text":"deploy the rollback","force":"on-call escalation"}"#;
        let raw = format!(
            "POST /guidance HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let resp = http_request(addr, &raw);
        assert!(resp.contains("202 Accepted"), "resp: {resp}");

        let pkt = bridge.next_prioritized_packet().expect("forced packet enqueued");
        assert_eq!(pkt.priority, SensoryPriority::Critical);
        assert_eq!(
            pkt.gate_override_reason.as_deref(),
            Some("on-call escalation"),
            "gate_override_reason must carry the force value"
        );
        assert!(
            matches!(&pkt.packet, senses::SensoryPacket::Text(t) if t.contains("rollback"))
        );
    }

    #[test]
    fn token_is_required_when_configured() {
        let hub = Arc::new(ConsoleHub::new());
        let bridge = SensoryBridge::new(HumanGuidance::new("t"));
        let server = ConsoleServer::new(
            hub,
            bridge,
            ServerConfig {
                addr: "127.0.0.1:0".into(),
                token: Some("sekret".into()),
            },
        );
        let (addr, _h) = server.spawn().unwrap();

        // No token → 401.
        let resp = http_request(
            addr,
            "GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(resp.contains("401"), "resp: {resp}");

        // Correct bearer token → 200.
        let resp = http_request(
            addr,
            "GET / HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer sekret\r\nConnection: close\r\n\r\n",
        );
        assert!(resp.contains("200 OK"), "resp: {resp}");

        // Health probe is always open.
        let resp = http_request(
            addr,
            "GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(resp.contains("200 OK"));
    }

    #[test]
    fn events_stream_delivers_published_events() {
        let (addr, hub, _bridge) = start();
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(b"GET /events HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();

        // Give the connection thread time to subscribe, then publish.
        std::thread::sleep(Duration::from_millis(100));
        hub.publish(OperatorEvent::AgentMessage {
            task_id: 1,
            tokens: 3,
            text: "hello from the agent".into(),
        });

        // Read repeatedly: the first chunk is the SSE headers, the data frame
        // arrives once the publish above propagates through the hub.
        let mut text = String::new();
        let mut buf = [0u8; 1024];
        for _ in 0..20 {
            match s.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    text.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if text.contains("hello from the agent") {
                        break;
                    }
                }
                Err(_) => break, // read timeout
            }
        }
        assert!(text.contains("text/event-stream"), "headers: {text}");
        assert!(
            text.contains("hello from the agent"),
            "should receive the published event: {text}"
        );
    }
}
