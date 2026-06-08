//! Aggregates [`AuditEntry`] slices into [`AgentMetrics`].
//!
//! [`aggregate`] is a single-pass fold; every entry is examined once and
//! dispatched to the appropriate counter/accumulator field.  Derived values
//! (rates, means) are computed at the end of the pass so the hot path stays
//! branch-free.

use serde::{Deserialize, Serialize};
use vita::audit::AuditEntry;

// ── AgentMetrics ──────────────────────────────────────────────────────────────

/// Structured metrics snapshot derived from a window of [`AuditEntry`] values.
///
/// All counter fields are monotonically non-decreasing within a window.
/// Rate fields (e.g. `task_success_rate`) are derived values in `[0.0, 1.0]`
/// computed from the raw counters; they are `0.0` when the denominator is zero.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentMetrics {
    /// Agent identifier sourced from the first entry in the window that carries
    /// an `agent_id` field, or `"unknown"` when no such entry exists.
    pub agent_id: String,

    /// Total number of audit entries in the window.
    pub window_entries: usize,

    // ── Task metrics ─────────────────────────────────────────────────────────
    /// Number of `TaskStarted` entries.
    pub tasks_started: u64,
    /// Number of `TaskCompleted` entries.
    pub tasks_completed: u64,
    /// Number of `TaskFailed` entries.
    pub tasks_failed: u64,
    /// `tasks_completed / tasks_started`, or `0.0` when no tasks were started.
    pub task_success_rate: f64,
    /// Sum of `tokens_emitted` across all `TaskCompleted` entries.
    pub total_tokens_emitted: u64,

    // ── Gate metrics ─────────────────────────────────────────────────────────
    /// Total `GateDecision` entries evaluated.
    pub gate_decisions: u64,
    /// Gate decisions where `invoke = true`.
    pub gate_invocations: u64,
    /// Gate decisions where `invoke = false`.
    pub gate_blocks: u64,
    /// `gate_invocations / gate_decisions`, or `0.0` when no decisions exist.
    pub gate_invoke_rate: f64,
    /// Invocations routed to `CheapLocal`.
    pub gate_cheap_local: u64,
    /// Invocations routed to `MidTier`.
    pub gate_mid_tier: u64,
    /// Invocations routed to `Frontier`.
    pub gate_frontier: u64,
    /// Gate decisions with `override_active = true`.
    pub gate_overrides: u64,
    /// Sum of `value_score` across all gate decisions (for mean computation).
    pub gate_value_score_sum: f64,
    /// Mean `value_score` across all gate decisions, or `0.0` when none exist.
    pub gate_mean_value_score: f64,

    // ── Router metrics ────────────────────────────────────────────────────────
    /// Number of `RouterModulated` entries (route downgraded under stress).
    pub router_modulations: u64,

    // ── Memory metrics ────────────────────────────────────────────────────────
    /// `MemoryPressureEvent` entries at `Normal` level.
    pub memory_pressure_normal: u64,
    /// `MemoryPressureEvent` entries at `HighWater` level.
    pub memory_pressure_high_water: u64,
    /// `MemoryPressureEvent` entries at `Critical` level.
    pub memory_pressure_critical: u64,
    /// Number of `SleepEntered` entries (proxy for completed sleep cycles).
    pub sleep_cycles: u64,
    /// `SleepPhaseCompleted` entries where `success = true`.
    pub sleep_phases_succeeded: u64,
    /// `SleepPhaseCompleted` entries where `success = false`.
    pub sleep_phases_failed: u64,

    // ── Cortex metrics ────────────────────────────────────────────────────────
    /// Number of `CortexInvoked` entries.
    pub cortex_invocations: u64,
    /// Number of `CortexCompleted` entries.
    pub cortex_completions: u64,
    /// Number of `CortexFault` entries.
    pub cortex_faults: u64,
    /// `cortex_faults / cortex_invocations`, or `0.0` when no invocations exist.
    pub cortex_fault_rate: f64,
    /// Sum of `tool_calls` across all `CortexCompleted` entries.
    pub cortex_total_tool_calls: u64,
    /// Sum of `latency_to_first_action_ms` across all `CortexInvoked` entries.
    pub cortex_latency_sum_ms: u64,
    /// Mean latency from invocation to first tool action (ms), or `0.0` when none.
    pub cortex_mean_latency_ms: f64,

    // ── Defence metrics ───────────────────────────────────────────────────────
    /// Number of `DefenceVeto` entries.
    pub defence_vetoes: u64,
    /// Number of `ConstitutionVeto` entries.
    pub constitution_vetoes: u64,
    /// Number of `AttentionDemandEscalated` entries.
    pub attention_escalations: u64,

    // ── Interoception metrics ─────────────────────────────────────────────────
    /// Number of `InteroceptiveSnapshot` entries.
    pub interoceptive_snapshots: u64,
    /// Running sum of `thermal_load` across snapshots (for mean computation).
    pub thermal_load_sum: f64,
    /// Mean `thermal_load` across snapshots, or `0.0` when none exist.
    pub mean_thermal_load: f64,
    /// Running sum of `memory_pressure` across snapshots.
    pub memory_pressure_sum: f64,
    /// Mean `memory_pressure` across snapshots, or `0.0` when none exist.
    pub mean_memory_pressure: f64,
    /// Running sum of `financial_budget` across snapshots.
    pub financial_budget_sum: f64,
    /// Mean `financial_budget` across snapshots, or `0.0` when none exist.
    pub mean_financial_budget: f64,
}

