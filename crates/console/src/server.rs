//! A dependency-free HTTP/1.1 server exposing the operator console.
//!
//! Routes served:
//!
//! | Method + path                          | Purpose                                                        |
//! |----------------------------------------|----------------------------------------------------------------|
//! | `GET /`                                | The self-contained browser dashboard (HTML + vanilla JS).      |
//! | `GET /events`                          | Server-Sent Events: the live [`OperatorEvent`] stream.         |
//! | `POST /guidance`                       | Afferent ingress — an [`OperatorInput`] becomes a sensory packet. |
//! | `GET /healthz`                         | Liveness probe.                                                |
//! | `GET /metrics`                         | Prometheus exposition-format metrics (E21).                    |
//! | `GET /approval-queue`                  | All approval proposals as JSON (E15 S15.2, optional).          |
//! | `POST /approval-queue/{id}/approve`    | Approve a pending proposal (E15 S15.2, optional).              |
//! | `POST /approval-queue/{id}/reject`     | Reject a pending proposal (E15 S15.2, optional).               |
//! | `GET /skills`                          | All skill entries as JSON (E11 S11.1, optional).               |
//! | `GET /adapters`                        | All adapter artifacts as JSON (E8, optional).                  |
//!
//! It is hand-rolled on `std::net` (thread-per-connection) precisely so the
//! `console` crate pulls in **no** third-party HTTP stack — keeping the
//! workspace's supply-chain audit (`deny.toml`) and build times unchanged.
//!
//! # Security
//!
//! - Bind to loopback (`127.0.0.1`) by default; the container maps only the
//!   loopback port to the host, mirroring the Ollama daemon. Binding a
//!   network-reachable address (anything non-loopback, including the
//!   `0.0.0.0` / `::` wildcards) **requires** a non-empty token — see
//!   [`check_bind_policy`] — so an unauthenticated console can never be exposed
//!   beyond this host by accident.
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
        let mut map = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        Self::prune(&mut map, ip, Instant::now());
        map.get(&ip).is_some_and(|v| v.len() >= MAX_AUTH_FAILURES)
    }

    /// Record a failed attempt. Returns `true` only on the transition that
    /// first reaches the lockout threshold, so callers can audit-log it once.
    fn record_failure(&self, ip: IpAddr) -> bool {
        let mut map = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        Self::prune(&mut map, ip, now);
        let v = map.entry(ip).or_default();
        let was_below = v.len() < MAX_AUTH_FAILURES;
        v.push(now);
        was_below && v.len() >= MAX_AUTH_FAILURES
    }

    /// Clear an IP's failure history after a successful auth.
    fn record_success(&self, ip: IpAddr) {
        self.failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ip);
    }
}

/// The operator console HTTP/SSE server.
pub struct ConsoleServer {
    hub: Arc<ConsoleHub>,
    bridge: SensoryBridge,
    config: ServerConfig,
    limiter: AuthRateLimiter,
    /// Path to the vita audit JSONL file for `GET /digest`. `None` when not
    /// wired (the server returns 503 rather than panicking).
    digest_path: Option<std::path::PathBuf>,
    /// Agent ID used as the `agent_id` argument to `generate_digest`.
    digest_agent_id: String,
    /// Mtime-based cache for the serialised digest JSON: `(mtime, json)`.
    /// Avoids re-reading the full audit JSONL on every `GET /digest` call when
    /// the file has not changed.
    digest_cache: Mutex<Option<(std::time::SystemTime, String)>>,
    /// Optional approval queue — when set, `GET /approval-queue` and the
    /// approve/reject action endpoints are active (E15 S15.2 dashboard surface).
    approval_queue: Option<Arc<Mutex<lifecycle::approval::ApprovalQueue>>>,
    /// Optional skill registry — when set, `GET /skills` is active (E11 S11.1
    /// dashboard surface).
    skill_registry: Option<Arc<Mutex<skills::SkillRegistry>>>,
    /// Optional adapter library — when set, `GET /adapters` is active (E8
    /// adapter-library dashboard surface).
    adapter_library: Option<Arc<Mutex<anima_finetune::AdapterLibrary>>>,
    /// Operator identity, when wired: `(registry, user_id)` (E33 S33.5).
    ///
    /// Answers "who does this console think I am": the profile conversations
    /// and feedback are recorded against, and the trust tier the E17 registry
    /// holds for them.  The bearer token still decides *whether* a request is
    /// served; this says who it is attributed to.
    identity: Option<(Arc<Mutex<users::UserRegistry>>, String)>,
    /// Shared feedback store, when wired — backs `POST /feedback` (E33 S33.4).
    ///
    /// The E24 store has existed since the operational wave with only a CLI in
    /// front of it, so quality signal could only be recorded by someone who
    /// had already left the conversation.
    feedback: Option<(Arc<Mutex<feedback::FeedbackStore>>, String)>,
    /// Shared conversation history, when wired: `(store, session_id)`.
    ///
    /// The same store the agent's conversation memory writes to (E33 S33.1),
    /// so `GET /conversation` serves exactly what the model was given rather
    /// than a parallel transcript that could drift from it.
    conversation: Option<(Arc<Mutex<sessions::SessionStore>>, String)>,
    /// Counter behind server-minted operator-message ids (E33 S33.2).
    ///
    /// Seeded from the process start time so ids stay distinct across restarts:
    /// the audit log outlives the process, and a re-read must not conflate a
    /// message from this run with one from the last.
    message_seq: std::sync::atomic::AtomicU64,
    /// Identifies this process in the durable feedback store (E33 S33.4).
    ///
    /// A rating is filed against the scheduler's task id, but those restart at
    /// `1 << 63` with every `LifecycleManager`, so the *n*-th reply of one run
    /// and the *n*-th of the next would share an invocation id and the E24
    /// report would fold two unrelated answers into one hint.  Qualifying the
    /// id with the run keeps them apart.
    run_id: String,
}

/// Wall-clock nanoseconds at process start, the entropy behind both per-run
/// identifiers above.  Clock failure degrades to `0`, which costs uniqueness
/// but never the request.
fn start_nanos() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// A label for this server instance: the clock separates processes, the counter
/// separates servers within one (and covers a clock too coarse to tell two
/// constructions apart).
fn mint_run_id() -> String {
    static INSTANCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = INSTANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("run{:016x}{n:x}", start_nanos())
}

/// Enforce the exposure policy for a console bind: a network-reachable address
/// must carry a token, or the bind is refused.
///
/// Loopback addresses (`127.0.0.0/8`, `::1`) are exempt — the container maps
/// only the loopback port to the host. Everything else — including the
/// unspecified wildcards `0.0.0.0` / `::` that bind *all* interfaces — requires
/// a non-empty `ANIMA_CONSOLE_TOKEN`, so an unauthenticated console can never be
/// exposed beyond this host by accident.
fn check_bind_policy(addr: std::net::SocketAddr, has_token: bool) -> std::io::Result<()> {
    if addr.ip().is_loopback() || has_token {
        return Ok(());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        format!(
            "refusing to bind the operator console to non-loopback address {addr} without \
             authentication; set a non-empty ANIMA_CONSOLE_TOKEN, or bind 127.0.0.1 for \
             loopback-only access"
        ),
    ))
}

/// Map a protocol priority onto the `senses` priority enum.
/// How often an SSE connection emits a [`OperatorEvent::Heartbeat`], regardless
/// of how much other traffic is flowing (E33 S33.0).  Also the proxy keep-alive.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

/// How long a subscriber blocks waiting for the next event before re-checking
/// the heartbeat cadence.  Short enough that the beat lands on time, long
/// enough that an idle console costs one wakeup a second.
const HEARTBEAT_POLL: Duration = Duration::from_secs(1);

/// Longest free-text correction stored with a feedback rating.
const MAX_FEEDBACK_COMMENT: usize = 2000;

/// Turns returned by `GET /conversation` when no `limit` is given.
const DEFAULT_CONVERSATION_LIMIT: usize = 100;

/// Hard cap on `GET /conversation?limit=`, so one request cannot serialise a
/// whole long-lived session into memory.
const MAX_CONVERSATION_LIMIT: usize = 1000;

/// Longest guidance echo carried in the `OperatorGuidance` audit event.
///
/// The echo is what the conversation view renders for the operator's own
/// message, so the old 200-byte cut silently truncated any message longer than
/// a short paragraph.  The bound still exists — the feed must not carry a
/// 64 KiB line — but it is now far above normal use (E33 S33.0).
const GUIDANCE_ECHO_LIMIT: usize = 4000;

/// Longest client-supplied correlation id accepted on `POST /guidance`.
const MAX_MESSAGE_ID_LEN: usize = 64;

