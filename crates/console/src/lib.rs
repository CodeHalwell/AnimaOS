#![forbid(unsafe_code)]

//! The AnimaOS operator console — the human-facing surface of an agent that is
//! otherwise its own user.
//!
//! AnimaOS models the human operator as *a high-priority sense*, not a
//! controller (`docs/01-architecture.md §1.3`; `docs/11-operator-interface.md`).
//! This crate is the realisation of that idea as two channels:
//!
//! - **Afferent (human → agent):** a `POST /guidance` ingress turns an
//!   [`console_proto::OperatorInput`] into a prioritised sensory packet on the
//!   agent's shared [`senses::SensoryBridge`]. It is validated against the
//!   agent's `HumanGuidance` policy and then arbitrated by the Striatal Gate —
//!   it never preempts the kernel.
//! - **Efferent / interoceptive (agent → human):** a [`ConsoleHub`] fans
//!   [`console_proto::OperatorEvent`]s — 1 Hz vitals, lifecycle state, gate
//!   rationale, audit deltas, and the agent's own messages — out to every
//!   connected operator over Server-Sent Events.
//!
//! # Integration seam (zero changes to `vita`)
//!
//! The console never reaches into the lifecycle. It observes the agent through
//! the **durable audit log** `vita` already writes (`$ANIMA_AUDIT_DIR`): the
//! [`AuditTailer`] follows that JSONL file and republishes each entry as an
//! [`OperatorEvent`]. The only shared mutable handle is the `SensoryBridge`,
//! which is `Clone` and thread-safe by construction.
//!
//! # One protocol, two transports
//!
//! Everything here speaks [`console_proto`], which is also `no_std`-clean for
//! the microVM kernel. The container/hosted surface uses this crate's HTTP/SSE
//! server; the microVM frames the identical NDJSON onto COM1 and a host-side
//! bridge (`anima-console serial`) re-publishes it over the same HTTP surface,
//! so a single dashboard works against both.

mod audit;
mod hub;
mod server;

pub use audit::{event_from_audit_line, event_from_audit_value, AuditTailer};
pub use hub::{ConsoleHub, Subscription};
pub use server::{ConsoleServer, ServerConfig};

// Re-export the protocol so downstream binaries need only depend on `console`.
pub use console_proto::{self, OperatorEvent, OperatorInput, Priority};

// Re-export the queue/registry types so callers that wire them don't need
// extra crate imports on top of `console`.
pub use lifecycle::ApprovalQueue;
pub use skills::SkillRegistry;
pub use vita::AuditLog;

/// The self-contained browser dashboard served at `GET /`.
pub const DASHBOARD_HTML: &str = include_str!("dashboard.html");

use std::sync::{Arc, Mutex};

/// A fully-wired console: the broadcast hub, the HTTP/SSE server, and the audit
/// tailer that feeds the hub from a `vita` audit JSONL file.
///
/// This is the one-call wiring used by the hosted `serve` command. Construct it
/// with the *same* `SensoryBridge` the lifecycle owns (a clone), point it at
/// the agent's audit log path, and call [`Console::start`].
///
/// Optionally wire the E15 approval queue and E11 skill registry for the
/// Pillar 3 dashboard panels:
///
/// ```ignore
/// let console = Console::new(bridge, audit_path, config)
///     .with_approval_queue(Arc::clone(&queue))
///     .with_skill_registry(Arc::clone(&registry));
/// ```
pub struct Console {
    hub: Arc<ConsoleHub>,
    bridge: senses::SensoryBridge,
    audit_path: std::path::PathBuf,
    config: ServerConfig,
    approval_queue: Option<Arc<Mutex<ApprovalQueue>>>,
    skill_registry: Option<Arc<Mutex<SkillRegistry>>>,
    audit_log: Option<Arc<Mutex<AuditLog>>>,
    agent_id: Option<String>,
}

