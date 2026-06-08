//! [`AuditSnapshot`] — a point-in-time view of the agent's observable state,
//! derived from the audit log and used as the common input for all checks.

use vita::audit::AuditEntry;

/// A point-in-time view of the agent's observable state derived from the
/// audit log and any live sensors.
///
/// All diagnostic checks operate on this snapshot rather than taking direct
/// references to subsystem internals, so checks are easily unit-testable with
/// synthetic data.
#[derive(Debug, Default, Clone)]
pub struct AuditSnapshot {
    /// Total number of tasks dispatched.
    pub tasks_dispatched: u64,
    /// Total number of task failures.
    pub task_failures: u64,
    /// Total cortex invocations recorded.
    pub cortex_invocations: u64,
    /// Total cortex faults recorded.
    pub cortex_faults: u64,
    /// Total defence vetoes recorded.
    pub defence_vetoes: u64,
    /// Attention-demand escalations (repeated vetoes crossing the threshold).
    pub attention_escalations: u64,
    /// Sleep cycles completed without error.
    pub sleep_cycles_ok: u64,
    /// Sleep cycles with at least one phase failure.
    pub sleep_cycles_failed: u64,
    /// Count of Critical memory pressure events (L1 context window).
    pub memory_pressure_critical_events: u64,
    /// Most recent L1 token occupancy seen (0 if never reported).
    pub last_l1_tokens: u32,
    /// Configured maximum context size (0 if never reported).
    pub last_l1_max_context: u32,
    /// Count of router-modulation events (interoceptive pressure redirected route).
    pub router_modulations: u64,
    /// Count of KV-cache controller faults (fell back to LRU).
    pub kv_controller_faults: u64,
    /// Count of agent delegations that failed (E16 A2A substrate).
    pub agent_delegation_failures: u64,
    /// Count of consolidation failures (E8 fine-tuning hook).
    pub consolidation_failures: u64,
    /// Current financial budget scalar (from the most recent interoceptive snapshot).
    /// Value in [0.0, 1.0]: 1.0 = full budget remaining, 0.0 = exhausted.
    pub last_financial_budget: f32,
    /// Current thermal load scalar from the most recent interoceptive snapshot.
    /// Value in [0.0, 1.0]: 0.0 = cool, 1.0 = max load.
    pub last_thermal_load: f32,
    /// Current memory pressure scalar from the most recent interoceptive snapshot.
    pub last_memory_pressure: f32,
    /// Total entries in the audit log at snapshot time.
    pub total_audit_entries: u64,
}

impl AuditSnapshot {
    /// Build a snapshot by folding over the full audit log.
    ///
    /// This is O(n) in the number of audit entries and is intended for
    /// offline / periodic diagnostic runs, not hot-path instrumentation.
    pub fn from_audit_log(entries: &[AuditEntry]) -> Self {
        let mut snap = Self {
            total_audit_entries: entries.len() as u64,
            ..Default::default()
        };

        for entry in entries {
            match entry {
                AuditEntry::TaskStarted { .. } => {
                    snap.tasks_dispatched += 1;
                }
                AuditEntry::TaskFailed { .. } => {
                    snap.task_failures += 1;
                }
                AuditEntry::CortexInvoked { .. } => {
                    snap.cortex_invocations += 1;
                }
                AuditEntry::CortexFault { .. } => {
                    snap.cortex_faults += 1;
                }
                AuditEntry::DefenceVeto { .. } => {
                    snap.defence_vetoes += 1;
                }
                AuditEntry::AttentionDemandEscalated { .. } => {
                    snap.attention_escalations += 1;
                }
                AuditEntry::SleepPhaseCompleted { success, .. } => {
                    if *success {
                        snap.sleep_cycles_ok += 1;
                    } else {
                        snap.sleep_cycles_failed += 1;
                    }
                }
                AuditEntry::MemoryPressureEvent {
                    level,
                    active_tokens,
                    max_context,
                    ..
                } => {
                    if level == "Critical" {
                        snap.memory_pressure_critical_events += 1;
                    }
                    snap.last_l1_tokens = *active_tokens;
                    snap.last_l1_max_context = *max_context;
                }
                AuditEntry::RouterModulated { .. } => {
                    snap.router_modulations += 1;
                }
                AuditEntry::KvControllerFaulted { .. } => {
                    snap.kv_controller_faults += 1;
                }
                AuditEntry::AgentDelegationFailed { .. } => {
                    snap.agent_delegation_failures += 1;
                }
                AuditEntry::ConsolidationFailed { .. } => {
                    snap.consolidation_failures += 1;
                }
                AuditEntry::InteroceptiveSnapshot {
                    financial_budget,
                    thermal_load,
                    memory_pressure,
                    ..
                } => {
                    snap.last_financial_budget = *financial_budget;
                    snap.last_thermal_load = *thermal_load;
                    snap.last_memory_pressure = *memory_pressure;
                }
                _ => {}
            }
        }

        snap
    }

