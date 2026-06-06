//! S15.3 — Decision replay / time-travel debugging.
//!
//! Extends `anima why` from "the last decision" to "any decision, with full
//! context".  A [`DecisionReplayer`] walks the durable audit log to reconstruct
//! the full decision trace for every gate evaluation: inputs (event features,
//! homeostatic signals), the threshold comparison, the route selected, the
//! tools permitted, and the cortex outcome.
//!
//! ## Usage
//!
//! ```rust,no_run
//! use lifecycle::replay::DecisionReplayer;
//!
//! // `entries` is a slice from `AuditLog::entries()` or a loaded JSONL file.
//! let entries: Vec<vita::audit::AuditEntry> = vec![];
//! let replayer = DecisionReplayer::new(&entries);
//!
//! // Replay a specific decision by its event_id.
//! if let Some(trace) = replayer.find_decision("e42") {
//!     println!("gate: invoke={} score={:.2} threshold={:.2}",
//!         trace.gate_invoked, trace.gate_value_score, trace.gate_threshold);
//!     println!("reasoning: {}", trace.gate_reasoning);
//! }
//!
//! // Replay all decisions in the log.
//! for trace in replayer.replay_all() {
//!     println!("{}: {}", trace.event_id, trace.outcome_label());
//! }
//! ```
//!
//! ## Determinism
//!
//! [`DecisionReplayer::replay_all`] iterates the audit log in insertion order.
//! Traces for the same `event_id` accumulate data from all matching entries
//! (gate → router → cortex), so the output order mirrors the order in which
//! gate decisions were first seen.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use vita::audit::AuditEntry;

// ── DecisionTrace ─────────────────────────────────────────────────────────────

/// Full replay trace for a single gate evaluation identified by `event_id`.
///
/// Fields that come from optional downstream entries (router, cortex) are
/// `Option`-wrapped: they will be `None` when the gate blocked the event
/// (`gate_invoked = false`) or when those entries were not yet written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionTrace {
    /// Per-event identifier (matches `GateDecision.event_id` in the audit log).
    pub event_id: String,

    // ── Gate fields (always present when the trace exists) ───────────────────
    /// `true` when the gate decided to invoke the cortex.
    pub gate_invoked: bool,
    /// Composite value score computed from event features + drives.
    pub gate_value_score: f32,
    /// Adaptive threshold the score was compared against.
    pub gate_threshold: f32,
    /// Human-readable gate reasoning string.
    pub gate_reasoning: String,
    /// `true` when a `GateOverride` changed the normal gate outcome.
    pub gate_override_active: bool,
    /// Homeostatic snapshot at gate evaluation time.
    pub homeostatic: HomeostaticSnapshot,

    // ── Router fields (Some when gate_invoked = true and router entry exists) ─
    /// Identifier of the route selected (e.g. `"mid-tier"`).
    pub route_id: Option<String>,
    /// Number of tools the cortex was permitted to use on this route.
    pub tools_permitted: Option<usize>,
    /// Whether the route was modulated by homeostatic pressure.
    pub route_was_modulated: bool,
    /// Reason for route modulation, if any.
    pub modulation_reason: Option<String>,

    // ── Cortex outcome (Some when cortex completed or faulted) ───────────────
    /// `"completed"`, `"faulted"`, or `None` (pending / not yet recorded).
    pub cortex_outcome: Option<String>,
    /// Number of tool calls the cortex made (when completed).
    pub cortex_tool_calls: Option<usize>,
}

impl DecisionTrace {
    /// Short description of the decision outcome for display.
    pub fn outcome_label(&self) -> &str {
        if !self.gate_invoked {
            return "blocked";
        }
        match self.cortex_outcome.as_deref() {
            Some("completed") => "completed",
            Some("faulted") => "faulted",
            _ => "invoked",
        }
    }
}

// ── HomeostaticSnapshot ───────────────────────────────────────────────────────

/// Homeostatic signal values captured at the time of a gate evaluation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HomeostaticSnapshot {
    pub thermal_load: f32,
    pub compute_pressure: f32,
    pub memory_pressure: f32,
    pub power_budget: f32,
    pub financial_budget: f32,
    pub attention_demand: f32,
}

impl Default for HomeostaticSnapshot {
    fn default() -> Self {
        HomeostaticSnapshot {
            thermal_load: 0.0,
            compute_pressure: 0.0,
            memory_pressure: 0.0,
            power_budget: 1.0,
            financial_budget: 1.0,
            attention_demand: 0.0,
        }
    }
}