impl Console {
    /// Create a console bound to a shared sensory bridge and an audit-log path.
    pub fn new(
        bridge: senses::SensoryBridge,
        audit_path: impl Into<std::path::PathBuf>,
        config: ServerConfig,
    ) -> Self {
        Self {
            hub: Arc::new(ConsoleHub::new()),
            bridge,
            audit_path: audit_path.into(),
            config,
            approval_queue: None,
            skill_registry: None,
            audit_log: None,
            agent_id: None,
        }
    }

    /// Wire the E15 approval queue so the dashboard's approval-queue panel
    /// can list, approve, and reject pending proposals.
    pub fn with_approval_queue(mut self, q: Arc<Mutex<ApprovalQueue>>) -> Self {
        self.approval_queue = Some(q);
        self
    }

    /// Wire the E11 skill registry so the dashboard's skills panel shows
    /// the live registry contents.
    pub fn with_skill_registry(mut self, r: Arc<Mutex<SkillRegistry>>) -> Self {
        self.skill_registry = Some(r);
        self
    }

    /// Wire a durable audit log so dashboard approve/reject decisions are
    /// persisted as [`vita::AuditEntry::ApprovalProposalDecided`] entries.
    ///
    /// Pass a second `AuditLog::with_file` handle opened against the same path
    /// as the somatic-loop log; both writers use `O_APPEND` so concurrent line
    /// appends are safe on Linux.
    pub fn with_audit_log(mut self, log: Arc<Mutex<AuditLog>>) -> Self {
        self.audit_log = Some(log);
        self
    }

    /// Set the agent identifier recorded in dashboard approval audit entries.
    /// Defaults to `"anima"`. Pass `ANIMA_AGENT_ID` so audit consumers can
    /// attribute decisions to the correct agent in multi-agent deployments.
    pub fn with_agent_id(mut self, id: impl Into<String>) -> Self {
        self.agent_id = Some(id.into());
        self
    }

    /// The shared event hub — also usable as an `interoception::SignalPublisher`
    /// if the caller wants to inject 1 Hz vitals directly rather than via the
    /// audit log.
    pub fn hub(&self) -> Arc<ConsoleHub> {
        Arc::clone(&self.hub)
    }

    /// Publish an [`OperatorEvent`] directly to all connected operators. The
    /// hosted `serve` driver calls this with a precise
    /// [`OperatorEvent::State`] after each batch so the dashboard's state panel
    /// shows real agenda depth (the audit-derived state events are coarser).
    pub fn publish(&self, event: OperatorEvent) {
        self.hub.publish(event);
    }

    /// Start the audit tailer and the HTTP server on background threads,
    /// returning the resolved server address.
    pub fn start(&self) -> std::io::Result<std::net::SocketAddr> {
        AuditTailer::new(self.audit_path.clone(), self.hub()).spawn();
        let mut server = ConsoleServer::new(self.hub(), self.bridge.clone(), self.config.clone());
        if let Some(q) = self.approval_queue.clone() {
            server = server.with_approval_queue(q);
        }
        if let Some(r) = self.skill_registry.clone() {
            server = server.with_skill_registry(r);
        }
        if let Some(l) = self.audit_log.clone() {
            server = server.with_audit_log(l);
        }
        if let Some(id) = self.agent_id.clone() {
            server = server.with_agent_id(id);
        }
        let (addr, _handle) = server.spawn()?;
        Ok(addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_html_is_embedded_and_nonempty() {
        assert!(DASHBOARD_HTML.contains("EventSource"));
        assert!(DASHBOARD_HTML.contains("/guidance"));
    }

    #[test]
    fn console_wires_and_starts() {
        let dir = std::env::temp_dir().join(format!("anima-console-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bridge = senses::SensoryBridge::new(senses::HumanGuidance::new("t"));
        let console = Console::new(
            bridge,
            dir.join("agent.jsonl"),
            ServerConfig {
                addr: "127.0.0.1:0".into(),
                token: None,
            },
        );
        let addr = console.start().expect("starts");
        assert_ne!(addr.port(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
