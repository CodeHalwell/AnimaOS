//! Autonomic sleep maintenance routines.
//!
//! These stubs model the four canonical sleep responsibilities described in
//! the AnimaOS spec:
//!
//! 1. Memory pruning & decay
//! 2. Generative replay validation
//! 3. Dream exploration
//! 4. Policy compilation

/// The four lifecycle sleep routines.
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

/// Outcome of a single sleep routine run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SleepRoutineOutcome {
    /// Routine that produced this outcome.
    pub routine: SleepRoutine,
    /// True when the routine completed without rollback.
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
    /// Returns true when every routine reported completion.
    pub fn all_completed(&self) -> bool {
        !self.outcomes.is_empty() && self.outcomes.iter().all(|o| o.completed)
    }
}

/// Runs the default sleep maintenance suite (all four routines in order).
pub fn run_default_maintenance() -> SleepMaintenanceReport {
    SleepMaintenanceReport {
        outcomes: vec![
            SleepRoutineOutcome {
                routine: SleepRoutine::MemoryPruning,
                completed: true,
                notes: "decay applied",
            },
            SleepRoutineOutcome {
                routine: SleepRoutine::GenerativeReplay,
                completed: true,
                notes: "replay verified",
            },
            SleepRoutineOutcome {
                routine: SleepRoutine::DreamExploration,
                completed: true,
                notes: "associative edges proposed",
            },
            SleepRoutineOutcome {
                routine: SleepRoutine::PolicyCompilation,
                completed: true,
                notes: "training pairs emitted",
            },
        ],
    }
}

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
}
