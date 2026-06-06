//! S15.1 — "While you were away" activity digest.
//!
//! Generates a structured, operator-facing summary of autonomous activity from
//! the existing durable [`vita::audit::AuditEntry`] stream.  No new
//! instrumentation is required — all data comes from the entries already
//! written by the scheduler, gate, router, cortex bridge, and defence layer.
//!
//! ## Design principles
//!
//! - **Pure function**: [`generate_digest`] is a deterministic fold over a
//!   `&[AuditEntry]` slice; it has no side-effects and can be called repeatedly.
//! - **Salience filter**: only operationally significant events appear in
//!   [`ActivityDigest::notable_events`]; routine clock ticks are counted but
//!   not individually narrated.
//! - **Composable**: callers may window the entry slice by index or by
//!   monotonic timestamp (when `tick_ns` fields are present) before calling
//!   [`generate_digest`], making cadence tuning a caller concern.

use serde::{Deserialize, Serialize};
use vita::audit::AuditEntry;

// ── ActivityDigest ────────────────────────────────────────────────────────────

/// Operator-facing summary of autonomous agent activity over an entry window.
///
/// Produced by [`generate_digest`] from a slice of [`AuditEntry`] values.
/// All counters are non-negative; a zero counter means the event type did not
/// occur in the window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityDigest {
    /// Agent identifier (mirrors the `agent_id` field in audit entries).
    pub agent_id: String,

    // ── Task outcomes ─────────────────────────────────────────────────────────
    /// Number of tasks that completed successfully in the window.
    pub tasks_completed: usize,
    /// Number of tasks that failed or were cancelled.
    pub tasks_failed: usize,
    /// Total tokens emitted by completed tasks.
    pub total_tokens_emitted: u64,

    // ── Cortex activity ───────────────────────────────────────────────────────
    /// Number of cortex invocations (gate `invoke = true`) in the window.
    pub cortex_invocations: usize,
    /// Number of cortex faults recorded in the window.
    pub cortex_faults: usize,

    // ── Sleep cycles ─────────────────────────────────────────────────────────
    /// Number of sleep cycles entered.
    pub sleep_cycles: usize,

    // ── Safety signals ────────────────────────────────────────────────────────
    /// Number of defence-layer vetoes issued.
    pub defence_vetoes: usize,
    /// Number of attention-demand escalations raised to the operator.
    pub attention_escalations: usize,

    // ── Gate / routing ────────────────────────────────────────────────────────
    /// Total gate decisions evaluated (both invoked and blocked).
    pub gate_decisions: usize,
    /// Gate decisions that resulted in a cortex invocation.
    pub gate_invocations: usize,
    /// Gate decisions that were blocked (value below threshold).
    pub gate_blocks: usize,
    /// Decisions where homeostatic pressure downgraded the route.
    pub route_modulations: usize,

    // ── Notable events ────────────────────────────────────────────────────────
    /// Events that warrant explicit operator attention (sorted by insertion
    /// order, which mirrors the original audit-log order).
    pub notable_events: Vec<NotableEvent>,
}

impl ActivityDigest {
    fn new(agent_id: &str) -> Self {
        ActivityDigest {
            agent_id: agent_id.to_string(),
            tasks_completed: 0,
            tasks_failed: 0,
            total_tokens_emitted: 0,
            cortex_invocations: 0,
            cortex_faults: 0,
            sleep_cycles: 0,
            defence_vetoes: 0,
            attention_escalations: 0,
            gate_decisions: 0,
            gate_invocations: 0,
            gate_blocks: 0,
            route_modulations: 0,
            notable_events: Vec::new(),
        }
    }

    /// Human-readable single-line summary of the digest.
    ///
    /// Suitable for the first line of a push-notification or channel message.
    pub fn headline(&self) -> String {
        format!(
            "{}: {} tasks done, {} failed, {} cortex calls, {} vetoes, {} sleep cycles",
            self.agent_id,
            self.tasks_completed,
            self.tasks_failed,
            self.cortex_invocations,
            self.defence_vetoes,
            self.sleep_cycles,
        )
    }
}

// ── NotableEvent ──────────────────────────────────────────────────────────────