/// Whether a client-supplied correlation id is acceptable (E33 S33.2).
///
/// The id is written to the durable audit log and rendered by every attached
/// console, so it is untrusted input on the operator channel (threat model §5)
/// and is restricted to an unambiguous, quoting-free alphabet rather than
/// being escaped at each of the places it later appears.
fn valid_message_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_MESSAGE_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

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
            digest_path: None,
            digest_agent_id: "anima".to_string(),
            digest_cache: Mutex::new(None),
            approval_queue: None,
            skill_registry: None,
            adapter_library: None,
            identity: None,
            feedback: None,
            conversation: None,
            // Nanoseconds, not seconds: two servers started in the same
            // second would otherwise mint the identical `op-…` sequence and
            // the audit log could not tell their messages apart.
            message_seq: std::sync::atomic::AtomicU64::new(start_nanos()),
            run_id: mint_run_id(),
        }
    }

    /// Wire the `GET /digest` endpoint to read from `path` and fold over the
    /// entries using `agent_id`. Without this the endpoint returns 503.
    pub fn with_digest(
        mut self,
        path: impl Into<std::path::PathBuf>,
        agent_id: impl Into<String>,
    ) -> Self {
        self.digest_path = Some(path.into());
        self.digest_agent_id = agent_id.into();
        self
    }

    /// Wire in a shared approval queue. When set, `GET /approval-queue` returns
    /// all proposals as JSON, and `POST /approval-queue/{id}/approve` /
    /// `POST /approval-queue/{id}/reject` allow operator decisions (E15 S15.2).
    pub fn with_approval_queue(
        mut self,
        queue: Arc<Mutex<lifecycle::approval::ApprovalQueue>>,
    ) -> Self {
        self.approval_queue = Some(queue);
        self
    }

    /// Wire in a shared skill registry. When set, `GET /skills` returns all
    /// skill entries as JSON (E11 S11.1 dashboard surface).
    pub fn with_skill_registry(mut self, registry: Arc<Mutex<skills::SkillRegistry>>) -> Self {
        self.skill_registry = Some(registry);
        self
    }

    /// Wire in a shared adapter library. When set, `GET /adapters` returns all
    /// registered adapters as JSON (E8 adapter-library dashboard surface).
    /// Wire in the operator identity so `GET /whoami` is active (E33 S33.5).
    pub fn with_identity(
        mut self,
        registry: Arc<Mutex<users::UserRegistry>>,
        user_id: impl Into<String>,
    ) -> Self {
        self.identity = Some((registry, user_id.into()));
        self
    }

    /// Wire in the shared feedback store so `POST /feedback` is active
    /// (E33 S33.4).  `user_id` is the operator the ratings are attributed to.
    pub fn with_feedback(
        mut self,
        store: Arc<Mutex<feedback::FeedbackStore>>,
        user_id: impl Into<String>,
    ) -> Self {
        self.feedback = Some((store, user_id.into()));
        self
    }

    /// Wire in the shared conversation store so `GET /conversation` is active
    /// (E33 S33.1).  Without it the route answers 404 and a client falls back
    /// to whatever the event stream replays.
    pub fn with_conversation(
        mut self,
        store: Arc<Mutex<sessions::SessionStore>>,
        session_id: impl Into<String>,
    ) -> Self {
        self.conversation = Some((store, session_id.into()));
        self
    }

    pub fn with_adapter_library(
        mut self,
        library: Arc<Mutex<anima_finetune::AdapterLibrary>>,
    ) -> Self {
        self.adapter_library = Some(library);
        self
    }

    /// Bind the listener (so the caller learns the resolved local address) and
    /// return it without yet serving. Useful for tests that need the OS-chosen
    /// port from `127.0.0.1:0`.
    ///
    /// Refuses, with [`std::io::ErrorKind::PermissionDenied`], to bind a
    /// network-reachable address without a token — see [`check_bind_policy`].
    pub fn bind(&self) -> std::io::Result<TcpListener> {
        let addr =
            self.config.addr.to_socket_addrs()?.next().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "no address")
            })?;
        let has_token = self.config.token.as_deref().is_some_and(|t| !t.is_empty());
        check_bind_policy(addr, has_token)?;
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
                                message_id: None,
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

        // Dynamic path routing: approval-queue approve/reject actions.
        // These must be checked before the static match because the paths
        // contain a proposal ID segment that is only known at runtime.
        if method == "POST" {
            if let Some(rest) = path.strip_prefix("/approval-queue/") {
                if let Some(raw_id) = rest.strip_suffix("/approve") {
                    let id = percent_decode(raw_id);
                    return self.serve_approval_action(
                        &id,
                        true,
                        content_length,
                        &mut reader,
                        &mut out,
                    );
                }
                if let Some(raw_id) = rest.strip_suffix("/reject") {
                    let id = percent_decode(raw_id);
                    return self.serve_approval_action(
                        &id,
                        false,
                        content_length,
                        &mut reader,
                        &mut out,
                    );
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
            // E33 S33.1 — durable conversation history, so a reloaded page
            // paints the real transcript instead of the replay ring's tail.
            // E33 S33.5 — who this console is talking as.
            ("GET", "/whoami") => self.serve_whoami(&mut out),
            ("GET", "/conversation") => self.serve_conversation(&query, &mut out),
            // E33 S33.4 — rate a reply from the conversation view.
            ("POST", "/feedback") => self.serve_feedback(&mut reader, content_length, &mut out),
            // S15.1 — "While you were away" activity digest
            ("GET", "/digest") => self.serve_digest(&mut out),
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
            // E15 S15.2 — Approval queue: list all proposals.
            // Returns 404 when the approval queue has not been wired in.
            ("GET", "/approval-queue") => {
                let Some(queue) = &self.approval_queue else {
                    return write_json(
                        &mut out,
                        404,
                        "Not Found",
                        br#"{"error":"approval queue not available"}"#,
                    );
                };
                let proposals = match queue.lock() {
                    Ok(q) => q.all().into_iter().cloned().collect::<Vec<_>>(),
                    Err(_) => {
                        return write_json(
                            &mut out,
                            500,
                            "Internal Server Error",
                            br#"{"error":"queue lock poisoned"}"#,
                        );
                    }
                };
                match serde_json::to_vec(&proposals) {
                    Ok(body) => write_json(&mut out, 200, "OK", &body),
                    Err(_) => write_json(
                        &mut out,
                        500,
                        "Internal Server Error",
                        br#"{"error":"serialisation failed"}"#,
                    ),
                }
            }
            // E11 S11.1 — Skills registry: list all skill entries.
            // Returns 404 when the skill registry has not been wired in.
            ("GET", "/skills") => {
                let Some(registry) = &self.skill_registry else {
                    return write_json(
                        &mut out,
                        404,
                        "Not Found",
                        br#"{"error":"skill registry not available"}"#,
                    );
                };
                let entries = match registry.lock() {
                    Ok(r) => r.list_all().into_iter().cloned().collect::<Vec<_>>(),
                    Err(_) => {
                        return write_json(
                            &mut out,
                            500,
                            "Internal Server Error",
                            br#"{"error":"registry lock poisoned"}"#,
                        );
                    }
                };
                match serde_json::to_vec(&entries) {
                    Ok(body) => write_json(&mut out, 200, "OK", &body),
                    Err(_) => write_json(
                        &mut out,
                        500,
                        "Internal Server Error",
                        br#"{"error":"serialisation failed"}"#,
                    ),
                }
            }
            // E8 — Adapter library: list all registered adapters.
            // Returns 404 when the adapter library has not been wired in.
            ("GET", "/adapters") => {
                let Some(library) = &self.adapter_library else {
                    return write_json(
                        &mut out,
                        404,
                        "Not Found",
                        br#"{"error":"adapter library not available"}"#,
                    );
                };
                let adapters = match library.lock() {
                    Ok(l) => l.list().into_iter().cloned().collect::<Vec<_>>(),
                    Err(_) => {
                        return write_json(
                            &mut out,
                            500,
                            "Internal Server Error",
                            br#"{"error":"library lock poisoned"}"#,
                        );
                    }
                };
                match serde_json::to_vec(&adapters) {
                    Ok(body) => write_json(&mut out, 200, "OK", &body),
                    Err(_) => write_json(
                        &mut out,
                        500,
                        "Internal Server Error",
                        br#"{"error":"serialisation failed"}"#,
                    ),
                }
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

        // E33 S33.0: the heartbeat used to ride the receive timeout alone, so a
        // console attached to a live agent never saw one — the 1 Hz vitals meant
        // the channel was never idle for the full timeout, and the dashboard's
        // uptime chip stayed blank forever.  Emit on a wall-clock cadence
        // instead, independent of how busy the stream is.
        let mut last_beat = std::time::Instant::now();
        loop {
            if last_beat.elapsed() >= HEARTBEAT_INTERVAL {
                last_beat = std::time::Instant::now();
                // No `id:` line — heartbeats are synthesised per-connection and
                // must not advance the client's replay cursor.
                let beat = OperatorEvent::Heartbeat {
                    uptime_secs: self.hub.uptime_secs(),
                };
                if write_sse(&mut out, None, &beat).is_err() {
                    break;
                }
            }
            match sub.rx.recv_timeout(HEARTBEAT_POLL) {
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
                // Nothing published this tick: fall through to the cadence
                // check at the top of the loop, which emits the keep-alive.
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
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

        // E33 S33.2: every accepted line gets a correlation id — the client's
        // when it supplied a usable one, otherwise one minted here — so the
        // reply can be tied back to the message that asked for it.
        let message_id = match input.message_id.as_deref() {
            Some(id) if valid_message_id(id) => id.to_string(),
            Some(_) => {
                return write_json(
                    out,
                    400,
                    "Bad Request",
                    br#"{"ok":false,"error":"message_id must be 1-64 chars of [A-Za-z0-9_-]"}"#,
                );
            }
            None => format!(
                "op-{:x}",
                self.message_seq
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ),
        };

        // Same alphabet rule as the correlation id: it is echoed to every
        // console and must not need escaping at each point of use.
        let reply_to = match input.reply_to.as_deref() {
            Some(id) if valid_message_id(id) => Some(id.to_string()),
            Some(_) => {
                return write_json(
                    out,
                    400,
                    "Bad Request",
                    br#"{"ok":false,"error":"reply_to must be 1-64 chars of [A-Za-z0-9_-]"}"#,
                );
            }
            None => None,
        };

        // E6.6: when `force` is set, route through `packetize_text_forced` so
        // vita's somatic loop can record an audited GateOverride::OperatorForced
        // entry.  Policy bounds still apply — the operator is a potentially-
        // compromised channel (threat model §5 in 11-operator-interface.md).
        let result = if let Some(reason) = input.force.as_deref() {
            self.bridge.packetize_text_forced_tagged(
                input.text.clone(),
                reason,
                Some(message_id.clone()),
            )
        } else {
            self.bridge.packetize_text_tagged(
                input.text.clone(),
                to_sensory_priority(input.priority),
                Some(message_id.clone()),
            )
        };

        match result {
            Ok(()) => {
                // Echo the accepted guidance into the event feed so every
                // connected operator sees what was injected (and by implication,
                // that it is now subject to the gate, not executed directly).
                //
                // E33 S33.2: the typed `Accepted` event replaces the free-text
                // `OperatorGuidance` audit echo.  It carries the correlation id
                // and the untruncated text, so a console renders the operator's
                // own message without reparsing a "[Priority] …" string, and
                // can then follow that message through gate, task and reply.
                let forced = input.force.is_some();
                self.hub.publish(OperatorEvent::Accepted {
                    message_id: message_id.clone(),
                    priority: if forced {
                        Priority::Critical
                    } else {
                        input.priority
                    },
                    forced,
                    force_reason: input.force.clone(),
                    reply_to,
                    text: truncate(&input.text, GUIDANCE_ECHO_LIMIT),
                });
                let body = format!(r#"{{"ok":true,"message_id":{}}}"#, json_string(&message_id));
                write_json(out, 202, "Accepted", body.as_bytes())
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

    /// Serve the operator's identity as JSON (E33 S33.5).
    ///
    /// `authenticated` reports whether a bearer token gates this server at
    /// all — a request that reached this handler has already satisfied it.  It
    /// is deliberately not a claim that the *person* was authenticated: a
    /// shared token identifies a console, not a human, which is why the trust
    /// tier comes from the registry rather than from the request.
    fn serve_whoami(&self, out: &mut TcpStream) -> std::io::Result<()> {
        let Some((registry, user_id)) = &self.identity else {
            return write_json(
                out,
                404,
                "Not Found",
                br#"{"error":"identity not available"}"#,
            );
        };
        let (display_name, trust_tier, known) = match registry.lock() {
            Ok(reg) => match reg.get(user_id) {
                Some(record) => (
                    record.profile.display_name.clone(),
                    record.profile.trust_tier.as_str().to_string(),
                    true,
                ),
                None => (user_id.clone(), "unknown".to_string(), false),
            },
            Err(_) => {
                return write_json(
                    out,
                    500,
                    "Internal Server Error",
                    br#"{"error":"user registry lock poisoned"}"#,
                );
            }
        };
        let body = format!(
            r#"{{"user_id":{},"display_name":{},"trust_tier":{},"registered":{},"token_required":{}}}"#,
            json_string(user_id),
            json_string(&display_name),
            json_string(&trust_tier),
            known,
            self.config.token.is_some(),
        );
        write_json(out, 200, "OK", body.as_bytes())
    }

    /// Record operator feedback on one reply (E33 S33.4).
    ///
    /// Body: `{"task_id": "<id>", "rating": "up"|"down", "comment": "…"}`.
    /// `task_id` identifies the invocation being rated and is echoed into the
    /// durable record, so a correction can be traced back to the exact answer.
    fn serve_feedback(
        &self,
        reader: &mut BufReader<TcpStream>,
        content_length: usize,
        out: &mut TcpStream,
    ) -> std::io::Result<()> {
        let Some((store, user_id)) = &self.feedback else {
            return write_json(
                out,
                404,
                "Not Found",
                br#"{"ok":false,"error":"feedback not available"}"#,
            );
        };

        const MAX_BODY: usize = 8 * 1024;
        if content_length > MAX_BODY {
            return write_json(
                out,
                413,
                "Payload Too Large",
                br#"{"ok":false,"error":"request body exceeds 8 KiB limit"}"#,
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
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) else {
            return write_json(
                out,
                400,
                "Bad Request",
                br#"{"ok":false,"error":"request body is not valid JSON"}"#,
            );
        };

        let Some(task_id) = value.get("task_id").and_then(|v| v.as_str()) else {
            return write_json(
                out,
                400,
                "Bad Request",
                br#"{"ok":false,"error":"task_id is required"}"#,
            );
        };
        // Same alphabet rule as the correlation ids: this reaches the durable
        // store and every surface that renders it.
        if !valid_message_id(task_id) {
            return write_json(
                out,
                400,
                "Bad Request",
                br#"{"ok":false,"error":"task_id must be 1-64 chars of [A-Za-z0-9_-]"}"#,
            );
        }

        let rating = match value.get("rating").and_then(|v| v.as_str()) {
            Some("up") => feedback::FeedbackRating::ThumbsUp,
            Some("down") => feedback::FeedbackRating::ThumbsDown,
            _ => {
                return write_json(
                    out,
                    400,
                    "Bad Request",
                    br#"{"ok":false,"error":"rating must be 'up' or 'down'"}"#,
                );
            }
        };

        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        // Qualified by the run: the scheduler's task ids restart at `1 << 63`
        // with every process, so the bare id would make the third reply of one
        // run and the third of the next look like the same answer rated twice
        // (`feedback::report` groups by exactly this field).
        let invocation_id = format!("{}-{task_id}", self.run_id);
        let mut record = feedback::FeedbackRecord::new(user_id, invocation_id, rating, now_ns);
        if let Some(comment) = value
            .get("comment")
            .and_then(|v| v.as_str())
            .filter(|c| !c.trim().is_empty())
        {
            record = record.with_correction(truncate(comment, MAX_FEEDBACK_COMMENT));
        }

        let mut guard = match store.lock() {
            Ok(g) => g,
            Err(_) => {
                return write_json(
                    out,
                    500,
                    "Internal Server Error",
                    br#"{"ok":false,"error":"feedback store lock poisoned"}"#,
                );
            }
        };
        match guard.record(record).and_then(|()| {
            // `record` only mutates memory; without this the rating is lost on
            // restart and invisible to `anima feedback`, while the caller has
            // already been told it was accepted.
            guard.flush()
        }) {
            Ok(()) => write_json(out, 202, "Accepted", br#"{"ok":true}"#),
            Err(e) => {
                let body = format!(r#"{{"ok":false,"error":{}}}"#, json_string(&e.to_string()));
                write_json(out, 500, "Internal Server Error", body.as_bytes())
            }
        }
    }

    /// Serve the durable conversation history as JSON (E33 S33.1).
    ///
    /// `?limit=N` returns the newest `N` turns (default
    /// [`DEFAULT_CONVERSATION_LIMIT`], capped at [`MAX_CONVERSATION_LIMIT`] so
    /// one request cannot serialise an entire long-lived session).  Turns come
    /// back oldest-first, the order a transcript is read in.
    ///
    /// `?before=I` pages backwards: the newest `limit` turns whose `index` is
    /// below `I`, exclusive.  A client walks the whole session by passing the
    /// `index` of the oldest turn it holds, and knows it has reached the start
    /// when that index is 0 or fewer than `limit` turns come back.  Without it
    /// the durable history the agent keeps is only readable up to one page
    /// deep, which is not much of a record.
    fn serve_conversation(&self, query: &str, out: &mut TcpStream) -> std::io::Result<()> {
        let Some((store, session_id)) = &self.conversation else {
            return write_json(
                out,
                404,
                "Not Found",
                br#"{"error":"conversation history not available"}"#,
            );
        };
        let limit = query
            .split('&')
            .find_map(|kv| kv.strip_prefix("limit="))
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(DEFAULT_CONVERSATION_LIMIT)
            .clamp(1, MAX_CONVERSATION_LIMIT);
        let before = query
            .split('&')
            .find_map(|kv| kv.strip_prefix("before="))
            .and_then(|v| v.parse::<usize>().ok());

        let turns = {
            let guard = match store.lock() {
                Ok(g) => g,
                Err(_) => {
                    return write_json(
                        out,
                        500,
                        "Internal Server Error",
                        br#"{"error":"session store lock poisoned"}"#,
                    );
                }
            };
            match guard.get(session_id) {
                // Newest `limit` turns below the cursor, still in chronological
                // order.  Turns are stored in index order, so the cursor is the
                // first position whose index reaches it — a scan rather than a
                // subscript, because a session's turns need not start at 0 once
                // an older run has been trimmed.
                Some(session) => {
                    let end = match before {
                        Some(cursor) => session
                            .turns
                            .iter()
                            .position(|t| t.index >= cursor)
                            .unwrap_or(session.turns.len()),
                        None => session.turns.len(),
                    };
                    let skip = end.saturating_sub(limit);
                    session.turns[skip..end].to_vec()
                }
                None => Vec::new(),
            }
        };

        match serde_json::to_vec(&turns) {
            Ok(body) => write_json(out, 200, "OK", &body),
            Err(_) => write_json(
                out,
                500,
                "Internal Server Error",
                br#"{"error":"serialisation failed"}"#,
            ),
        }
    }

    /// Serve the S15.1 "while you were away" activity digest as JSON.
    ///
    /// Reads the audit JSONL file at `digest_path`, folds the entries with
    /// [`lifecycle::digest::generate_digest`], and returns the serialised
    /// [`lifecycle::digest::ActivityDigest`].  Returns 503 if the server was not
    /// wired with [`ConsoleServer::with_digest`]; returns an empty digest (no
    /// error) if the file does not exist yet.
    fn serve_digest(&self, out: &mut TcpStream) -> std::io::Result<()> {
        let Some(ref path) = self.digest_path else {
            return write_json(
                out,
                503,
                "Service Unavailable",
                br#"{"error":"digest not configured"}"#,
            );
        };
        // Check mtime; serve the cached JSON if the file has not changed since
        // the last request, avoiding a full audit-log read on every call.
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        {
            let cache = self.digest_cache.lock().unwrap_or_else(|e| e.into_inner());
            if let (Some(mt), Some((cached_mt, cached_json))) = (mtime, cache.as_ref()) {
                if mt == *cached_mt {
                    return write_json(out, 200, "OK", cached_json.as_bytes());
                }
            }
        }
        let entries = read_audit_entries(path);
        let digest = lifecycle::digest::generate_digest(&self.digest_agent_id, &entries);
        match serde_json::to_string(&digest) {
            Ok(json) => {
                if let Some(mt) = mtime {
                    *self.digest_cache.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some((mt, json.clone()));
                }
                write_json(out, 200, "OK", json.as_bytes())
            }
            Err(_) => write_json(
                out,
                500,
                "Internal Server Error",
                br#"{"error":"serialization failed"}"#,
            ),
        }
    }

    /// Handle `POST /approval-queue/{id}/approve` and
    /// `POST /approval-queue/{id}/reject`.  `approve` distinguishes the two.
    ///
    /// Body (optional JSON): `{"reason": "..."}`.  An empty body or missing
    /// `reason` key defaults to an empty reason string.
    fn serve_approval_action(
        &self,
        proposal_id: &str,
        approve: bool,
        content_length: usize,
        reader: &mut BufReader<TcpStream>,
        out: &mut TcpStream,
    ) -> std::io::Result<()> {
        let Some(queue) = &self.approval_queue else {
            return write_json(
                out,
                404,
                "Not Found",
                br#"{"ok":false,"error":"approval queue not available"}"#,
            );
        };

        const MAX_BODY: usize = 4 * 1024;
        if content_length > MAX_BODY {
            return write_json(
                out,
                413,
                "Payload Too Large",
                br#"{"ok":false,"error":"request body exceeds 4 KiB limit"}"#,
            );
        }

        let reason = if content_length > 0 {
            let mut buf = vec![0u8; content_length];
            reader.read_exact(&mut buf)?;
            let Ok(text) = String::from_utf8(buf) else {
                return write_json(
                    out,
                    400,
                    "Bad Request",
                    br#"{"ok":false,"error":"request body is not valid UTF-8"}"#,
                );
            };
            let value: serde_json::Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(_) => {
                    return write_json(
                        out,
                        400,
                        "Bad Request",
                        br#"{"ok":false,"error":"request body is not valid JSON"}"#,
                    );
                }
            };
            value
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("")
                .to_string()
        } else {
            String::new()
        };

        // Capture WeightUpdate (adapter_id, weights_digest) inside the same
        // lock acquisition as approve/reject to avoid TOCTOU races. The adapter
        // library is then updated *outside* the queue lock to prevent
        // simultaneous lock ordering issues.
        let (result, weight_op, skill_id) = {
            let mut q = match queue.lock() {
                Ok(q) => q,
                Err(_) => {
                    return write_json(
                        out,
                        500,
                        "Internal Server Error",
                        br#"{"ok":false,"error":"queue lock poisoned"}"#,
                    );
                }
            };
            // For WeightUpdate proposals, record the (adapter_id, digest) pair
            // so we can forward the operator decision to the adapter library.
            // ID format is "{adapter_id}@{weights_digest}" (adapter_bridge.rs).
            let weight_op = q.get(proposal_id).and_then(|p| {
                if let lifecycle::approval::ProposalKind::WeightUpdate {
                    ref adapter_hash, ..
                } = p.kind
                {
                    let adapter_id = match p.id.rfind('@') {
                        Some(i) => p.id[..i].to_string(),
                        None => p.id.clone(),
                    };
                    Some((adapter_id, adapter_hash.clone()))
                } else {
                    None
                }
            });
            // E33 S33.0: a skill proposal's queue id *is* its registry skill id
            // (`lifecycle::skill_bridge` sets it that way), so an approval can be
            // routed to the registry without a second mapping to keep in step.
            let skill_id = q.get(proposal_id).and_then(|p| {
                matches!(p.kind, lifecycle::approval::ProposalKind::NewSkill { .. })
                    .then(|| p.id.clone())
            });
            let result = if approve {
                q.approve(proposal_id, &reason)
            } else {
                q.reject(proposal_id, &reason)
            };
            (result, weight_op, skill_id)
        };

        match result {
            Ok(()) => {
                // Synchronise WeightUpdate decisions with the adapter library so
                // that mount_gated sees operator sign-off (approve) or its
                // revocation (reject), completing the human half of the two-stage
                // adoption gate (S8.4.8 / AdapterLibrary::mount_gated).
                if let (Some((adapter_id, weights_digest)), Some(library)) =
                    (weight_op, &self.adapter_library)
                {
                    if let Ok(mut lib) = library.lock() {
                        if approve {
                            lib.record_operator_approval(&adapter_id, &weights_digest);
                        } else {
                            lib.revoke_operator_approval(&adapter_id, &weights_digest);
                        }
                    }
                }
                // The same for a skill: approving it in the queue alone left the
                // registry entry `Proposed`, so the skill the operator had just
                // said yes to still could not be selected.  Promotion is what
                // makes the decision mean something.  A rejection deliberately
                // leaves the registry alone — the skill stays `Proposed` rather
                // than being rolled back, matching `SkillApprovalBridge::reject`.
                if let (true, Some(skill_id), Some(registry)) =
                    (approve, skill_id, &self.skill_registry)
                {
                    let outcome = match registry.lock() {
                        Ok(mut r) => r.promote(&skill_id).map_err(|e| e.to_string()),
                        Err(_) => Err("skill registry lock poisoned".to_string()),
                    };
                    if let Err(e) = outcome {
                        // The queue already records the approval, so saying "ok"
                        // here would claim an activation that did not happen.
                        let body = format!(
                            r#"{{"ok":false,"approved":true,"error":{}}}"#,
                            json_string(&format!(
                                "proposal approved, but the skill could not be activated: {e}"
                            ))
                        );
                        return write_json(out, 500, "Internal Server Error", body.as_bytes());
                    }
                }
                write_json(out, 200, "OK", br#"{"ok":true}"#)
            }
            Err(e) => {
                let body = format!(r#"{{"ok":false,"error":{}}}"#, json_string(&e));
                write_json(out, 422, "Unprocessable Entity", body.as_bytes())
            }
        }
    }
}

/// Read and parse all complete lines from a vita audit JSONL file.
///
/// Missing or unreadable files return an empty vec; malformed lines are
/// silently skipped so a partially-written tail entry never blocks the digest.
fn read_audit_entries(path: &std::path::Path) -> Vec<vita::AuditEntry> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(|l| l.ok())
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect()
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

/// Decode a percent-encoded URL path segment (`%2F` → `/`, `%40` → `@`, etc.).
///
/// Only `%XX` sequences with valid hex digits are decoded; everything else is
/// passed through unchanged. Decoded bytes are collected first and then
/// interpreted as UTF-8, so multi-byte sequences such as `%C3%A9` (é) are
/// reconstructed correctly rather than being pushed as individual Latin-1
/// scalars. Invalid UTF-8 is replaced with U+FFFD. Used to round-trip
/// `encodeURIComponent`-encoded proposal IDs from the dashboard.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut decoded: Vec<u8> = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let (Some(&hi), Some(&lo)) = (bytes.get(i + 1), bytes.get(i + 2)) {
                if let (Some(h), Some(l)) = (hex_nibble(hi), hex_nibble(lo)) {
                    decoded.push(h << 4 | l);
                    i += 3;
                    continue;
                }
            }
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(decoded)
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
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
         Access-Control-Allow-Methods: GET, OPTIONS\r\n\
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

    // ── Bind exposure policy ─────────────────────────────────────────────────

    fn server_with(addr: &str, token: Option<&str>) -> ConsoleServer {
        ConsoleServer::new(
            Arc::new(ConsoleHub::new()),
            SensoryBridge::new(HumanGuidance::new("t")),
            ServerConfig {
                addr: addr.into(),
                token: token.map(str::to_string),
            },
        )
    }

    #[test]
    fn bind_policy_allows_loopback_without_token() {
        assert!(check_bind_policy("127.0.0.1:8088".parse().unwrap(), false).is_ok());
        assert!(check_bind_policy("[::1]:8088".parse().unwrap(), false).is_ok());
    }

    #[test]
    fn bind_policy_rejects_non_loopback_without_token() {
        let err = check_bind_policy("203.0.113.5:8088".parse().unwrap(), false).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn bind_policy_rejects_wildcard_without_token() {
        // 0.0.0.0 / :: bind all interfaces → must require a token.
        assert!(check_bind_policy("0.0.0.0:8088".parse().unwrap(), false).is_err());
        assert!(check_bind_policy("[::]:8088".parse().unwrap(), false).is_err());
    }

    #[test]
    fn bind_policy_allows_non_loopback_with_token() {
        assert!(check_bind_policy("203.0.113.5:8088".parse().unwrap(), true).is_ok());
    }

    #[test]
    fn bind_refuses_wildcard_without_token_before_opening_socket() {
        let err = server_with("0.0.0.0:0", None).bind().unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        // An empty token is treated as no token.
        assert!(
            server_with("0.0.0.0:0", Some("")).bind().is_err(),
            "empty ANIMA_CONSOLE_TOKEN must not satisfy the exposure gate"
        );
    }

    #[test]
    fn bind_allows_loopback_without_token() {
        assert!(server_with("127.0.0.1:0", None).bind().is_ok());
    }

    // ── E33 S33.0 ─────────────────────────────────────────────────────────

    #[test]
    fn long_guidance_is_echoed_past_the_old_two_hundred_byte_cut() {
        // The echo is what the conversation view renders as the operator's own
        // message, so a 200-byte cut silently truncated anything longer than a
        // short paragraph.
        let (addr, hub, _bridge) = start();
        let sub = hub.subscribe();
        let text = "y".repeat(1500);
        let body = format!(r#"{{"text":"{text}","priority":"Normal"}}"#);
        let resp = http_request(
            addr,
            &format!(
                "POST /guidance HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            ),
        );
        assert!(resp.contains("202 Accepted"), "resp: {resp}");

        let echo = sub
            .rx
            .recv_timeout(Duration::from_secs(2))
            .expect("guidance echo published");
        match echo {
            (_, OperatorEvent::Accepted { text: echoed, .. }) => assert!(
                echoed.contains(&"y".repeat(1500)),
                "echo truncated at {} chars",
                echoed.len()
            ),
            other => panic!("expected an Accepted echo, got {other:?}"),
        }
    }

    #[test]
    fn guidance_echo_is_still_bounded_so_the_feed_cannot_carry_a_64_kib_line() {
        let (addr, hub, _bridge) = start();
        let sub = hub.subscribe();
        let text = "z".repeat(GUIDANCE_ECHO_LIMIT + 500);
        let body = format!(r#"{{"text":"{text}","priority":"Normal"}}"#);
        let _ = http_request(
            addr,
            &format!(
                "POST /guidance HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            ),
        );
        match sub.rx.recv_timeout(Duration::from_secs(2)).unwrap() {
            (_, OperatorEvent::Accepted { text: echoed, .. }) => assert!(
                echoed.len() < GUIDANCE_ECHO_LIMIT + 200,
                "echo unbounded at {} chars",
                echoed.len()
            ),
            other => panic!("expected an Accepted echo, got {other:?}"),
        }
    }

    // ── E33 S33.5 — operator identity ─────────────────────────────────────

    fn start_with_identity(register: bool, token: Option<&str>) -> std::net::SocketAddr {
        let mut registry = users::UserRegistry::in_memory();
        if register {
            let mut profile =
                users::UserProfile::new("user:operator", "Dana", "console", 1_000_000);
            profile.trust_tier = users::TrustTier::Trusted;
            registry.register(profile).unwrap();
        }
        let hub = Arc::new(ConsoleHub::new());
        let bridge = SensoryBridge::new(HumanGuidance::new("test"));
        ConsoleServer::new(
            hub,
            bridge,
            ServerConfig {
                addr: "127.0.0.1:0".into(),
                token: token.map(str::to_string),
            },
        )
        .with_identity(Arc::new(Mutex::new(registry)), "user:operator")
        .spawn()
        .expect("spawn")
        .0
    }

    fn get_whoami(addr: std::net::SocketAddr, auth: &str) -> String {
        http_request(
            addr,
            &format!("GET /whoami HTTP/1.1\r\nHost: x\r\n{auth}Connection: close\r\n\r\n"),
        )
    }

    #[test]
    fn whoami_returns_404_when_not_wired() {
        let (addr, _hub, _bridge) = start();
        assert!(get_whoami(addr, "").contains("404 Not Found"));
    }

    #[test]
    fn whoami_names_the_registered_operator_and_their_trust_tier() {
        let addr = start_with_identity(true, None);
        let resp = get_whoami(addr, "");
        assert!(resp.contains("200 OK"), "resp: {resp}");
        assert!(resp.contains(r#""display_name":"Dana""#), "resp: {resp}");
        assert!(resp.contains(r#""trust_tier":"trusted""#), "resp: {resp}");
        assert!(resp.contains(r#""registered":true"#), "resp: {resp}");
        // Loopback with no token configured.
        assert!(resp.contains(r#""token_required":false"#), "resp: {resp}");
    }

    #[test]
    fn whoami_reports_an_unregistered_operator_without_inventing_trust() {
        let addr = start_with_identity(false, None);
        let resp = get_whoami(addr, "");
        assert!(resp.contains(r#""registered":false"#), "resp: {resp}");
        assert!(
            resp.contains(r#""trust_tier":"unknown""#),
            "an unregistered operator must not be granted a tier: {resp}"
        );
    }

    #[test]
    fn whoami_is_behind_the_bearer_token_like_every_other_route() {
        let addr = start_with_identity(true, Some("sekret"));
        assert!(get_whoami(addr, "").contains("401 Unauthorized"));
        let ok = get_whoami(addr, "Authorization: Bearer sekret\r\n");
        assert!(ok.contains("200 OK"), "resp: {ok}");
        assert!(ok.contains(r#""token_required":true"#), "resp: {ok}");
    }

    #[test]
    fn whoami_body_is_valid_json() {
        let addr = start_with_identity(true, None);
        let resp = get_whoami(addr, "");
        let payload = resp.split("\r\n\r\n").nth(1).unwrap_or("").trim();
        assert!(
            serde_json::from_str::<serde_json::Value>(payload).is_ok(),
            "not valid JSON: {payload}"
        );
    }

    // ── E33 S33.4 — feedback ──────────────────────────────────────────────

    fn start_with_feedback() -> (std::net::SocketAddr, Arc<Mutex<feedback::FeedbackStore>>) {
        let store = Arc::new(Mutex::new(feedback::FeedbackStore::in_memory()));
        let hub = Arc::new(ConsoleHub::new());
        let bridge = SensoryBridge::new(HumanGuidance::new("test"));
        let addr = ConsoleServer::new(
            hub,
            bridge,
            ServerConfig {
                addr: "127.0.0.1:0".into(),
                token: None,
            },
        )
        .with_feedback(Arc::clone(&store), "user:operator")
        .spawn()
        .expect("spawn")
        .0;
        (addr, store)
    }

    fn post_feedback(addr: std::net::SocketAddr, body: &str) -> String {
        http_request(
            addr,
            &format!(
                "POST /feedback HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            ),
        )
    }

    #[test]
    fn feedback_returns_404_when_not_wired() {
        let (addr, _hub, _bridge) = start();
        let resp = post_feedback(addr, r#"{"task_id":"7","rating":"up"}"#);
        assert!(resp.contains("404 Not Found"), "resp: {resp}");
    }

    #[test]
    fn a_rating_reaches_the_durable_store() {
        let (addr, store) = start_with_feedback();
        let resp = post_feedback(addr, r#"{"task_id":"7","rating":"up"}"#);
        assert!(resp.contains("202 Accepted"), "resp: {resp}");
        let guard = store.lock().unwrap();
        let records = guard.list();
        assert_eq!(records.len(), 1);
        assert!(
            records[0].invocation_id.ends_with("-7"),
            "the task id is not in the invocation id: {}",
            records[0].invocation_id
        );
        assert_eq!(records[0].rating, feedback::FeedbackRating::ThumbsUp);
    }

    #[test]
    fn two_runs_rating_the_same_task_id_do_not_share_an_invocation() {
        // Scheduler task ids restart at `1 << 63` every process, so the bare id
        // would fold the n-th reply of one run into the n-th of the next when
        // `feedback::report` groups by invocation.
        let (first_addr, first_store) = start_with_feedback();
        let (second_addr, second_store) = start_with_feedback();
        assert!(post_feedback(first_addr, r#"{"task_id":"7","rating":"up"}"#).contains("202"));
        assert!(post_feedback(second_addr, r#"{"task_id":"7","rating":"down"}"#).contains("202"));

        let first = first_store.lock().unwrap().list()[0].invocation_id.clone();
        let second = second_store.lock().unwrap().list()[0].invocation_id.clone();
        assert_ne!(
            first, second,
            "two runs rating task 7 were filed against the same invocation"
        );
    }

    #[test]
    fn a_comment_is_stored_as_a_correction() {
        let (addr, store) = start_with_feedback();
        let resp = post_feedback(
            addr,
            r#"{"task_id":"7","rating":"down","comment":"it missed the second question"}"#,
        );
        assert!(resp.contains("202 Accepted"), "resp: {resp}");
        let guard = store.lock().unwrap();
        assert!(guard.list()[0].has_correction());
    }

    #[test]
    fn malformed_feedback_is_rejected_with_a_parseable_error() {
        let (addr, _store) = start_with_feedback();
        for body in [
            r#"{"rating":"up"}"#,                     // no task_id
            r#"{"task_id":"7"}"#,                     // no rating
            r#"{"task_id":"7","rating":"sideways"}"#, // unknown rating
            r#"{"task_id":"bad id","rating":"up"}"#,  // hostile id
            r#"not json at all"#,
        ] {
            let resp = post_feedback(addr, body);
            assert!(resp.contains("400 Bad Request"), "accepted {body}: {resp}");
            // Every error body must itself be valid JSON — a client that
            // parses the response must not choke on the rejection.
            let payload = resp.split("\r\n\r\n").nth(1).unwrap_or("").trim();
            assert!(
                serde_json::from_str::<serde_json::Value>(payload).is_ok(),
                "error body is not valid JSON: {payload}"
            );
        }
    }

    // ── E33 S33.1 — conversation history ──────────────────────────────────

    fn start_with_conversation(turns: usize) -> std::net::SocketAddr {
        use sessions::{ConversationRole, ConversationTurn, SessionRecord, SessionStore};
        let mut store = SessionStore::in_memory();
        store
            .insert(SessionRecord::new("sess-1", "user:operator", "anima"))
            .unwrap();
        for i in 0..turns {
            let role = if i % 2 == 0 {
                ConversationRole::User
            } else {
                ConversationRole::Assistant
            };
            store
                .append_turn(
                    "sess-1",
                    ConversationTurn::new(0, role, format!("turn {i}")),
                )
                .unwrap();
        }
        let hub = Arc::new(ConsoleHub::new());
        let bridge = SensoryBridge::new(HumanGuidance::new("test"));
        let server = ConsoleServer::new(
            hub,
            bridge,
            ServerConfig {
                addr: "127.0.0.1:0".into(),
                token: None,
            },
        )
        .with_conversation(Arc::new(Mutex::new(store)), "sess-1");
        server.spawn().expect("spawn").0
    }

    #[test]
    fn conversation_returns_404_when_not_wired() {
        let (addr, _hub, _bridge) = start();
        let resp = http_request(
            addr,
            "GET /conversation HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(resp.contains("404 Not Found"), "resp: {resp}");
    }

    #[test]
    fn conversation_serves_turns_oldest_first() {
        let addr = start_with_conversation(4);
        let resp = http_request(
            addr,
            "GET /conversation HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(resp.contains("200 OK"), "resp: {resp}");
        let body = resp.split("\r\n\r\n").nth(1).unwrap_or("");
        let first = body.find("turn 0").expect("oldest turn present");
        let last = body.find("turn 3").expect("newest turn present");
        assert!(first < last, "turns are not in chronological order: {body}");
        assert!(body.contains(r#""role":"user""#), "roles missing: {body}");
    }

    #[test]
    fn conversation_limit_returns_the_newest_turns() {
        let addr = start_with_conversation(10);
        let resp = http_request(
            addr,
            "GET /conversation?limit=3 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        let body = resp.split("\r\n\r\n").nth(1).unwrap_or("");
        assert!(body.contains("turn 9"), "newest turn missing: {body}");
        assert!(body.contains("turn 7"), "third-newest missing: {body}");
        assert!(
            !body.contains("turn 6"),
            "limit not applied from the newest end: {body}"
        );
    }

    #[test]
    fn conversation_before_pages_backwards_through_the_whole_session() {
        // Without a cursor the durable history is only readable one page deep,
        // so anything older than the newest page is unreachable from the UI
        // even though it is on disk.
        let addr = start_with_conversation(10);
        let page = |q: &str| -> String {
            let resp = http_request(
                addr,
                &format!("GET /conversation{q} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"),
            );
            assert!(resp.contains("200 OK"), "resp: {resp}");
            resp.split("\r\n\r\n").nth(1).unwrap_or("").to_string()
        };

        let newest = page("?limit=3");
        assert!(newest.contains("turn 9") && newest.contains("turn 7"));

        // `before` is the index of the oldest turn already held, exclusive.
        let older = page("?limit=3&before=7");
        assert!(older.contains("turn 6"), "page did not step back: {older}");
        assert!(older.contains("turn 4"), "page is short: {older}");
        assert!(
            !older.contains("turn 7"),
            "the cursor turn was served twice: {older}"
        );

        // Walking off the front yields the remainder, then nothing.
        let oldest = page("?limit=3&before=1");
        assert!(
            oldest.contains("turn 0"),
            "first turn unreachable: {oldest}"
        );
        let past_the_start = page("?limit=3&before=0");
        assert_eq!(
            past_the_start.trim(),
            "[]",
            "paging past the start should be empty: {past_the_start}"
        );
    }

    #[test]
    fn a_garbage_before_cursor_serves_the_newest_page() {
        // Same contract as `limit`: an unparseable cursor falls back rather
        // than erroring, so a stale client cannot lose its history.
        let addr = start_with_conversation(5);
        for q in ["?before=abc", "?before=-1", "?before="] {
            let resp = http_request(
                addr,
                &format!("GET /conversation{q} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"),
            );
            assert!(resp.contains("200 OK"), "{q} -> {resp}");
            let body = resp.split("\r\n\r\n").nth(1).unwrap_or("");
            assert!(body.contains("turn 4"), "{q} lost the newest turn: {body}");
        }
    }

    #[test]
    fn conversation_limit_is_capped_and_a_garbage_limit_falls_back() {
        // A caller must not be able to ask the server to serialise an entire
        // long-lived session, nor crash it with a non-numeric limit.
        let addr = start_with_conversation(5);
        for q in ["?limit=99999999", "?limit=abc", "?limit=0", "?limit=-4"] {
            let resp = http_request(
                addr,
                &format!("GET /conversation{q} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"),
            );
            assert!(resp.contains("200 OK"), "limit {q} failed: {resp}");
        }
    }

    #[test]
    fn conversation_for_an_unknown_session_is_empty_not_an_error() {
        use sessions::SessionStore;
        let hub = Arc::new(ConsoleHub::new());
        let bridge = SensoryBridge::new(HumanGuidance::new("test"));
        let addr = ConsoleServer::new(
            hub,
            bridge,
            ServerConfig {
                addr: "127.0.0.1:0".into(),
                token: None,
            },
        )
        .with_conversation(Arc::new(Mutex::new(SessionStore::in_memory())), "missing")
        .spawn()
        .expect("spawn")
        .0;
        let resp = http_request(
            addr,
            "GET /conversation HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(resp.contains("200 OK"), "resp: {resp}");
        assert!(
            resp.trim_end().ends_with("[]"),
            "expected empty list: {resp}"
        );
    }

    // ── E33 S33.2 ─────────────────────────────────────────────────────────

    #[test]
    fn accepted_guidance_reports_a_correlation_id_the_caller_can_follow() {
        let (addr, hub, _bridge) = start();
        let sub = hub.subscribe();
        let body = r#"{"text":"hello","priority":"Normal"}"#;
        let resp = http_request(
            addr,
            &format!(
                "POST /guidance HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            ),
        );
        assert!(resp.contains("202 Accepted"), "resp: {resp}");
        assert!(
            resp.contains(r#""message_id":"op-"#),
            "response carries no minted id: {resp}"
        );
        match sub.rx.recv_timeout(Duration::from_secs(2)).unwrap() {
            (_, OperatorEvent::Accepted { message_id, .. }) => {
                assert!(resp.contains(&message_id), "id in body differs from event")
            }
            other => panic!("expected Accepted, got {other:?}"),
        }
    }

    #[test]
    fn a_client_supplied_correlation_id_is_honoured_and_reaches_the_packet() {
        let (addr, hub, bridge) = start();
        let sub = hub.subscribe();
        let body = r#"{"text":"hello","priority":"High","message_id":"client-42"}"#;
        let resp = http_request(
            addr,
            &format!(
                "POST /guidance HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            ),
        );
        assert!(resp.contains("202 Accepted"), "resp: {resp}");
        match sub.rx.recv_timeout(Duration::from_secs(2)).unwrap() {
            (_, OperatorEvent::Accepted { message_id, .. }) => {
                assert_eq!(message_id, "client-42")
            }
            other => panic!("expected Accepted, got {other:?}"),
        }
        let packet = bridge.next_prioritized_packet().expect("packet enqueued");
        assert_eq!(packet.message_id.as_deref(), Some("client-42"));
    }

    #[test]
    fn a_hostile_correlation_id_is_rejected_rather_than_written_to_the_audit_log() {
        // The id lands in the durable audit trail and in every attached
        // console's DOM, so the operator channel does not get to choose its
        // alphabet (threat model §5).
        let (addr, _hub, _bridge) = start();
        for hostile in [
            r#"<script>alert(1)</script>"#,
            r#"a"b"#,
            "",
            "x x",
            "../../etc/passwd",
        ] {
            let body = format!(
                r#"{{"text":"hello","message_id":"{}"}}"#,
                hostile.replace('\\', "\\\\").replace('"', "\\\"")
            );
            let resp = http_request(
                addr,
                &format!(
                    "POST /guidance HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                ),
            );
            assert!(
                resp.contains("400 Bad Request"),
                "accepted hostile id {hostile:?}: {resp}"
            );
        }
    }

    #[test]
    fn an_over_long_correlation_id_is_rejected() {
        let (addr, _hub, _bridge) = start();
        let body = format!(
            r#"{{"text":"hello","message_id":"{}"}}"#,
            "a".repeat(MAX_MESSAGE_ID_LEN + 1)
        );
        let resp = http_request(
            addr,
            &format!(
                "POST /guidance HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            ),
        );
        assert!(resp.contains("400 Bad Request"), "resp: {resp}");
    }

    #[test]
    fn forced_guidance_is_accepted_as_critical_with_its_reason() {
        let (addr, hub, _bridge) = start();
        let sub = hub.subscribe();
        let body = r#"{"text":"check disk","priority":"Low","force":"operator emergency"}"#;
        let _ = http_request(
            addr,
            &format!(
                "POST /guidance HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            ),
        );
        match sub.rx.recv_timeout(Duration::from_secs(2)).unwrap() {
            (
                _,
                OperatorEvent::Accepted {
                    priority,
                    forced,
                    force_reason,
                    ..
                },
            ) => {
                assert!(forced);
                // A forced line is enqueued at Critical whatever the client asked for.
                assert_eq!(priority, Priority::Critical);
                assert_eq!(force_reason.as_deref(), Some("operator emergency"));
            }
            other => panic!("expected Accepted, got {other:?}"),
        }
    }

    #[test]
    fn heartbeat_arrives_on_a_busy_stream_not_only_an_idle_one() {
        // Regression: the beat used to ride the receive timeout alone, so the
        // 1 Hz vitals of a live agent starved it and the uptime chip never
        // painted.  Publish continuously and assert a beat still lands.
        let (addr, hub, _bridge) = start();
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(b"GET /events HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        s.set_read_timeout(Some(HEARTBEAT_INTERVAL * 3)).unwrap();

        let pump = std::thread::spawn(move || {
            for i in 0..(HEARTBEAT_INTERVAL.as_secs() * 4 + 8) {
                hub.publish(OperatorEvent::Audit {
                    kind: "Noise".into(),
                    detail: format!("tick {i}"),
                    message_id: None,
                });
                std::thread::sleep(Duration::from_millis(250));
            }
        });

        let mut reader = BufReader::new(s);
        let deadline = std::time::Instant::now() + HEARTBEAT_INTERVAL * 3;
        let mut saw_beat = false;
        while std::time::Instant::now() < deadline {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            if line.contains("\"Heartbeat\"") {
                saw_beat = true;
                break;
            }
        }
        let _ = pump.join();
        assert!(saw_beat, "no heartbeat observed while events were flowing");
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
            message_id: None,
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
                message_id: None,
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
                message_id: None,
                task_id: 1,
                tokens: 1,
                text: "historical-replay".into(),
            },
        );
        hub.publish_at(
            11,
            OperatorEvent::AgentMessage {
                message_id: None,
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

    // ── S15.1: GET /digest ────────────────────────────────────────────────────

    #[test]
    fn digest_endpoint_503_when_not_configured() {
        // A server created without with_digest() must return 503, not panic.
        let (addr, _hub, _bridge) = start();
        let resp = http_request(
            addr,
            "GET /digest HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(
            resp.contains("503"),
            "expected 503 without digest source: {resp}"
        );
    }

    #[test]
    fn digest_endpoint_returns_json_when_wired() {
        use std::io::Write as IoWrite;

        let dir = std::env::temp_dir().join(format!("anima-digest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent.jsonl");

        let hub = Arc::new(ConsoleHub::new());
        let bridge = SensoryBridge::new(senses::HumanGuidance::new("t"));
        let server = ConsoleServer::new(
            hub.clone(),
            bridge,
            ServerConfig {
                addr: "127.0.0.1:0".into(),
                token: None,
            },
        )
        .with_digest(path.clone(), "agent-a".to_string());
        let (addr, _h) = server.spawn().expect("spawn");

        // Write one completed task into the audit log.
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(
            f,
            r#"{{"TaskCompleted":{{"agent_id":"agent-a","task_id":1,"tokens_emitted":42,"response":"ok"}}}}"#
        )
        .unwrap();
        f.flush().unwrap();

        let resp = http_request(
            addr,
            "GET /digest HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(resp.contains("200 OK"), "resp: {resp}");
        assert!(
            resp.contains("application/json"),
            "must be JSON content-type: {resp}"
        );
        assert!(
            resp.contains("tasks_completed"),
            "digest payload must include tasks_completed: {resp}"
        );
        // The one entry we wrote should be counted.
        assert!(
            resp.contains(r#""tasks_completed":1"#),
            "tasks_completed should be 1: {resp}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── E15 S15.2: /approval-queue endpoints ──────────────────────────────────

    fn start_with_queue() -> (
        std::net::SocketAddr,
        Arc<Mutex<lifecycle::approval::ApprovalQueue>>,
    ) {
        use lifecycle::approval::{Proposal, ProposalKind, ProposalStatus};
        let hub = Arc::new(ConsoleHub::new());
        let bridge = SensoryBridge::new(HumanGuidance::new("test"));
        let queue = Arc::new(Mutex::new(lifecycle::approval::ApprovalQueue::new()));
        // Seed one pending proposal for tests.
        queue.lock().unwrap().enqueue(Proposal {
            id: "test-p1".to_string(),
            kind: ProposalKind::NewSkill {
                name: "test-skill".to_string(),
                description: "a test skill".to_string(),
                prompt_hash: "abc123".to_string(),
            },
            created_at_ns: 1_000_000_000,
            provenance: "test".to_string(),
            sandbox_result: None,
            defence_verdict: None,
            status: ProposalStatus::Pending,
        });
        let server = ConsoleServer::new(
            hub.clone(),
            bridge.clone(),
            ServerConfig {
                addr: "127.0.0.1:0".into(),
                token: None,
            },
        )
        .with_approval_queue(Arc::clone(&queue));
        let (addr, _h) = server.spawn().expect("spawn");
        (addr, queue)
    }

    #[test]
    fn approval_queue_returns_404_when_not_wired() {
        let (addr, _hub, _bridge) = start();
        let resp = http_request(
            addr,
            "GET /approval-queue HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(resp.contains("404"), "expected 404: {resp}");
    }

    #[test]
    fn approval_queue_lists_proposals_as_json() {
        let (addr, _q) = start_with_queue();
        let resp = http_request(
            addr,
            "GET /approval-queue HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(resp.contains("200 OK"), "status: {resp}");
        assert!(resp.contains("application/json"), "content-type: {resp}");
        assert!(resp.contains("test-p1"), "proposal id missing: {resp}");
        assert!(resp.contains("NewSkill"), "proposal kind missing: {resp}");
    }

    #[test]
    fn approval_queue_approve_transitions_proposal_to_approved() {
        let (addr, queue) = start_with_queue();
        let body = r#"{"reason":"looks good"}"#;
        let req = format!(
            "POST /approval-queue/test-p1/approve HTTP/1.1\r\n\
             Host: x\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let resp = http_request(addr, &req);
        assert!(resp.contains("200 OK"), "status: {resp}");
        assert!(resp.contains(r#""ok":true"#), "body: {resp}");
        assert!(
            queue.lock().unwrap().get("test-p1").unwrap().is_approved(),
            "proposal should be approved"
        );
    }

    /// A queue and a registry holding the *same* proposed skill, exactly as
    /// `cmd_serve` wires them: one registry handle shared by the agent and the
    /// console, and a proposal whose queue id is the registry skill id.
    fn start_with_queue_and_registry() -> (
        std::net::SocketAddr,
        Arc<Mutex<lifecycle::approval::ApprovalQueue>>,
        Arc<Mutex<skills::SkillRegistry>>,
    ) {
        use lifecycle::approval::{Proposal, ProposalKind, ProposalStatus};
        use skills::{SkillProvenance, SkillState};

        let mut reg = skills::SkillRegistry::default();
        let skill_id = reg
            .register_from_text(
                "---\nname: log-triage\ndescription: triage overnight logs\n---\nRead the log.",
                SkillProvenance::agent(1_000_000_000, "ep-1"),
                SkillState::Proposed,
            )
            .expect("register");
        let registry = Arc::new(Mutex::new(reg));

        let queue = Arc::new(Mutex::new(lifecycle::approval::ApprovalQueue::new()));
        queue.lock().unwrap().enqueue(Proposal {
            id: skill_id,
            kind: ProposalKind::NewSkill {
                name: "log-triage".to_string(),
                description: "triage overnight logs".to_string(),
                prompt_hash: "abc123".to_string(),
            },
            created_at_ns: 1_000_000_000,
            provenance: "agent (dreaming-phase reflection)".to_string(),
            sandbox_result: None,
            defence_verdict: None,
            status: ProposalStatus::Pending,
        });

        let server = ConsoleServer::new(
            Arc::new(ConsoleHub::new()),
            SensoryBridge::new(HumanGuidance::new("test")),
            ServerConfig {
                addr: "127.0.0.1:0".into(),
                token: None,
            },
        )
        .with_approval_queue(Arc::clone(&queue))
        .with_skill_registry(Arc::clone(&registry));
        let (addr, _h) = server.spawn().expect("spawn");
        (addr, queue, registry)
    }

    fn skill_state(registry: &Arc<Mutex<skills::SkillRegistry>>) -> skills::SkillState {
        let guard = registry.lock().unwrap();
        guard.list_all().first().expect("one skill").state.clone()
    }

    fn approval_action(addr: std::net::SocketAddr, id: &str, action: &str) -> String {
        let body = r#"{"reason":"looks good"}"#;
        http_request(
            addr,
            &format!(
                "POST /approval-queue/{id}/{action} HTTP/1.1\r\n\
                 Host: x\r\n\
                 Content-Type: application/json\r\n\
                 Content-Length: {}\r\n\
                 Connection: close\r\n\r\n{}",
                body.len(),
                body
            ),
        )
    }

    #[test]
    fn approving_a_skill_promotes_it_in_the_registry() {
        // Approving in the queue alone left the skill `Proposed`, so the thing
        // the operator had just said yes to still could not be selected.
        let (addr, queue, registry) = start_with_queue_and_registry();
        assert_eq!(skill_state(&registry), skills::SkillState::Proposed);

        let resp = approval_action(addr, "log-triage", "approve");
        assert!(resp.contains("200 OK"), "status: {resp}");
        assert!(resp.contains(r#""ok":true"#), "body: {resp}");
        assert!(
            queue
                .lock()
                .unwrap()
                .get("log-triage")
                .unwrap()
                .is_approved(),
            "queue entry should be approved"
        );
        assert_eq!(
            skill_state(&registry),
            skills::SkillState::Active,
            "the approved skill is still not selectable"
        );
    }

    #[test]
    fn rejecting_a_skill_leaves_the_registry_alone() {
        // `SkillApprovalBridge::reject` deliberately does not roll the skill
        // back, and the console must not diverge from it.
        let (addr, queue, registry) = start_with_queue_and_registry();
        let resp = approval_action(addr, "log-triage", "reject");
        assert!(resp.contains("200 OK"), "status: {resp}");
        use lifecycle::approval::ProposalStatus;
        assert!(
            matches!(
                queue.lock().unwrap().get("log-triage").unwrap().status,
                ProposalStatus::Rejected { .. }
            ),
            "queue entry should be rejected"
        );
        assert_eq!(skill_state(&registry), skills::SkillState::Proposed);
    }

    #[test]
    fn a_skill_approval_that_cannot_be_activated_is_reported_as_a_failure() {
        // The queue records the approval before the registry is touched, so a
        // proposal naming a skill the registry does not hold must not come back
        // as "ok" — that would claim an activation that did not happen.
        use lifecycle::approval::{Proposal, ProposalKind, ProposalStatus};
        let (addr, queue, registry) = start_with_queue_and_registry();
        queue.lock().unwrap().enqueue(Proposal {
            id: "ghost-skill".to_string(),
            kind: ProposalKind::NewSkill {
                name: "ghost-skill".to_string(),
                description: "never registered".to_string(),
                prompt_hash: "def456".to_string(),
            },
            created_at_ns: 1_000_000_000,
            provenance: "test".to_string(),
            sandbox_result: None,
            defence_verdict: None,
            status: ProposalStatus::Pending,
        });

        let resp = approval_action(addr, "ghost-skill", "approve");
        assert!(resp.contains("500"), "status: {resp}");
        assert!(resp.contains(r#""ok":false"#), "body: {resp}");
        assert!(
            resp.contains("could not be activated"),
            "the operator is not told what happened: {resp}"
        );
        // The real skill is untouched by the failed one.
        assert_eq!(skill_state(&registry), skills::SkillState::Proposed);
    }

    #[test]
    fn approval_queue_reject_transitions_proposal_to_rejected() {
        let (addr, queue) = start_with_queue();
        let body = r#"{"reason":"too risky"}"#;
        let req = format!(
            "POST /approval-queue/test-p1/reject HTTP/1.1\r\n\
             Host: x\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let resp = http_request(addr, &req);
        assert!(resp.contains("200 OK"), "status: {resp}");
        use lifecycle::approval::ProposalStatus;
        assert!(
            matches!(
                queue.lock().unwrap().get("test-p1").unwrap().status,
                ProposalStatus::Rejected { .. }
            ),
            "proposal should be rejected"
        );
    }

    #[test]
    fn approval_queue_approve_unknown_id_returns_422() {
        let (addr, _q) = start_with_queue();
        let body = r#"{"reason":"ok"}"#;
        let req = format!(
            "POST /approval-queue/no-such-id/approve HTTP/1.1\r\n\
             Host: x\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let resp = http_request(addr, &req);
        assert!(resp.contains("422"), "expected 422: {resp}");
    }

    #[test]
    fn percent_encoded_proposal_id_is_decoded_server_side() {
        use lifecycle::approval::{Proposal, ProposalKind, ProposalStatus};
        let hub = Arc::new(ConsoleHub::new());
        let bridge = SensoryBridge::new(HumanGuidance::new("test"));
        let queue = Arc::new(Mutex::new(lifecycle::approval::ApprovalQueue::new()));
        // Proposal whose id contains characters that encodeURIComponent encodes.
        // '@' → %40  (WeightUpdate ids)   '?' → %3F  (hypothetical edge-case)
        let raw_id = "adapter@abc123";
        queue.lock().unwrap().enqueue(Proposal {
            id: raw_id.to_string(),
            kind: ProposalKind::NewSkill {
                name: "x".into(),
                description: "x".into(),
                prompt_hash: "x".into(),
            },
            created_at_ns: 1,
            provenance: "test".into(),
            sandbox_result: None,
            defence_verdict: None,
            status: ProposalStatus::Pending,
        });
        let server = ConsoleServer::new(
            hub,
            bridge,
            ServerConfig {
                addr: "127.0.0.1:0".into(),
                token: None,
            },
        )
        .with_approval_queue(Arc::clone(&queue));
        let (addr, _h) = server.spawn().expect("spawn");

        // Client sends the id percent-encoded (as encodeURIComponent would).
        let encoded_id = "adapter%40abc123";
        let body = r#"{}"#;
        let req = format!(
            "POST /approval-queue/{encoded_id}/approve HTTP/1.1\r\n\
             Host: x\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\r\n{{}}",
            body.len()
        );
        let resp = http_request(addr, &req);
        assert!(
            resp.contains("200 OK"),
            "server should decode %40 to @ and find the proposal: {resp}"
        );
        assert!(
            queue.lock().unwrap().get(raw_id).unwrap().is_approved(),
            "proposal should be approved"
        );
    }

    #[test]
    fn percent_decode_handles_multibyte_utf8_sequences() {
        // é = U+00E9 = UTF-8 bytes [0xC3, 0xA9] = %C3%A9 (as encodeURIComponent produces)
        assert_eq!(percent_decode("%C3%A9"), "é");
        // ASCII round-trips unchanged.
        assert_eq!(percent_decode("hello-world"), "hello-world");
        // @ (ASCII but encoded by encodeURIComponent) decodes correctly.
        assert_eq!(percent_decode("adapter%40abc123"), "adapter@abc123");
        // Incomplete sequence is passed through literally (graceful degradation).
        assert_eq!(percent_decode("%"), "%");
        assert_eq!(percent_decode("%2"), "%2");
        // Invalid hex nibble is left as-is.
        assert_eq!(percent_decode("%GG"), "%GG");
    }

    // ── E11 S11.1: /skills endpoint ───────────────────────────────────────────

    fn start_with_skills() -> std::net::SocketAddr {
        let hub = Arc::new(ConsoleHub::new());
        let bridge = SensoryBridge::new(HumanGuidance::new("test"));
        let registry = Arc::new(Mutex::new(skills::SkillRegistry::with_builtins()));
        let server = ConsoleServer::new(
            hub.clone(),
            bridge.clone(),
            ServerConfig {
                addr: "127.0.0.1:0".into(),
                token: None,
            },
        )
        .with_skill_registry(registry);
        let (addr, _h) = server.spawn().expect("spawn");
        addr
    }

    #[test]
    fn skills_returns_404_when_not_wired() {
        let (addr, _hub, _bridge) = start();
        let resp = http_request(
            addr,
            "GET /skills HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(resp.contains("404"), "expected 404: {resp}");
    }

    #[test]
    fn skills_lists_builtin_skills_as_json() {
        let addr = start_with_skills();
        let resp = http_request(
            addr,
            "GET /skills HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(resp.contains("200 OK"), "status: {resp}");
        assert!(resp.contains("application/json"), "content-type: {resp}");
        // At least one builtin skill must be present.
        assert!(
            resp.contains(r#""state":"Active""#),
            "no active skills: {resp}"
        );
    }

    // ── E8: /adapters endpoint ────────────────────────────────────────────────

    #[test]
    fn adapters_returns_404_when_not_wired() {
        let (addr, _hub, _bridge) = start();
        let resp = http_request(
            addr,
            "GET /adapters HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(resp.contains("404"), "expected 404: {resp}");
    }

    #[test]
    fn adapters_lists_empty_library_as_json_array() {
        let hub = Arc::new(ConsoleHub::new());
        let bridge = SensoryBridge::new(HumanGuidance::new("test"));
        let library = Arc::new(Mutex::new(anima_finetune::AdapterLibrary::new(10)));
        let server = ConsoleServer::new(
            hub.clone(),
            bridge.clone(),
            ServerConfig {
                addr: "127.0.0.1:0".into(),
                token: None,
            },
        )
        .with_adapter_library(library);
        let (addr, _h) = server.spawn().expect("spawn");
        let resp = http_request(
            addr,
            "GET /adapters HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(resp.contains("200 OK"), "status: {resp}");
        assert!(resp.contains("application/json"), "content-type: {resp}");
        assert!(resp.contains("[]"), "empty array expected: {resp}");
    }

    // ── WeightUpdate approval → adapter library wiring ────────────────────────

    #[test]
    fn approving_weight_update_proposal_calls_record_operator_approval() {
        use anima_finetune::{
            AdapterLibrary, AdoptionDecision, FineTuneConfig, FineTuneJob, FineTuner,
            FixtureFineTuner, TrainingPair, TrainingSet,
        };
        use lifecycle::approval::{ApprovalQueue, Proposal, ProposalKind, ProposalStatus};

        // Train a deterministic fixture adapter so we get a real artifact with a
        // populated weights_digest, which record_adoption validates against.
        let tuner = FixtureFineTuner::new();
        let cfg = FineTuneConfig::new("base-q4", "episodic://test", "nightly-adapter");
        let job = FineTuneJob::new("test-job".to_string(), cfg);
        let pairs = vec![TrainingPair::new("q?", "a")];
        let set = TrainingSet::from_pairs(&pairs);
        let artifact = tuner.run_job(&job, set.pairs()).unwrap();
        let adapter_id = artifact.adapter_id.clone();
        let weights_digest = artifact.weights_digest.clone();

        // Register the adapter and record automated adoption so only operator
        // sign-off is missing before mount_gated would succeed.
        let mut lib = AdapterLibrary::new(8);
        lib.register(artifact.clone()).unwrap();
        lib.record_adoption(&AdoptionDecision {
            adapter_id: adapter_id.clone(),
            weights_digest: weights_digest.clone(),
            approved: true,
            eval_passed: true,
            alignment_passed: true,
            reasons: vec![],
        });
        assert!(
            lib.is_adopted(&adapter_id),
            "precondition: adoption recorded"
        );
        assert!(
            !lib.is_operator_approved(&adapter_id),
            "precondition: not yet operator-approved"
        );
        let library = Arc::new(Mutex::new(lib));

        // Enqueue a WeightUpdate proposal whose ID carries the same adapter_id@digest.
        let proposal_id = format!("{adapter_id}@{weights_digest}");
        let mut queue = ApprovalQueue::new();
        queue.enqueue(Proposal {
            id: proposal_id.clone(),
            kind: ProposalKind::WeightUpdate {
                model_id: "base-q4".to_string(),
                adapter_hash: weights_digest.clone(),
                rank: None,
                training_summary: "1 pair".to_string(),
            },
            created_at_ns: 1,
            provenance: "adoption gate".to_string(),
            sandbox_result: None,
            defence_verdict: None,
            status: ProposalStatus::Pending,
        });
        let shared_queue = Arc::new(Mutex::new(queue));

        let hub = Arc::new(ConsoleHub::new());
        let bridge = SensoryBridge::new(HumanGuidance::new("test"));
        let server = ConsoleServer::new(
            hub,
            bridge,
            ServerConfig {
                addr: "127.0.0.1:0".into(),
                token: None,
            },
        )
        .with_approval_queue(Arc::clone(&shared_queue))
        .with_adapter_library(Arc::clone(&library));
        let (addr, _h) = server.spawn().expect("spawn");

        let body = r#"{"reason":"operator approved"}"#;
        let req = format!(
            "POST /approval-queue/{}/approve HTTP/1.1\r\n\
             Host: x\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\r\n{}",
            proposal_id,
            body.len(),
            body
        );
        let resp = http_request(addr, &req);
        assert!(resp.contains("200 OK"), "status: {resp}");
        assert!(
            library.lock().unwrap().is_operator_approved(&adapter_id),
            "adapter library should reflect operator approval after the endpoint returns"
        );
    }
}
