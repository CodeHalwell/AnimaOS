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
//! | `GET /metrics`    | Prometheus exposition-format metrics (E21).                    |
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
//! - Failed token attempts are rate-limited per source IP: an address is
//!   locked out (HTTP 429) after [`MAX_AUTH_FAILURES`] failures within
//!   [`AUTH_FAILURE_WINDOW`], throttling brute-force guessing of a weak token.
//! - Inbound guidance is validated against the agent's `HumanGuidance` policy
//!   bounds by `packetize_text_checked` before it ever enters the queue, and is
//!   then still arbitrated by the Striatal Gate. The console cannot preempt the
//!   kernel.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// Maximum failed bearer-token attempts from one source IP within
/// [`AUTH_FAILURE_WINDOW`] before that IP is temporarily locked out.
const MAX_AUTH_FAILURES: usize = 5;

/// Trailing window over which failed-auth attempts are counted. The lockout
/// naturally clears this long after the last counted failure.
const AUTH_FAILURE_WINDOW: Duration = Duration::from_secs(60);

/// Per-source-IP failed-auth tracker that throttles bearer-token brute force.
///
/// A sliding window of failure timestamps is kept per IP; once an IP reaches
/// [`MAX_AUTH_FAILURES`] within [`AUTH_FAILURE_WINDOW`] it is locked out until
/// the window slides past those failures. Rejected-while-locked attempts are
/// *not* recorded, so an attacker cannot extend their own lockout indefinitely
/// (nor lock out a spoofed victim forever).
#[derive(Default)]
struct AuthRateLimiter {
    failures: Mutex<HashMap<IpAddr, Vec<Instant>>>,
}

impl AuthRateLimiter {
    /// Drop failure timestamps for `ip` that have aged out of the window,
    /// removing the entry entirely when none remain.
    fn prune(map: &mut HashMap<IpAddr, Vec<Instant>>, ip: IpAddr, now: Instant) {
        if let Some(v) = map.get_mut(&ip) {
            v.retain(|t| now.duration_since(*t) < AUTH_FAILURE_WINDOW);
            if v.is_empty() {
                map.remove(&ip);
            }
        }
    }

    /// Whether `ip` is currently locked out.
    fn is_locked(&self, ip: IpAddr) -> bool {
        let mut map = self.failures.lock().expect("poisoned");
        Self::prune(&mut map, ip, Instant::now());
        map.get(&ip).is_some_and(|v| v.len() >= MAX_AUTH_FAILURES)
    }

    /// Record a failed attempt. Returns `true` only on the transition that
    /// first reaches the lockout threshold, so callers can audit-log it once.
    fn record_failure(&self, ip: IpAddr) -> bool {
        let mut map = self.failures.lock().expect("poisoned");
        let now = Instant::now();
        Self::prune(&mut map, ip, now);
        let v = map.entry(ip).or_default();
        let was_below = v.len() < MAX_AUTH_FAILURES;
        v.push(now);
        was_below && v.len() >= MAX_AUTH_FAILURES
    }

    /// Clear an IP's failure history after a successful auth.
    fn record_success(&self, ip: IpAddr) {
        self.failures.lock().expect("poisoned").remove(&ip);
    }
}