// ── DecisionReplayer ──────────────────────────────────────────────────────────

/// Replays audit log entries to reconstruct past gate decisions.
///
/// Constructed from a slice of [`AuditEntry`] values; typically loaded from
/// the durable JSONL file at `$ANIMA_AUDIT_DIR/<agent_id>.jsonl` or sourced
/// from [`vita::audit::AuditLog::entries()`].
pub struct DecisionReplayer<'a> {
    entries: &'a [AuditEntry],
}

impl<'a> DecisionReplayer<'a> {
    /// Create a replayer over the given audit entry slice.
    pub fn new(entries: &'a [AuditEntry]) -> Self {
        DecisionReplayer { entries }
    }

    /// Find and reconstruct the decision trace for the given `event_id`.
    ///
    /// Returns `None` when no `GateDecision` entry with a matching `event_id`
    /// exists in the log.  When multiple entries refer to the same `event_id`
    /// (e.g. `GateDecision`, `RouterDecision`, `RouterModulated`, then later
    /// `CortexCompleted`), they are merged into a single trace.
    pub fn find_decision(&self, event_id: &str) -> Option<DecisionTrace> {
        let mut traces = self.build_traces();
        traces.remove(event_id)
    }

    /// Reconstruct all decision traces from the log, in gate-decision order.
    ///
    /// The order of the returned vector matches the order in which
    /// `GateDecision` entries were first encountered in the log.
    pub fn replay_all(&self) -> Vec<DecisionTrace> {
        let mut order: Vec<String> = Vec::new();
        let mut traces = self.build_traces();

        // Collect insertion order by scanning for GateDecision entries.
        for entry in self.entries {
            if let AuditEntry::GateDecision { event_id, .. } = entry {
                if !order.contains(event_id) {
                    order.push(event_id.clone());
                }
            }
        }

        order
            .into_iter()
            .filter_map(|id| traces.remove(&id))
            .collect()
    }

    /// Total number of distinct gate decisions in the log.
    pub fn decision_count(&self) -> usize {
        let mut seen = std::collections::HashSet::new();
        for entry in self.entries {
            if let AuditEntry::GateDecision { event_id, .. } = entry {
                seen.insert(event_id.clone());
            }
        }
        seen.len()
    }

    // ── Internal builders ─────────────────────────────────────────────────────