/// An event from the audit stream that warrants explicit operator attention.
///
/// Only high-salience events are surfaced here; routine scheduler dispatches,
/// 1 Hz interoceptive snapshots, and KV-gate passes are counted in aggregate
/// fields on [`ActivityDigest`] but not individually narrated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotableEvent {
    /// Short label (e.g. `"defence_veto"`, `"cortex_fault"`,
    /// `"attention_escalation"`, `"route_modulation"`).
    pub kind: String,
    /// Free-form description with context extracted from the audit entry.
    pub description: String,
}

impl NotableEvent {
    fn defence_veto(detector: &str, reason: &str) -> Self {
        NotableEvent {
            kind: "defence_veto".into(),
            description: format!("{}: {}", detector, reason),
        }
    }

    fn cortex_fault(task_id: &str, error: &str) -> Self {
        NotableEvent {
            kind: "cortex_fault".into(),
            description: format!("task {}: {}", task_id, error),
        }
    }

    fn attention_escalation(veto_count: usize, window_secs: u64) -> Self {
        NotableEvent {
            kind: "attention_escalation".into(),
            description: format!(
                "{} vetoes in {}s window — operator attention required",
                veto_count, window_secs
            ),
        }
    }

    fn route_modulation(reason: &str) -> Self {
        NotableEvent {
            kind: "route_modulation".into(),
            description: reason.to_string(),
        }
    }
}

// ── generate_digest ───────────────────────────────────────────────────────────