    /// Task failure rate as a fraction in `[0.0, 1.0]`.
    /// Returns `0.0` when no tasks have been dispatched.
    pub fn task_failure_rate(&self) -> f32 {
        if self.tasks_dispatched == 0 {
            return 0.0;
        }
        self.task_failures as f32 / self.tasks_dispatched as f32
    }

    /// Cortex fault rate as a fraction in `[0.0, 1.0]`.
    /// Returns `0.0` when no invocations have occurred.
    pub fn cortex_fault_rate(&self) -> f32 {
        if self.cortex_invocations == 0 {
            return 0.0;
        }
        self.cortex_faults as f32 / self.cortex_invocations as f32
    }

    /// Sleep cycle failure rate as a fraction in `[0.0, 1.0]`.
    pub fn sleep_failure_rate(&self) -> f32 {
        let total = self.sleep_cycles_ok + self.sleep_cycles_failed;
        if total == 0 {
            return 0.0;
        }
        self.sleep_cycles_failed as f32 / total as f32
    }

    /// L1 fill fraction in `[0.0, 1.0]`.
    /// Returns `0.0` when max_context is 0 (no data yet).
    pub fn l1_fill_fraction(&self) -> f32 {
        if self.last_l1_max_context == 0 {
            return 0.0;
        }
        self.last_l1_tokens as f32 / self.last_l1_max_context as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entries(variants: &[AuditEntry]) -> Vec<AuditEntry> {
        variants.to_vec()
    }

    #[test]
    fn empty_audit_log_produces_zero_snapshot() {
        let snap = AuditSnapshot::from_audit_log(&[]);
        assert_eq!(snap.tasks_dispatched, 0);
        assert_eq!(snap.task_failures, 0);
        assert_eq!(snap.total_audit_entries, 0);
    }

    #[test]
    fn task_started_increments_dispatched() {
        let entries = make_entries(&[AuditEntry::TaskStarted {
            agent_id: "a".into(),
            task_id: 1,
            tier: 0,
            prompt: "test".into(),
        }]);
        let snap = AuditSnapshot::from_audit_log(&entries);
        assert_eq!(snap.tasks_dispatched, 1);
    }

    #[test]
    fn task_failure_rate_is_correct() {
        let entries = make_entries(&[
            AuditEntry::TaskStarted {
                agent_id: "a".into(),
                task_id: 1,
                tier: 0,
                prompt: "t".into(),
            },
            AuditEntry::TaskFailed {
                agent_id: "a".into(),
                task_id: 1,
                error: "err".into(),
            },
            AuditEntry::TaskStarted {
                agent_id: "a".into(),
                task_id: 2,
                tier: 0,
                prompt: "t".into(),
            },
        ]);
        let snap = AuditSnapshot::from_audit_log(&entries);
        assert_eq!(snap.tasks_dispatched, 2);
        assert_eq!(snap.task_failures, 1);
        // 1 / 2 = 0.5
        assert!((snap.task_failure_rate() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn zero_dispatched_gives_zero_failure_rate() {
        let snap = AuditSnapshot::default();
        assert_eq!(snap.task_failure_rate(), 0.0);
    }

    #[test]
    fn memory_pressure_critical_is_tracked() {
        let entries = make_entries(&[AuditEntry::MemoryPressureEvent {
            agent_id: "a".into(),
            level: "Critical".into(),
            active_tokens: 4000,
            max_context: 4096,
        }]);
        let snap = AuditSnapshot::from_audit_log(&entries);
        assert_eq!(snap.memory_pressure_critical_events, 1);
        assert_eq!(snap.last_l1_tokens, 4000);
        assert_eq!(snap.last_l1_max_context, 4096);
    }

    #[test]
    fn l1_fill_fraction_computed_correctly() {
        let snap = AuditSnapshot {
            last_l1_tokens: 2048,
            last_l1_max_context: 4096,
            ..Default::default()
        };
        let frac = snap.l1_fill_fraction();
        assert!((frac - 0.5).abs() < 1e-6);
    }

    #[test]
    fn sleep_failure_rate_with_mixed_outcomes() {
        let entries: Vec<AuditEntry> = vec![
            AuditEntry::SleepPhaseCompleted {
                agent_id: "a".into(),
                phase: "MemoryPruning".into(),
                success: true,
            },
            AuditEntry::SleepPhaseCompleted {
                agent_id: "a".into(),
                phase: "GenerativeReplay".into(),
                success: false,
            },
        ];
        let snap = AuditSnapshot::from_audit_log(&entries);
        assert_eq!(snap.sleep_cycles_ok, 1);
        assert_eq!(snap.sleep_cycles_failed, 1);
        assert!((snap.sleep_failure_rate() - 0.5).abs() < 1e-6);
    }
}
