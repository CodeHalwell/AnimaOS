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

/// The self-contained browser dashboard served at `GET /`.
pub const DASHBOARD_HTML: &str = include_str!("dashboard.html");

use std::sync::{Arc, Mutex};

/// A fully-wired console: the broadcast hub, the HTTP/SSE server, and the audit
/// tailer that feeds the hub from a `vita` audit JSONL file.
///
/// This is the one-call wiring used by the hosted `serve` command. Construct it
/// with the *same* `SensoryBridge` the lifecycle owns (a clone), point it at
/// the agent's audit log path, and call [`Console::start`].
pub struct Console {
    hub: Arc<ConsoleHub>,
    bridge: senses::SensoryBridge,
    audit_path: std::path::PathBuf,
    config: ServerConfig,
    approval_queue: Option<Arc<Mutex<lifecycle::approval::ApprovalQueue>>>,
    skill_registry: Option<Arc<Mutex<skills::SkillRegistry>>>,
    adapter_library: Option<Arc<Mutex<anima_finetune::AdapterLibrary>>>,
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
            adapter_library: None,
        }
    }

    /// Wire in a shared approval queue so the console can serve
    /// `GET /approval-queue` and the approve/reject action endpoints.
    pub fn with_approval_queue(
        mut self,
        queue: Arc<Mutex<lifecycle::approval::ApprovalQueue>>,
    ) -> Self {
        self.approval_queue = Some(queue);
        self
    }

    /// Wire in a shared skill registry so the console can serve `GET /skills`.
    pub fn with_skill_registry(mut self, registry: Arc<Mutex<skills::SkillRegistry>>) -> Self {
        self.skill_registry = Some(registry);
        self
    }

    /// Wire in a shared adapter library so the console can serve `GET /adapters`.
    pub fn with_adapter_library(
        mut self,
        library: Arc<Mutex<anima_finetune::AdapterLibrary>>,
    ) -> Self {
        self.adapter_library = Some(library);
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
        let agent_id = std::env::var("ANIMA_AGENT_ID").unwrap_or_else(|_| "anima".to_string());
        let mut server = ConsoleServer::new(self.hub(), self.bridge.clone(), self.config.clone())
            .with_digest(self.audit_path.clone(), agent_id);
        if let Some(q) = &self.approval_queue {
            server = server.with_approval_queue(Arc::clone(q));
        }
        if let Some(r) = &self.skill_registry {
            server = server.with_skill_registry(Arc::clone(r));
        }
        if let Some(l) = &self.adapter_library {
            server = server.with_adapter_library(Arc::clone(l));
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