/// Generate an activity digest from a slice of audit entries.
///
/// The function folds over `entries` in order, accumulating statistics and
/// extracting high-salience events.  The `agent_id` parameter is used for the
/// digest's own `agent_id` field; entries from other agents (if the log is
/// multi-agent) are still counted if their `agent_id` field matches — callers
/// should pre-filter the slice when a single-agent view is required.
///
/// # Salience rules
///
/// The following entry types contribute to [`ActivityDigest::notable_events`]:
/// - [`AuditEntry::DefenceVeto`]
/// - [`AuditEntry::CortexFault`]
/// - [`AuditEntry::AttentionDemandEscalated`]
/// - [`AuditEntry::RouterModulated`]
///
/// All other entry types update aggregate counters only.
pub fn generate_digest(agent_id: &str, entries: &[AuditEntry]) -> ActivityDigest {
    let mut d = ActivityDigest::new(agent_id);

    for entry in entries {
        match entry {
            AuditEntry::TaskCompleted { tokens_emitted, .. } => {
                d.tasks_completed += 1;
                d.total_tokens_emitted += *tokens_emitted as u64;
            }
            AuditEntry::TaskFailed { .. } => {
                d.tasks_failed += 1;
            }
            AuditEntry::CortexInvoked { .. } => {
                d.cortex_invocations += 1;
            }
            AuditEntry::CortexFault { task_id, error } => {
                d.cortex_faults += 1;
                d.notable_events
                    .push(NotableEvent::cortex_fault(task_id, error));
            }
            AuditEntry::SleepEntered { .. } => {
                d.sleep_cycles += 1;
            }
            AuditEntry::DefenceVeto {
                detector, reason, ..
            } => {
                d.defence_vetoes += 1;
                d.notable_events
                    .push(NotableEvent::defence_veto(detector, reason));
            }
            AuditEntry::AttentionDemandEscalated {
                veto_count,
                window_secs,
                ..
            } => {
                d.attention_escalations += 1;
                d.notable_events.push(NotableEvent::attention_escalation(
                    *veto_count,
                    *window_secs,
                ));
            }
            AuditEntry::GateDecision { invoke, .. } => {
                d.gate_decisions += 1;
                if *invoke {
                    d.gate_invocations += 1;
                } else {
                    d.gate_blocks += 1;
                }
            }
            AuditEntry::RouterModulated { reason, .. } => {
                d.route_modulations += 1;
                d.notable_events
                    .push(NotableEvent::route_modulation(reason));
            }
            _ => {}
        }
    }

    d
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use vita::audit::AuditEntry;

    fn task_completed(agent: &str, id: u64, tokens: u32) -> AuditEntry {
        AuditEntry::TaskCompleted {
            agent_id: agent.to_string(),
            task_id: id,
            tokens_emitted: tokens,
            response: "ok".to_string(),
        }
    }

    fn task_failed(agent: &str, id: u64) -> AuditEntry {
        AuditEntry::TaskFailed {
            agent_id: agent.to_string(),
            task_id: id,
            error: "timeout".to_string(),
        }
    }

    fn cortex_invoked(id: &str) -> AuditEntry {
        AuditEntry::CortexInvoked {
            task_id: id.to_string(),
            latency_to_first_action_ms: 42,
        }
    }

    fn cortex_fault(id: &str, err: &str) -> AuditEntry {
        AuditEntry::CortexFault {
            task_id: id.to_string(),
            error: err.to_string(),
        }
    }

    fn sleep_entered(agent: &str) -> AuditEntry {
        AuditEntry::SleepEntered {
            agent_id: agent.to_string(),
        }
    }

    fn defence_veto(agent: &str, detector: &str, reason: &str) -> AuditEntry {
        AuditEntry::DefenceVeto {
            agent_id: agent.to_string(),
            invocation_id: "inv-1".to_string(),
            detector: detector.to_string(),
            action_blocked: "write /etc/passwd".to_string(),
            reason: reason.to_string(),
        }
    }

    fn attention_escalated(agent: &str, veto_count: usize, window_secs: u64) -> AuditEntry {
        AuditEntry::AttentionDemandEscalated {
            agent_id: agent.to_string(),
            invocation_id: "inv-2".to_string(),
            veto_count,
            window_secs,
        }
    }

    fn gate_decision(agent: &str, event_id: &str, invoke: bool) -> AuditEntry {
        AuditEntry::GateDecision {
            agent_id: agent.to_string(),
            event_id: event_id.to_string(),
            invoke,
            cost_class: if invoke {
                Some("MidTier".to_string())
            } else {
                None
            },
            urgency: 0.7,
            novelty: 0.5,
            user_facing: true,
            semantic_class: "Query".to_string(),
            value_score: 0.6,
            threshold_applied: 0.4,
            thermal_load: 0.1,
            compute_pressure: 0.2,
            memory_pressure: 0.1,
            power_budget: 0.9,
            financial_budget: 0.8,
            attention_demand: 0.5,
            reasoning: "user query".to_string(),
            override_active: false,
        }
    }

    fn router_modulated(agent: &str, event_id: &str, reason: &str) -> AuditEntry {
        AuditEntry::RouterModulated {
            agent_id: agent.to_string(),
            event_id: event_id.to_string(),
            requested_route_id: "frontier".to_string(),
            effective_route_id: "mid-tier".to_string(),
            reason: reason.to_string(),
        }
    }

    #[test]
    fn empty_entries_produce_zero_digest() {
        let d = generate_digest("agent-a", &[]);
        assert_eq!(d.tasks_completed, 0);
        assert_eq!(d.tasks_failed, 0);
        assert_eq!(d.total_tokens_emitted, 0);
        assert_eq!(d.cortex_invocations, 0);
        assert_eq!(d.sleep_cycles, 0);
        assert_eq!(d.defence_vetoes, 0);
        assert!(d.notable_events.is_empty());
    }

    #[test]
    fn digest_counts_completed_tasks_and_accumulates_tokens() {
        let entries = vec![
            task_completed("a", 1, 100),
            task_completed("a", 2, 200),
            task_completed("a", 3, 50),
        ];
        let d = generate_digest("a", &entries);
        assert_eq!(d.tasks_completed, 3);
        assert_eq!(d.total_tokens_emitted, 350);
        assert_eq!(d.tasks_failed, 0);
    }

    #[test]
    fn digest_counts_failed_tasks() {
        let entries = vec![
            task_completed("a", 1, 10),
            task_failed("a", 2),
            task_failed("a", 3),
        ];
        let d = generate_digest("a", &entries);
        assert_eq!(d.tasks_completed, 1);
        assert_eq!(d.tasks_failed, 2);
    }

    #[test]
    fn digest_counts_cortex_invocations_and_faults() {
        let entries = vec![
            cortex_invoked("c1"),
            cortex_invoked("c2"),
            cortex_fault("c3", "process exited"),
        ];
        let d = generate_digest("a", &entries);
        assert_eq!(d.cortex_invocations, 2);
        assert_eq!(d.cortex_faults, 1);
    }

    #[test]
    fn digest_counts_sleep_cycles() {
        let entries = vec![sleep_entered("a"), sleep_entered("a"), sleep_entered("a")];
        let d = generate_digest("a", &entries);
        assert_eq!(d.sleep_cycles, 3);
    }

    #[test]
    fn defence_veto_increments_counter_and_appears_in_notable_events() {
        let entries = vec![defence_veto(
            "a",
            "PromptInjectionDetector",
            "injection detected",
        )];
        let d = generate_digest("a", &entries);
        assert_eq!(d.defence_vetoes, 1);
        assert_eq!(d.notable_events.len(), 1);
        assert_eq!(d.notable_events[0].kind, "defence_veto");
        assert!(d.notable_events[0]
            .description
            .contains("injection detected"));
    }

    #[test]
    fn attention_escalation_increments_counter_and_appears_in_notable_events() {
        let entries = vec![attention_escalated("a", 5, 300)];
        let d = generate_digest("a", &entries);
        assert_eq!(d.attention_escalations, 1);
        assert_eq!(d.notable_events.len(), 1);
        assert_eq!(d.notable_events[0].kind, "attention_escalation");
        assert!(d.notable_events[0].description.contains("5"));
    }

    #[test]
    fn gate_decisions_split_into_invocations_and_blocks() {
        let entries = vec![
            gate_decision("a", "e1", true),
            gate_decision("a", "e2", false),
            gate_decision("a", "e3", true),
        ];
        let d = generate_digest("a", &entries);
        assert_eq!(d.gate_decisions, 3);
        assert_eq!(d.gate_invocations, 2);
        assert_eq!(d.gate_blocks, 1);
    }

    #[test]
    fn route_modulation_increments_counter_and_appears_in_notable_events() {
        let entries = vec![router_modulated("a", "e1", "financial pressure")];
        let d = generate_digest("a", &entries);
        assert_eq!(d.route_modulations, 1);
        assert_eq!(d.notable_events.len(), 1);
        assert_eq!(d.notable_events[0].kind, "route_modulation");
        assert!(d.notable_events[0].description.contains("financial"));
    }

    #[test]
    fn cortex_fault_appears_in_notable_events() {
        let entries = vec![cortex_fault("t1", "OOM")];
        let d = generate_digest("a", &entries);
        assert_eq!(d.cortex_faults, 1);
        assert_eq!(d.notable_events.len(), 1);
        assert_eq!(d.notable_events[0].kind, "cortex_fault");
        assert!(d.notable_events[0].description.contains("OOM"));
    }

    #[test]
    fn notable_events_ordered_by_audit_log_position() {
        let entries = vec![
            defence_veto("a", "D1", "first"),
            cortex_fault("t1", "second"),
            router_modulated("a", "e1", "third"),
        ];
        let d = generate_digest("a", &entries);
        assert_eq!(d.notable_events.len(), 3);
        assert_eq!(d.notable_events[0].kind, "defence_veto");
        assert_eq!(d.notable_events[1].kind, "cortex_fault");
        assert_eq!(d.notable_events[2].kind, "route_modulation");
    }

    #[test]
    fn headline_includes_agent_id_and_key_counts() {
        let entries = vec![
            task_completed("agent-x", 1, 100),
            cortex_invoked("c1"),
            sleep_entered("agent-x"),
        ];
        let d = generate_digest("agent-x", &entries);
        let h = d.headline();
        assert!(h.contains("agent-x"), "headline: {}", h);
        assert!(h.contains("1 tasks"), "headline: {}", h);
        assert!(h.contains("1 cortex"), "headline: {}", h);
        assert!(h.contains("1 sleep"), "headline: {}", h);
    }

    #[test]
    fn digest_is_serialisable_to_json_and_back() {
        let entries = vec![task_completed("a", 1, 50), defence_veto("a", "D", "r")];
        let d = generate_digest("a", &entries);
        let json = serde_json::to_string(&d).expect("serialise");
        let d2: ActivityDigest = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(d, d2);
    }
}
