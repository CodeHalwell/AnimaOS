//! Autonomic sleep maintenance routines.
//!
//! These routines model the four canonical sleep responsibilities described in
//! the AnimaOS spec:
//!
//! 1. **Memory Pruning** — apply exponential decay and evict below-threshold nodes.
//! 2. **Generative Replay** — validate proposed structural changes against
//!    synthetic queries.
//! 3. **Dream Exploration** — random graph walks to discover latent associative
//!    edges.
//! 4. **Policy Compilation** — compile raw traces into training datasets.
//!
//! # E3.4 additions
//!
//! [`run_maintenance_audited`] wraps each phase with
//! [`AuditEntry::SleepPhaseStarted`] / [`AuditEntry::SleepPhaseCompleted`]
//! entries so that every sleep cycle is traceable end-to-end (exit criterion 1
//! of epic E3.4).

use crate::audit::{AuditEntry, AuditLog};

/// The four lifecycle sleep routines, executed in this order each cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepRoutine {
    /// Apply exponential decay and evict below-threshold nodes.
    MemoryPruning,
    /// Validate proposed structural changes against synthetic queries.
    GenerativeReplay,
    /// Random graph walks to discover latent associative edges.
    DreamExploration,
    /// Compile raw traces into training datasets.
    PolicyCompilation,
}

impl SleepRoutine {
    /// Returns a stable, human-readable name suitable for audit log entries.
    pub fn as_str(self) -> &'static str {
        match self {
            SleepRoutine::MemoryPruning => "MemoryPruning",
            SleepRoutine::GenerativeReplay => "GenerativeReplay",
            SleepRoutine::DreamExploration => "DreamExploration",
            SleepRoutine::PolicyCompilation => "PolicyCompilation",
        }
    }
}

/// Outcome of a single sleep routine run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SleepRoutineOutcome {
    /// Routine that produced this outcome.
    pub routine: SleepRoutine,
    /// `true` when the routine completed without rollback.
    pub completed: bool,
    /// Optional human-readable notes.
    pub notes: &'static str,
}

/// Aggregated report from a sleep maintenance pass.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SleepMaintenanceReport {
    /// Outcomes for every routine that ran, in execution order.
    pub outcomes: Vec<SleepRoutineOutcome>,
}

impl SleepMaintenanceReport {
    /// Returns `true` when every routine reported completion.
    pub fn all_completed(&self) -> bool {
        !self.outcomes.is_empty() && self.outcomes.iter().all(|o| o.completed)
    }
}

/// Runs the default sleep maintenance suite (all four routines in order) and
/// returns the aggregated report.  No audit logging is performed.
///
/// See [`run_maintenance_audited`] for the audit-logging variant used by the
/// somatic execution loop.
pub fn run_default_maintenance() -> SleepMaintenanceReport {
    SleepMaintenanceReport {
        outcomes: PHASES.iter().map(|&r| run_routine(r)).collect(),
    }
}

/// Runs the default sleep maintenance suite, emitting
/// [`AuditEntry::SleepPhaseStarted`] and [`AuditEntry::SleepPhaseCompleted`]
/// entries into `audit` for each phase.
///
/// This satisfies E3.4 exit criterion 1: transitions (including every
/// maintenance phase) are audited end-to-end in the log.
pub fn run_maintenance_audited(agent_id: &str, audit: &mut AuditLog) -> SleepMaintenanceReport {
    let mut outcomes = Vec::with_capacity(PHASES.len());

    for &routine in PHASES {
        let phase = routine.as_str().to_owned();

        audit.push(AuditEntry::SleepPhaseStarted {
            agent_id: agent_id.to_string(),
            phase: phase.clone(),
        });

        let outcome = run_routine(routine);
        let success = outcome.completed;

        audit.push(AuditEntry::SleepPhaseCompleted {
            agent_id: agent_id.to_string(),
            phase,
            success,
        });

        outcomes.push(outcome);
    }

    SleepMaintenanceReport { outcomes }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Canonical phase execution order.
const PHASES: &[SleepRoutine] = &[
    SleepRoutine::MemoryPruning,
    SleepRoutine::GenerativeReplay,
    SleepRoutine::DreamExploration,
    SleepRoutine::PolicyCompilation,
];

/// Executes a single routine and returns its outcome.  The production path
/// will call real memory/replay/dream/compiler subsystems here; for now the
/// stubs always succeed.
fn run_routine(routine: SleepRoutine) -> SleepRoutineOutcome {
    let notes = match routine {
        SleepRoutine::MemoryPruning => "decay applied, floor enforced",
        SleepRoutine::GenerativeReplay => "replay verified, no rollback required",
        SleepRoutine::DreamExploration => "associative edges proposed",
        SleepRoutine::PolicyCompilation => "training pairs emitted",
    };
    SleepRoutineOutcome {
        routine,
        completed: true,
        notes,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_maintenance_runs_all_routines_in_order() {
        let report = run_default_maintenance();
        assert!(report.all_completed());
        let routines: Vec<SleepRoutine> = report.outcomes.iter().map(|o| o.routine).collect();
        assert_eq!(
            routines,
            vec![
                SleepRoutine::MemoryPruning,
                SleepRoutine::GenerativeReplay,
                SleepRoutine::DreamExploration,
                SleepRoutine::PolicyCompilation,
            ]
        );
    }

    #[test]
    fn empty_report_does_not_count_as_complete() {
        let report = SleepMaintenanceReport::default();
        assert!(!report.all_completed());
    }

    #[test]
    fn audited_maintenance_emits_start_and_complete_for_each_phase() {
        let mut audit = AuditLog::new();
        let report = run_maintenance_audited("test-agent", &mut audit);

        assert!(report.all_completed(), "all phases should complete");

        // Expect exactly 8 entries: Start + Complete per phase × 4 phases.
        assert_eq!(audit.len(), 8, "should have 8 audit entries (2 per phase)");

        // Verify ordering: Start then Complete, alternating.
        let entries = audit.entries();
        for (i, routine) in PHASES.iter().enumerate() {
            let start_idx = i * 2;
            let complete_idx = start_idx + 1;

            assert!(
                matches!(
                    &entries[start_idx],
                    AuditEntry::SleepPhaseStarted { phase, .. } if phase == routine.as_str()
                ),
                "entry {start_idx} should be SleepPhaseStarted for {:?}",
                routine
            );
            assert!(
                matches!(
                    &entries[complete_idx],
                    AuditEntry::SleepPhaseCompleted { phase, success: true, .. }
                        if phase == routine.as_str()
                ),
                "entry {complete_idx} should be SleepPhaseCompleted for {:?}",
                routine
            );
        }
    }

    #[test]
    fn audited_maintenance_carries_agent_id_in_every_entry() {
        let mut audit = AuditLog::new();
        run_maintenance_audited("soak-agent", &mut audit);

        for entry in audit.entries() {
            match entry {
                AuditEntry::SleepPhaseStarted { agent_id, .. }
                | AuditEntry::SleepPhaseCompleted { agent_id, .. } => {
                    assert_eq!(agent_id, "soak-agent");
                }
                _ => panic!("unexpected entry type: {entry:?}"),
            }
        }
    }

    #[test]
    fn sleep_routine_as_str_is_stable() {
        assert_eq!(SleepRoutine::MemoryPruning.as_str(), "MemoryPruning");
        assert_eq!(SleepRoutine::GenerativeReplay.as_str(), "GenerativeReplay");
        assert_eq!(SleepRoutine::DreamExploration.as_str(), "DreamExploration");
        assert_eq!(
            SleepRoutine::PolicyCompilation.as_str(),
            "PolicyCompilation"
        );
    }
}