    fn build_traces(&self) -> HashMap<String, DecisionTrace> {
        let mut traces: HashMap<String, DecisionTrace> = HashMap::new();

        for entry in self.entries {
            match entry {
                AuditEntry::GateDecision {
                    event_id,
                    invoke,
                    value_score,
                    threshold_applied,
                    reasoning,
                    override_active,
                    thermal_load,
                    compute_pressure,
                    memory_pressure,
                    power_budget,
                    financial_budget,
                    attention_demand,
                    ..
                } => {
                    traces.entry(event_id.clone()).or_insert(DecisionTrace {
                        event_id: event_id.clone(),
                        gate_invoked: *invoke,
                        gate_value_score: *value_score,
                        gate_threshold: *threshold_applied,
                        gate_reasoning: reasoning.clone(),
                        gate_override_active: *override_active,
                        homeostatic: HomeostaticSnapshot {
                            thermal_load: *thermal_load,
                            compute_pressure: *compute_pressure,
                            memory_pressure: *memory_pressure,
                            power_budget: *power_budget,
                            financial_budget: *financial_budget,
                            attention_demand: *attention_demand,
                        },
                        route_id: None,
                        tools_permitted: None,
                        route_was_modulated: false,
                        modulation_reason: None,
                        cortex_outcome: None,
                        cortex_tool_calls: None,
                    });
                }

                AuditEntry::RouterDecision {
                    event_id,
                    route_id,
                    tools_permitted,
                    ..
                } => {
                    if let Some(t) = traces.get_mut(event_id) {
                        t.route_id = Some(route_id.clone());
                        t.tools_permitted = Some(*tools_permitted);
                    }
                }

                AuditEntry::RouterModulated {
                    event_id, reason, ..
                } => {
                    if let Some(t) = traces.get_mut(event_id) {
                        t.route_was_modulated = true;
                        t.modulation_reason = Some(reason.clone());
                    }
                }

                AuditEntry::CortexCompleted {
                    task_id,
                    tool_calls,
                    ..
                } => {
                    // CortexCompleted uses task_id, not event_id; match by prefix
                    // convention (task_id == event_id in the somatic loop) or
                    // search all pending traces for those that have no outcome yet.
                    for t in traces.values_mut() {
                        if t.cortex_outcome.is_none()
                            && t.gate_invoked
                            && (t.event_id == *task_id || task_id.starts_with(&t.event_id))
                        {
                            t.cortex_outcome = Some("completed".to_string());
                            t.cortex_tool_calls = Some(*tool_calls);
                            break;
                        }
                    }
                }

                AuditEntry::CortexFault { task_id, .. } => {
                    for t in traces.values_mut() {
                        if t.cortex_outcome.is_none()
                            && t.gate_invoked
                            && (t.event_id == *task_id || task_id.starts_with(&t.event_id))
                        {
                            t.cortex_outcome = Some("faulted".to_string());
                            break;
                        }
                    }
                }

                _ => {}
            }
        }

        traces
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use vita::audit::AuditEntry;

    fn gate(event_id: &str, invoke: bool, score: f32, threshold: f32) -> AuditEntry {
        AuditEntry::GateDecision {
            agent_id: "a".to_string(),
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
            value_score: score,
            threshold_applied: threshold,
            thermal_load: 0.1,
            compute_pressure: 0.2,
            memory_pressure: 0.1,
            power_budget: 0.9,
            financial_budget: 0.8,
            attention_demand: 0.5,
            reasoning: format!("event {}", event_id),
            override_active: false,
        }
    }

    fn router(event_id: &str, route: &str, permitted: usize) -> AuditEntry {
        AuditEntry::RouterDecision {
            agent_id: "a".to_string(),
            event_id: event_id.to_string(),
            route_id: route.to_string(),
            model_selector: "mid".to_string(),
            tool_scope_name: "standard".to_string(),
            tools_available: 5,
            tools_permitted: permitted,
            memory_scope_identity: true,
            memory_scope_l1: true,
            memory_scope_l2: false,
            memory_scope_l3: false,
            max_turns: 10,
            max_tool_calls: 5,
        }
    }

    fn router_modulated(event_id: &str, reason: &str) -> AuditEntry {
        AuditEntry::RouterModulated {
            agent_id: "a".to_string(),
            event_id: event_id.to_string(),
            requested_route_id: "frontier".to_string(),
            effective_route_id: "mid-tier".to_string(),
            reason: reason.to_string(),
        }
    }

    fn cortex_completed(task_id: &str, calls: usize) -> AuditEntry {
        AuditEntry::CortexCompleted {
            task_id: task_id.to_string(),
            tool_calls: calls,
            summary_len: 50,
        }
    }

    fn cortex_fault(task_id: &str) -> AuditEntry {
        AuditEntry::CortexFault {
            task_id: task_id.to_string(),
            error: "OOM".to_string(),
        }
    }

    #[test]
    fn replayer_returns_none_for_unknown_event_id() {
        let entries = vec![gate("e1", true, 0.7, 0.4)];
        let r = DecisionReplayer::new(&entries);
        assert!(r.find_decision("e999").is_none());
    }

    #[test]
    fn replayer_finds_decision_by_event_id() {
        let entries = vec![gate("e1", true, 0.7, 0.4), gate("e2", false, 0.3, 0.4)];
        let r = DecisionReplayer::new(&entries);

        let t = r.find_decision("e1").unwrap();
        assert_eq!(t.event_id, "e1");
        assert!(t.gate_invoked);
        assert!((t.gate_value_score - 0.7).abs() < 1e-6);
        assert!((t.gate_threshold - 0.4).abs() < 1e-6);

        let t2 = r.find_decision("e2").unwrap();
        assert!(!t2.gate_invoked);
    }

    #[test]
    fn replayer_merges_router_decision_into_gate_trace() {
        let entries = vec![gate("e1", true, 0.7, 0.4), router("e1", "mid-tier", 3)];
        let r = DecisionReplayer::new(&entries);
        let t = r.find_decision("e1").unwrap();
        assert_eq!(t.route_id.as_deref(), Some("mid-tier"));
        assert_eq!(t.tools_permitted, Some(3));
    }

    #[test]
    fn replayer_merges_router_modulation_into_trace() {
        let entries = vec![
            gate("e1", true, 0.7, 0.4),
            router("e1", "mid-tier", 3),
            router_modulated("e1", "financial pressure"),
        ];
        let r = DecisionReplayer::new(&entries);
        let t = r.find_decision("e1").unwrap();
        assert!(t.route_was_modulated);
        assert_eq!(t.modulation_reason.as_deref(), Some("financial pressure"));
    }

    #[test]
    fn replayer_merges_cortex_completed_into_trace() {
        let entries = vec![gate("e1", true, 0.7, 0.4), cortex_completed("e1", 2)];
        let r = DecisionReplayer::new(&entries);
        let t = r.find_decision("e1").unwrap();
        assert_eq!(t.cortex_outcome.as_deref(), Some("completed"));
        assert_eq!(t.cortex_tool_calls, Some(2));
    }

    #[test]
    fn replayer_merges_cortex_fault_into_trace() {
        let entries = vec![gate("e1", true, 0.7, 0.4), cortex_fault("e1")];
        let r = DecisionReplayer::new(&entries);
        let t = r.find_decision("e1").unwrap();
        assert_eq!(t.cortex_outcome.as_deref(), Some("faulted"));
    }

    #[test]
    fn replay_all_returns_decisions_in_gate_insertion_order() {
        let entries = vec![
            gate("e1", true, 0.7, 0.4),
            gate("e2", false, 0.2, 0.4),
            gate("e3", true, 0.9, 0.4),
        ];
        let r = DecisionReplayer::new(&entries);
        let traces = r.replay_all();
        assert_eq!(traces.len(), 3);
        assert_eq!(traces[0].event_id, "e1");
        assert_eq!(traces[1].event_id, "e2");
        assert_eq!(traces[2].event_id, "e3");
    }

    #[test]
    fn replay_all_on_empty_log_returns_empty_vec() {
        let r = DecisionReplayer::new(&[]);
        assert!(r.replay_all().is_empty());
    }

    #[test]
    fn decision_count_is_correct() {
        let entries = vec![
            gate("e1", true, 0.7, 0.4),
            gate("e2", false, 0.2, 0.4),
            gate("e1", true, 0.7, 0.4), // duplicate – should not double-count
        ];
        let r = DecisionReplayer::new(&entries);
        assert_eq!(r.decision_count(), 2);
    }

    #[test]
    fn outcome_label_blocked_when_not_invoked() {
        let entries = vec![gate("e1", false, 0.2, 0.4)];
        let r = DecisionReplayer::new(&entries);
        let t = r.find_decision("e1").unwrap();
        assert_eq!(t.outcome_label(), "blocked");
    }

    #[test]
    fn outcome_label_completed_after_cortex_success() {
        let entries = vec![gate("e1", true, 0.7, 0.4), cortex_completed("e1", 1)];
        let r = DecisionReplayer::new(&entries);
        let t = r.find_decision("e1").unwrap();
        assert_eq!(t.outcome_label(), "completed");
    }

    #[test]
    fn homeostatic_snapshot_captured_from_gate_entry() {
        let entries = vec![AuditEntry::GateDecision {
            agent_id: "a".to_string(),
            event_id: "e1".to_string(),
            invoke: true,
            cost_class: Some("MidTier".to_string()),
            urgency: 0.8,
            novelty: 0.6,
            user_facing: true,
            semantic_class: "Q".to_string(),
            value_score: 0.75,
            threshold_applied: 0.4,
            thermal_load: 0.3,
            compute_pressure: 0.4,
            memory_pressure: 0.5,
            power_budget: 0.7,
            financial_budget: 0.6,
            attention_demand: 0.8,
            reasoning: "high urgency".to_string(),
            override_active: false,
        }];
        let r = DecisionReplayer::new(&entries);
        let t = r.find_decision("e1").unwrap();
        assert!((t.homeostatic.thermal_load - 0.3).abs() < 1e-6);
        assert!((t.homeostatic.memory_pressure - 0.5).abs() < 1e-6);
        assert!((t.homeostatic.financial_budget - 0.6).abs() < 1e-6);
    }

    #[test]
    fn trace_is_serialisable_to_json_and_back() {
        let entries = vec![gate("e1", true, 0.7, 0.4), router("e1", "mid-tier", 2)];
        let r = DecisionReplayer::new(&entries);
        let t = r.find_decision("e1").unwrap();
        let json = serde_json::to_string(&t).expect("serialise");
        let t2: DecisionTrace = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(t, t2);
    }
}