impl AgentMetrics {
    /// Returns `true` when the metrics window contains no meaningful activity.
    pub fn is_idle(&self) -> bool {
        self.tasks_started == 0
            && self.gate_decisions == 0
            && self.cortex_invocations == 0
            && self.sleep_cycles == 0
    }

    /// One-line summary suitable for a status line or push notification.
    pub fn headline(&self) -> String {
        let task_rate = (self.task_success_rate * 100.0).round() as u32;
        format!(
            "agent={} tasks={}/{} success={}% tokens={} vetoes={}",
            self.agent_id,
            self.tasks_completed,
            self.tasks_started,
            task_rate,
            self.total_tokens_emitted,
            self.defence_vetoes + self.constitution_vetoes,
        )
    }
}

// ── aggregate ─────────────────────────────────────────────────────────────────

/// Aggregate a window of [`AuditEntry`] values into an [`AgentMetrics`] snapshot.
///
/// # Single-pass semantics
///
/// The function performs exactly one linear scan of `entries`.  All derived
/// values (rates, means) are computed after the scan completes.
///
/// # Agent identity
///
/// `agent_id` is taken from the first entry in `entries` that carries the field.
/// If `entries` is empty or none carry the field, `agent_id` is `"unknown"`.
pub fn aggregate(entries: &[AuditEntry]) -> AgentMetrics {
    let mut m = AgentMetrics {
        agent_id: "unknown".to_string(),
        window_entries: entries.len(),
        tasks_started: 0,
        tasks_completed: 0,
        tasks_failed: 0,
        task_success_rate: 0.0,
        total_tokens_emitted: 0,
        gate_decisions: 0,
        gate_invocations: 0,
        gate_blocks: 0,
        gate_invoke_rate: 0.0,
        gate_cheap_local: 0,
        gate_mid_tier: 0,
        gate_frontier: 0,
        gate_overrides: 0,
        gate_value_score_sum: 0.0,
        gate_mean_value_score: 0.0,
        router_modulations: 0,
        memory_pressure_normal: 0,
        memory_pressure_high_water: 0,
        memory_pressure_critical: 0,
        sleep_cycles: 0,
        sleep_phases_succeeded: 0,
        sleep_phases_failed: 0,
        cortex_invocations: 0,
        cortex_completions: 0,
        cortex_faults: 0,
        cortex_fault_rate: 0.0,
        cortex_total_tool_calls: 0,
        cortex_latency_sum_ms: 0,
        cortex_mean_latency_ms: 0.0,
        defence_vetoes: 0,
        constitution_vetoes: 0,
        attention_escalations: 0,
        interoceptive_snapshots: 0,
        thermal_load_sum: 0.0,
        mean_thermal_load: 0.0,
        memory_pressure_sum: 0.0,
        mean_memory_pressure: 0.0,
        financial_budget_sum: 0.0,
        mean_financial_budget: 0.0,
    };

    for entry in entries {
        match entry {
            // ── Task outcomes ─────────────────────────────────────────────────
            AuditEntry::TaskStarted { agent_id, .. } => {
                resolve_agent_id(&mut m.agent_id, agent_id);
                m.tasks_started += 1;
            }
            AuditEntry::TaskCompleted {
                agent_id,
                tokens_emitted,
                ..
            } => {
                resolve_agent_id(&mut m.agent_id, agent_id);
                m.tasks_completed += 1;
                m.total_tokens_emitted += *tokens_emitted as u64;
            }
            AuditEntry::TaskFailed { agent_id, .. } => {
                resolve_agent_id(&mut m.agent_id, agent_id);
                m.tasks_failed += 1;
            }

            // ── Sleep cycle ───────────────────────────────────────────────────
            AuditEntry::SleepEntered { agent_id } => {
                resolve_agent_id(&mut m.agent_id, agent_id);
                m.sleep_cycles += 1;
            }
            AuditEntry::SleepPhaseCompleted {
                agent_id, success, ..
            } => {
                resolve_agent_id(&mut m.agent_id, agent_id);
                if *success {
                    m.sleep_phases_succeeded += 1;
                } else {
                    m.sleep_phases_failed += 1;
                }
            }

            // ── Memory pressure ───────────────────────────────────────────────
            AuditEntry::MemoryPressureEvent {
                agent_id, level, ..
            } => {
                resolve_agent_id(&mut m.agent_id, agent_id);
                match level.as_str() {
                    "Normal" => m.memory_pressure_normal += 1,
                    "HighWater" => m.memory_pressure_high_water += 1,
                    "Critical" => m.memory_pressure_critical += 1,
                    _ => {}
                }
            }

            // ── Cortex ────────────────────────────────────────────────────────
            AuditEntry::CortexInvoked {
                latency_to_first_action_ms,
                ..
            } => {
                m.cortex_invocations += 1;
                m.cortex_latency_sum_ms += latency_to_first_action_ms;
            }
            AuditEntry::CortexCompleted { tool_calls, .. } => {
                m.cortex_completions += 1;
                m.cortex_total_tool_calls += *tool_calls as u64;
            }
            AuditEntry::CortexFault { .. } => {
                m.cortex_faults += 1;
            }

            // ── Gate ─────────────────────────────────────────────────────────
            AuditEntry::GateDecision {
                agent_id,
                invoke,
                cost_class,
                override_active,
                value_score,
                ..
            } => {
                resolve_agent_id(&mut m.agent_id, agent_id);
                m.gate_decisions += 1;
                m.gate_value_score_sum += *value_score as f64;
                if *invoke {
                    m.gate_invocations += 1;
                    match cost_class.as_deref() {
                        Some("CheapLocal") => m.gate_cheap_local += 1,
                        Some("MidTier") => m.gate_mid_tier += 1,
                        Some("Frontier") => m.gate_frontier += 1,
                        _ => {}
                    }
                } else {
                    m.gate_blocks += 1;
                }
                if *override_active {
                    m.gate_overrides += 1;
                }
            }

            // ── Router modulation ─────────────────────────────────────────────
            AuditEntry::RouterModulated { agent_id, .. } => {
                resolve_agent_id(&mut m.agent_id, agent_id);
                m.router_modulations += 1;
            }
            AuditEntry::RouterDecision { agent_id, .. } => {
                resolve_agent_id(&mut m.agent_id, agent_id);
            }

            // ── Defence ───────────────────────────────────────────────────────
            AuditEntry::DefenceVeto { agent_id, .. } => {
                resolve_agent_id(&mut m.agent_id, agent_id);
                m.defence_vetoes += 1;
            }
            AuditEntry::ConstitutionVeto { agent_id, .. } => {
                resolve_agent_id(&mut m.agent_id, agent_id);
                m.constitution_vetoes += 1;
            }
            AuditEntry::AttentionDemandEscalated { agent_id, .. } => {
                resolve_agent_id(&mut m.agent_id, agent_id);
                m.attention_escalations += 1;
            }

            // ── Interoception ─────────────────────────────────────────────────
            AuditEntry::InteroceptiveSnapshot {
                agent_id,
                tick_ns: _,
                thermal_load,
                memory_pressure,
                financial_budget,
                ..
            } => {
                resolve_agent_id(&mut m.agent_id, agent_id);
                m.interoceptive_snapshots += 1;
                m.thermal_load_sum += *thermal_load as f64;
                m.memory_pressure_sum += *memory_pressure as f64;
                m.financial_budget_sum += *financial_budget as f64;
            }

            // All other variants are counted in `window_entries` but not
            // individually tracked — future epics may add more counters here.
            _ => {}
        }
    }

    // ── Derived values ────────────────────────────────────────────────────────
    if m.tasks_started > 0 {
        m.task_success_rate = m.tasks_completed as f64 / m.tasks_started as f64;
    }
    if m.gate_decisions > 0 {
        m.gate_invoke_rate = m.gate_invocations as f64 / m.gate_decisions as f64;
        m.gate_mean_value_score = m.gate_value_score_sum / m.gate_decisions as f64;
    }
    if m.cortex_invocations > 0 {
        m.cortex_fault_rate = m.cortex_faults as f64 / m.cortex_invocations as f64;
        m.cortex_mean_latency_ms = m.cortex_latency_sum_ms as f64 / m.cortex_invocations as f64;
    }
    if m.interoceptive_snapshots > 0 {
        let n = m.interoceptive_snapshots as f64;
        m.mean_thermal_load = m.thermal_load_sum / n;
        m.mean_memory_pressure = m.memory_pressure_sum / n;
        m.mean_financial_budget = m.financial_budget_sum / n;
    }

    m
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Sets `target` to `candidate` on the first call where `target == "unknown"`.
fn resolve_agent_id(target: &mut String, candidate: &str) {
    if target == "unknown" && !candidate.is_empty() {
        *target = candidate.to_string();
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use vita::audit::AuditEntry;

    fn make_task_started(agent: &str, id: u64) -> AuditEntry {
        AuditEntry::TaskStarted {
            agent_id: agent.to_string(),
            task_id: id,
            tier: 0,
            prompt: "test".to_string(),
        }
    }

    fn make_task_completed(agent: &str, id: u64, tokens: u32) -> AuditEntry {
        AuditEntry::TaskCompleted {
            agent_id: agent.to_string(),
            task_id: id,
            tokens_emitted: tokens,
            response: "ok".to_string(),
        }
    }

    fn make_task_failed(agent: &str, id: u64) -> AuditEntry {
        AuditEntry::TaskFailed {
            agent_id: agent.to_string(),
            task_id: id,
            error: "boom".to_string(),
        }
    }

    fn make_gate_decision(
        agent: &str,
        invoke: bool,
        cost_class: Option<&str>,
        value_score: f32,
        override_active: bool,
    ) -> AuditEntry {
        AuditEntry::GateDecision {
            agent_id: agent.to_string(),
            event_id: "ev1".to_string(),
            invoke,
            cost_class: cost_class.map(|s| s.to_string()),
            urgency: 0.5,
            novelty: 0.5,
            user_facing: false,
            semantic_class: "Task".to_string(),
            value_score,
            threshold_applied: 0.4,
            thermal_load: 0.0,
            compute_pressure: 0.0,
            memory_pressure: 0.0,
            power_budget: 1.0,
            financial_budget: 1.0,
            attention_demand: 0.0,
            reasoning: "test".to_string(),
            override_active,
        }
    }

    fn make_sleep_phase_completed(agent: &str, phase: &str, success: bool) -> AuditEntry {
        AuditEntry::SleepPhaseCompleted {
            agent_id: agent.to_string(),
            phase: phase.to_string(),
            success,
        }
    }

    #[test]
    fn empty_slice_produces_zero_metrics() {
        let m = aggregate(&[]);
        assert_eq!(m.window_entries, 0);
        assert_eq!(m.tasks_started, 0);
        assert_eq!(m.agent_id, "unknown");
        assert!(m.is_idle());
    }

    #[test]
    fn agent_id_resolved_from_first_carrying_entry() {
        let entries = vec![make_task_started("alpha", 1)];
        let m = aggregate(&entries);
        assert_eq!(m.agent_id, "alpha");
    }

    #[test]
    fn task_counters_are_correct() {
        let entries = vec![
            make_task_started("a", 1),
            make_task_started("a", 2),
            make_task_completed("a", 1, 100),
            make_task_failed("a", 2),
        ];
        let m = aggregate(&entries);
        assert_eq!(m.tasks_started, 2);
        assert_eq!(m.tasks_completed, 1);
        assert_eq!(m.tasks_failed, 1);
        assert_eq!(m.total_tokens_emitted, 100);
    }

    #[test]
    fn task_success_rate_is_correct() {
        let entries = vec![
            make_task_started("a", 1),
            make_task_started("a", 2),
            make_task_started("a", 3),
            make_task_completed("a", 1, 0),
            make_task_completed("a", 2, 0),
        ];
        let m = aggregate(&entries);
        // 2 completed out of 3 started = 2/3 ≈ 0.666
        assert!((m.task_success_rate - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn task_success_rate_is_zero_when_no_tasks_started() {
        let m = aggregate(&[]);
        assert_eq!(m.task_success_rate, 0.0);
    }

    #[test]
    fn gate_invoke_and_block_counters() {
        let entries = vec![
            make_gate_decision("a", true, Some("CheapLocal"), 0.6, false),
            make_gate_decision("a", true, Some("MidTier"), 0.7, false),
            make_gate_decision("a", true, Some("Frontier"), 0.9, true),
            make_gate_decision("a", false, None, 0.2, false),
        ];
        let m = aggregate(&entries);
        assert_eq!(m.gate_decisions, 4);
        assert_eq!(m.gate_invocations, 3);
        assert_eq!(m.gate_blocks, 1);
        assert_eq!(m.gate_cheap_local, 1);
        assert_eq!(m.gate_mid_tier, 1);
        assert_eq!(m.gate_frontier, 1);
        assert_eq!(m.gate_overrides, 1);
    }

    #[test]
    fn gate_invoke_rate_is_correct() {
        let entries = vec![
            make_gate_decision("a", true, Some("MidTier"), 0.5, false),
            make_gate_decision("a", true, Some("MidTier"), 0.5, false),
            make_gate_decision("a", false, None, 0.1, false),
            make_gate_decision("a", false, None, 0.1, false),
        ];
        let m = aggregate(&entries);
        assert!((m.gate_invoke_rate - 0.5).abs() < 1e-9);
    }

    #[test]
    fn gate_mean_value_score_is_correct() {
        let entries = vec![
            make_gate_decision("a", true, Some("MidTier"), 0.4, false),
            make_gate_decision("a", false, None, 0.6, false),
        ];
        let m = aggregate(&entries);
        assert!((m.gate_mean_value_score - 0.5).abs() < 1e-6);
    }

    #[test]
    fn sleep_cycle_and_phase_counters() {
        let entries = vec![
            AuditEntry::SleepEntered {
                agent_id: "a".to_string(),
            },
            make_sleep_phase_completed("a", "MemoryPruning", true),
            make_sleep_phase_completed("a", "GenerativeReplay", false),
            make_sleep_phase_completed("a", "DreamExploration", true),
        ];
        let m = aggregate(&entries);
        assert_eq!(m.sleep_cycles, 1);
        assert_eq!(m.sleep_phases_succeeded, 2);
        assert_eq!(m.sleep_phases_failed, 1);
    }

    #[test]
    fn cortex_counters_and_derived_values() {
        let entries = vec![
            AuditEntry::CortexInvoked {
                task_id: "t1".to_string(),
                latency_to_first_action_ms: 200,
            },
            AuditEntry::CortexInvoked {
                task_id: "t2".to_string(),
                latency_to_first_action_ms: 400,
            },
            AuditEntry::CortexCompleted {
                task_id: "t1".to_string(),
                tool_calls: 3,
                summary_len: 100,
            },
            AuditEntry::CortexFault {
                task_id: "t2".to_string(),
                error: "crash".to_string(),
            },
        ];
        let m = aggregate(&entries);
        assert_eq!(m.cortex_invocations, 2);
        assert_eq!(m.cortex_completions, 1);
        assert_eq!(m.cortex_faults, 1);
        assert!((m.cortex_fault_rate - 0.5).abs() < 1e-9);
        assert_eq!(m.cortex_total_tool_calls, 3);
        assert!((m.cortex_mean_latency_ms - 300.0).abs() < 1e-9);
    }

    #[test]
    fn defence_counters_are_correct() {
        let entries = vec![
            AuditEntry::DefenceVeto {
                agent_id: "a".to_string(),
                invocation_id: "inv1".to_string(),
                detector: "InjectionDetector".to_string(),
                action_blocked: "cmd".to_string(),
                reason: "injection".to_string(),
            },
            AuditEntry::ConstitutionVeto {
                agent_id: "a".to_string(),
                invocation_id: "inv2".to_string(),
                prohibition_id: "P1".to_string(),
                clause_text: "No deception".to_string(),
                action_blocked: "lie".to_string(),
                proposal_type: "CortexAction".to_string(),
            },
            AuditEntry::AttentionDemandEscalated {
                agent_id: "a".to_string(),
                invocation_id: "inv3".to_string(),
                veto_count: 5,
                window_secs: 60,
            },
        ];
        let m = aggregate(&entries);
        assert_eq!(m.defence_vetoes, 1);
        assert_eq!(m.constitution_vetoes, 1);
        assert_eq!(m.attention_escalations, 1);
    }

    #[test]
    fn memory_pressure_levels_counted_separately() {
        let make_pressure = |level: &str| AuditEntry::MemoryPressureEvent {
            agent_id: "a".to_string(),
            level: level.to_string(),
            active_tokens: 1000,
            max_context: 4000,
        };
        let entries = vec![
            make_pressure("Normal"),
            make_pressure("HighWater"),
            make_pressure("HighWater"),
            make_pressure("Critical"),
        ];
        let m = aggregate(&entries);
        assert_eq!(m.memory_pressure_normal, 1);
        assert_eq!(m.memory_pressure_high_water, 2);
        assert_eq!(m.memory_pressure_critical, 1);
    }

    #[test]
    fn interoceptive_means_computed_correctly() {
        let make_snapshot =
            |thermal: f32, memory: f32, financial: f32| AuditEntry::InteroceptiveSnapshot {
                agent_id: "a".to_string(),
                tick_ns: 0,
                thermal_load: thermal,
                compute_pressure: 0.0,
                memory_pressure: memory,
                power_budget: 1.0,
                financial_budget: financial,
                attention_demand: 0.0,
                aggregate_stress: 0.0,
            };
        let entries = vec![make_snapshot(0.2, 0.4, 0.8), make_snapshot(0.4, 0.6, 0.6)];
        let m = aggregate(&entries);
        assert_eq!(m.interoceptive_snapshots, 2);
        assert!((m.mean_thermal_load - 0.3).abs() < 1e-6);
        assert!((m.mean_memory_pressure - 0.5).abs() < 1e-6);
        assert!((m.mean_financial_budget - 0.7).abs() < 1e-6);
    }

    #[test]
    fn headline_contains_key_fields() {
        let entries = vec![
            make_task_started("myagent", 1),
            make_task_completed("myagent", 1, 50),
        ];
        let m = aggregate(&entries);
        let h = m.headline();
        assert!(h.contains("myagent"));
        assert!(h.contains("tasks=1/1"));
        assert!(h.contains("success=100%"));
        assert!(h.contains("tokens=50"));
    }

    #[test]
    fn is_idle_true_on_empty_window() {
        assert!(aggregate(&[]).is_idle());
    }

    #[test]
    fn is_idle_false_after_task() {
        let entries = vec![make_task_started("a", 1)];
        assert!(!aggregate(&entries).is_idle());
    }

    #[test]
    fn router_modulation_counter() {
        let entries = vec![AuditEntry::RouterModulated {
            agent_id: "a".to_string(),
            event_id: "ev1".to_string(),
            requested_route_id: "frontier".to_string(),
            effective_route_id: "mid-tier".to_string(),
            reason: "financial pressure".to_string(),
        }];
        let m = aggregate(&entries);
        assert_eq!(m.router_modulations, 1);
    }

    #[test]
    fn total_tokens_accumulate_across_tasks() {
        let entries = vec![
            make_task_started("a", 1),
            make_task_started("a", 2),
            make_task_started("a", 3),
            make_task_completed("a", 1, 100),
            make_task_completed("a", 2, 200),
            make_task_completed("a", 3, 150),
        ];
        let m = aggregate(&entries);
        assert_eq!(m.total_tokens_emitted, 450);
    }

    #[test]
    fn window_entries_matches_slice_length() {
        let entries: Vec<AuditEntry> = (0..7).map(|i| make_task_started("a", i)).collect();
        let m = aggregate(&entries);
        assert_eq!(m.window_entries, 7);
    }

    #[test]
    fn metrics_json_round_trips() {
        let entries = vec![make_task_started("a", 1), make_task_completed("a", 1, 42)];
        let m = aggregate(&entries);
        let json = serde_json::to_string(&m).expect("serialize");
        let m2: AgentMetrics = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(m, m2);
    }
}