/// The operator console HTTP/SSE server.
pub struct ConsoleServer {
    hub: Arc<ConsoleHub>,
    bridge: SensoryBridge,
    config: ServerConfig,
    limiter: AuthRateLimiter,
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
            limiter: AuthRateLimiter::default(),
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
        let peer_ip = stream.peer_addr().map(|a| a.ip()).ok();
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
        let mut last_event_id: Option<u64> = None;
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
                    // Sent automatically by EventSource on reconnect; lets the
                    // snapshot replay skip events the client already rendered.
                    "last-event-id" => last_event_id = value.parse().ok(),
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
        if path != "/healthz" {
            // Throttling only matters when a token actually gates access; an
            // open loopback server never produces an auth failure to count.
            let enforce = self.config.token.is_some();

            // Reject locked-out sources before even inspecting the credential,
            // so a brute-force loop hits a wall rather than a constant-time
            // token comparison it can keep hammering.
            if enforce {
                if let Some(ip) = peer_ip {
                    if self.limiter.is_locked(ip) {
                        return write_response(
                            &mut out,
                            429,
                            "Too Many Requests",
                            "text/plain; charset=utf-8",
                            b"too many failed authentication attempts; retry later\n",
                        );
                    }
                }
            }

            if !self.authorised(auth_header.as_deref(), &query) {
                if enforce {
                    if let Some(ip) = peer_ip {
                        if self.limiter.record_failure(ip) {
                            // One-shot audit entry on the lockout transition.
                            self.hub.publish(OperatorEvent::Audit {
                                kind: "AuthLockout".to_string(),
                                detail: format!(
                                    "source {ip} locked out after {MAX_AUTH_FAILURES} failed console auth attempts"
                                ),
                            });
                        }
                    }
                }
                return write_response(
                    &mut out,
                    401,
                    "Unauthorized",
                    "text/plain; charset=utf-8",
                    b"unauthorized\n",
                );
            }

            // A successful auth clears any prior failure streak for this IP.
            if enforce {
                if let Some(ip) = peer_ip {
                    self.limiter.record_success(ip);
                }
            }
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
            ("GET", "/events") => self.serve_events(out, last_event_id),
            ("POST", "/guidance") => self.serve_guidance(&mut reader, content_length, &mut out),
            // E21 — Prometheus metrics endpoint
            ("GET", "/metrics") => {
                let body = self.hub.render_metrics();
                write_response(
                    &mut out,
                    200,
                    "OK",
                    "text/plain; version=0.0.4; charset=utf-8",
                    body.as_bytes(),
                )
            }
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
                // Constant-time compare so a network attacker can't recover the
                // token byte-by-byte from response-timing differences.
                if constant_time_str_eq(tok, expected) {
                    return true;
                }
            }
        }
        // EventSource can't set request headers, so we also accept the token as a
        // `?token=` query parameter. This is a deliberate trade-off: query
        // strings are more prone to leaking into proxy/access logs than headers,
        // but it is the only way browser EventSource clients can authenticate.
        // The lockout limiter (per-IP) and constant-time comparison mitigate the
        // brute-force / timing risk this introduces.
        query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .any(|(k, v)| k == "token" && constant_time_str_eq(v, expected))
    }

    /// Stream the live event feed as Server-Sent Events. Blocks this
    /// connection's thread until the client disconnects.
    ///
    /// Every event is written with an `id:` line carrying the hub's publish
    /// sequence number. EventSource clients echo the last id they saw as a
    /// `Last-Event-ID` header on automatic reconnect; snapshot entries at or
    /// below that cursor are skipped so a network blip does not duplicate
    /// chat bubbles and feed lines on an already-rendered page. Ids are the
    /// audit-file byte offsets of the events' source lines, so they remain
    /// stable across server restarts (the freshly-started tailer re-reads
    /// the same file and republishes the same lines with the same ids) —
    /// a reconnecting page is replayed only what it has not seen, whether
    /// the gap was a network blip or a full agent restart.
    fn serve_events(&self, mut out: TcpStream, last_event_id: Option<u64>) -> std::io::Result<()> {
        let head = "HTTP/1.1 200 OK\r\n\
             Content-Type: text/event-stream\r\n\
             Cache-Control: no-cache\r\n\
             Connection: keep-alive\r\n\
             Access-Control-Allow-Origin: *\r\n\
             \r\n";
        out.write_all(head.as_bytes())?;
        out.flush()?;

        let sub = self.hub.subscribe();
        // Replay the snapshot so a freshly-opened dashboard has immediate
        // state, skipping anything a reconnecting client already rendered.
        for (seq, event) in &sub.snapshot {
            if last_event_id.is_some_and(|last| *seq <= last) {
                continue;
            }
            if write_sse(&mut out, Some(*seq), event).is_err() {
                self.hub.unsubscribe(sub.id());
                return Ok(());
            }
        }

        loop {
            match sub.rx.recv_timeout(Duration::from_secs(15)) {
                Ok((seq, event)) => {
                    // The same cursor filter as the snapshot: after a restart
                    // the tailer re-reads the audit file from offset 0 and
                    // republishes history through this live path; a client
                    // that reconnected mid-catch-up must not see lines it
                    // already rendered. In steady state live seqs are always
                    // above the cursor, so this never filters fresh events.
                    if last_event_id.is_some_and(|last| seq <= last) {
                        continue;
                    }
                    if write_sse(&mut out, Some(seq), &event).is_err() {
                        break;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // Idle keep-alive so proxies and the client don't time out.
                    // No `id:` line — heartbeats are synthesised per-connection
                    // and must not advance the client's replay cursor.
                    let beat = OperatorEvent::Heartbeat {
                        uptime_secs: self.hub.uptime_secs(),
                    };
                    if write_sse(&mut out, None, &beat).is_err() {
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
        // The operator is a potentially-compromised channel (threat model §5),
        // so bound the request body and reject anything we cannot faithfully
        // decode rather than silently truncating it or lossily mangling bytes
        // into U+FFFD — either of which could smuggle an unintended command
        // past the policy bounds applied downstream.
        const MAX_BODY: usize = 64 * 1024;
        if content_length > MAX_BODY {
            return write_json(
                out,
                413,
                "Payload Too Large",
                br#"{"ok":false,"error":"request body exceeds 64 KiB limit"}"#,
            );
        }
        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body)?;
        let Ok(body) = String::from_utf8(body) else {
            return write_json(
                out,
                400,
                "Bad Request",
                br#"{"ok":false,"error":"request body is not valid UTF-8"}"#,
            );
        };

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
                let detail = if let Some(reason) = input.force.as_deref() {
                    format!(
                        "[FORCED:Critical] (Reason: {reason}) {}",
                        truncate(&input.text, 200)
                    )
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

/// Constant-time string equality for secret comparison.
///
/// Folds an XOR accumulator over the bytes so the running time depends only on
/// the input length, not on where the first differing byte appears — denying a
/// network attacker a timing oracle for recovering the bearer token. Differing
/// lengths return `false` without a content short-circuit.
fn constant_time_str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn write_sse(out: &mut TcpStream, id: Option<u64>, event: &OperatorEvent) -> std::io::Result<()> {
    if let Some(id) = id {
        out.write_all(format!("id: {id}\n").as_bytes())?;
    }
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
            max_pcm_samples: None,
            blocked_prefixes: vec![],
            max_image_bytes: None,
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

        let pkt = bridge
            .next_prioritized_packet()
            .expect("forced packet enqueued");
        assert_eq!(pkt.priority, SensoryPriority::Critical);
        assert_eq!(
            pkt.gate_override_reason.as_deref(),
            Some("on-call escalation"),
            "gate_override_reason must carry the force value"
        );
        assert!(matches!(&pkt.packet, senses::SensoryPacket::Text(t) if t.contains("rollback")));
    }

    fn http_request_bytes(addr: std::net::SocketAddr, raw: &[u8]) -> String {
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(raw).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut buf = String::new();
        let _ = s.read_to_string(&mut buf);
        buf
    }

    #[test]
    fn post_guidance_rejects_oversized_body() {
        // A Content-Length beyond the 64 KiB cap must be rejected outright
        // rather than silently truncated to a partial (and possibly altered)
        // command. The server should answer before reading the body.
        let (addr, _hub, bridge) = start();
        let raw = "POST /guidance HTTP/1.1\r\nHost: x\r\nContent-Length: 70000\r\nConnection: close\r\n\r\n";
        let resp = http_request(addr, raw);
        assert!(
            resp.contains("413"),
            "expected 413 Payload Too Large, got: {resp}"
        );
        assert!(
            bridge.next_prioritized_packet().is_none(),
            "no packet should be enqueued for a rejected oversized body"
        );
    }

    #[test]
    fn post_guidance_rejects_non_utf8_body() {
        // Invalid UTF-8 must be rejected, not lossily replaced with U+FFFD,
        // which could smuggle a mangled command past the policy bounds.
        let (addr, _hub, bridge) = start();
        // Body: {"text":"<0xFF>"} — the 0xFF byte is not valid UTF-8.
        let mut body = br#"{"text":""#.to_vec();
        body.push(0xFF);
        body.extend_from_slice(br#""}"#);
        let mut raw = format!(
            "POST /guidance HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        raw.extend_from_slice(&body);
        let resp = http_request_bytes(addr, &raw);
        assert!(
            resp.contains("400"),
            "expected 400 Bad Request for non-UTF-8, got: {resp}"
        );
        assert!(
            bridge.next_prioritized_packet().is_none(),
            "no packet should be enqueued for a rejected non-UTF-8 body"
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
    fn token_accepted_as_query_param_for_eventsource() {
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

        // EventSource can't set headers; the ?token= query param must authorise.
        let resp = http_request(
            addr,
            "GET /?token=sekret HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(resp.contains("200 OK"), "resp: {resp}");

        // A wrong query token is still rejected.
        let resp = http_request(
            addr,
            "GET /?token=nope HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(resp.contains("401"), "resp: {resp}");
    }

    #[test]
    fn constant_time_str_eq_matches_naive_equality() {
        assert!(constant_time_str_eq("sekret", "sekret"));
        assert!(!constant_time_str_eq("sekret", "sekreT"));
        assert!(!constant_time_str_eq("sekret", "sekre")); // length mismatch
        assert!(!constant_time_str_eq("", "x"));
        assert!(constant_time_str_eq("", ""));
    }

    #[test]
    fn repeated_failed_auth_locks_out_the_source() {
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

        let bad =
            "GET / HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer wrong\r\nConnection: close\r\n\r\n";

        // The first MAX_AUTH_FAILURES bad attempts are answered with 401.
        for i in 0..MAX_AUTH_FAILURES {
            let resp = http_request(addr, bad);
            assert!(resp.contains("401"), "attempt {i} should be 401: {resp}");
        }

        // The next attempt is locked out with 429 — even with the *correct*
        // token, proving the lockout is checked before the credential.
        let resp = http_request(
            addr,
            "GET / HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer sekret\r\nConnection: close\r\n\r\n",
        );
        assert!(
            resp.contains("429"),
            "should be locked out after {MAX_AUTH_FAILURES} failures: {resp}"
        );
    }

    #[test]
    fn successful_auth_resets_the_failure_streak() {
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

        let bad =
            "GET / HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer wrong\r\nConnection: close\r\n\r\n";
        let good =
            "GET / HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer sekret\r\nConnection: close\r\n\r\n";

        // Stay one short of the threshold, then succeed (resets the counter)…
        for _ in 0..MAX_AUTH_FAILURES - 1 {
            assert!(http_request(addr, bad).contains("401"));
        }
        assert!(http_request(addr, good).contains("200 OK"));

        // …so a fresh run of bad attempts is again answered with 401, not 429.
        for _ in 0..MAX_AUTH_FAILURES - 1 {
            let resp = http_request(addr, bad);
            assert!(
                resp.contains("401"),
                "streak should have reset after success: {resp}"
            );
        }
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
        assert!(
            text.contains("id: "),
            "events must carry SSE ids for reconnect replay-skipping: {text}"
        );
    }

    #[test]
    fn reconnect_with_last_event_id_skips_already_seen_snapshot() {
        let (addr, hub, _bridge) = start();

        // Three feed events land in the replay ring as seqs 0, 1, 2.
        for (i, word) in ["alpha", "beta", "gamma"].iter().enumerate() {
            hub.publish(OperatorEvent::AgentMessage {
                task_id: i as u64,
                tokens: 1,
                text: (*word).into(),
            });
        }

        // Read until `until` appears or the deadline passes — a transient
        // read-timeout is NOT end-of-stream (CI runners pause mid-frame).
        let read_stream = |req: &str, until: &str| {
            let mut s = TcpStream::connect(addr).unwrap();
            s.write_all(req.as_bytes()).unwrap();
            s.set_read_timeout(Some(Duration::from_millis(200)))
                .unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut text = String::new();
            let mut buf = [0u8; 2048];
            while std::time::Instant::now() < deadline && !text.contains(until) {
                match s.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => text.push_str(&String::from_utf8_lossy(&buf[..n])),
                    Err(_) => {} // timeout tick — keep waiting for the frame
                }
            }
            text
        };

        // A fresh client (no Last-Event-ID) is replayed the whole ring.
        let fresh = read_stream("GET /events HTTP/1.1\r\nHost: x\r\n\r\n", "gamma");
        for word in ["alpha", "beta", "gamma"] {
            assert!(fresh.contains(word), "fresh client missing {word}: {fresh}");
        }

        // A reconnecting client that already saw seq 1 gets only seq 2.
        let resumed = read_stream(
            "GET /events HTTP/1.1\r\nHost: x\r\nLast-Event-ID: 1\r\n\r\n",
            "gamma",
        );
        assert!(
            !resumed.contains("alpha") && !resumed.contains("beta"),
            "events at or below the cursor must be skipped: {resumed}"
        );
        assert!(
            resumed.contains("gamma"),
            "events after the cursor must still replay: {resumed}"
        );
    }

    #[test]
    fn live_path_also_respects_last_event_id_cursor() {
        // After a process restart the audit tailer re-reads the file from
        // offset 0 and republishes history through the LIVE path; a client
        // that reconnected mid-catch-up must not see lines it already
        // rendered (Codex review, PR #114).
        let (addr, hub, _bridge) = start();

        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(b"GET /events HTTP/1.1\r\nHost: x\r\nLast-Event-ID: 10\r\n\r\n")
            .unwrap();
        s.set_read_timeout(Some(Duration::from_millis(400)))
            .unwrap();
        std::thread::sleep(Duration::from_millis(100));

        // Historical re-read (seq below the cursor) vs genuinely new line.
        hub.publish_at(
            5,
            OperatorEvent::AgentMessage {
                task_id: 1,
                tokens: 1,
                text: "historical-replay".into(),
            },
        );
        hub.publish_at(
            11,
            OperatorEvent::AgentMessage {
                task_id: 2,
                tokens: 1,
                text: "fresh-line".into(),
            },
        );

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut text = String::new();
        let mut buf = [0u8; 2048];
        while std::time::Instant::now() < deadline && !text.contains("fresh-line") {
            match s.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => text.push_str(&String::from_utf8_lossy(&buf[..n])),
                Err(_) => {} // timeout tick — keep waiting
            }
        }
        assert!(
            !text.contains("historical-replay"),
            "live events at or below the cursor must be filtered: {text}"
        );
        assert!(
            text.contains("fresh-line"),
            "live events above the cursor must flow: {text}"
        );
    }

    // ── E21: /metrics endpoint ─────────────────────────────────────────────────

    #[test]
    fn metrics_endpoint_returns_prometheus_text() {
        let (addr, _hub, _bridge) = start();
        let resp = http_request(
            addr,
            "GET /metrics HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(resp.contains("200 OK"), "status: {resp}");
        assert!(
            resp.contains("text/plain; version=0.0.4"),
            "content-type: {resp}"
        );
        assert!(
            resp.contains("# HELP anima_tasks_total"),
            "prometheus help line: {resp}"
        );
        assert!(
            resp.contains("# TYPE anima_tasks_total counter"),
            "prometheus type line: {resp}"
        );
    }

    #[test]
    fn metrics_endpoint_reflects_audit_updates_via_hub() {
        let (addr, hub, _bridge) = start();

        // Feed a gate decision directly into the hub metrics registry.
        let audit_line = r#"{"GateDecision":{"agent_id":"a","event_id":"e1","invoke":true,"cost_class":"Frontier","urgency":0.9,"novelty":0.5,"user_facing":true,"semantic_class":"UserQuery","value_score":0.82,"threshold_applied":0.4,"thermal_load":0.1,"compute_pressure":0.0,"memory_pressure":0.0,"power_budget":1.0,"financial_budget":1.0,"attention_demand":0.7,"reasoning":"test","override_active":false}}"#;
        hub.update_metrics_from_json(audit_line);

        let resp = http_request(
            addr,
            "GET /metrics HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(
            resp.contains("anima_gate_decisions_total{outcome=\"invoke\"} 1"),
            "gate counter: {resp}"
        );
        assert!(
            resp.contains("anima_gate_invocations_total{cost_class=\"Frontier\"} 1"),
            "cost_class label: {resp}"
        );
    }

    #[test]
    fn metrics_endpoint_requires_auth_when_token_is_configured() {
        let hub = Arc::new(ConsoleHub::new());
        let bridge = senses::SensoryBridge::new(senses::HumanGuidance::new("test"));
        let server = ConsoleServer::new(
            hub.clone(),
            bridge,
            ServerConfig {
                addr: "127.0.0.1:0".into(),
                token: Some("secret123".into()),
            },
        );
        let (addr, _h) = server.spawn().expect("spawn");

        // Without token → 401.
        let resp_no_token = http_request(
            addr,
            "GET /metrics HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(
            resp_no_token.contains("401"),
            "should require auth: {resp_no_token}"
        );

        // With correct token → 200.
        let resp_with_token = http_request(
            addr,
            "GET /metrics HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer secret123\r\nConnection: close\r\n\r\n",
        );
        assert!(
            resp_with_token.contains("200 OK"),
            "should accept valid token: {resp_with_token}"
        );
    }
}
