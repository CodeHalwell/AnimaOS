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
//!
//! # E3.5 additions
//!
//! The `MemoryPruning` phase now performs *real* emotional-decay pruning when a
//! [`PruningContext`] is supplied to [`run_maintenance_audited`].  Without a
//! context the phase falls back to a no-op stub so that existing lightweight
//! tests remain fast.
//!
//! A [`PruningContext`] carries:
//! - a mutable borrow of the agent's [`L1PruningStore`],
//! - the elapsed time (seconds) for the decay model, and
//! - an optional floor override (defaults to [`memory::decay::SEMANTIC_FLOOR`]).
//!
//! The outcome struct has been extended with an optional [`PruningReport`]
//! field that callers can inspect to observe the pruning statistics.

use memory::decay::SEMANTIC_FLOOR;
use memory::{L1PruningStore, L3Archive, PruningReport, ReplayConfig, ReplayReport};

use crate::audit::{AuditEntry, AuditLog};

// ── SleepRoutine ──────────────────────────────────────────────────────────────

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

// ── PruningContext ────────────────────────────────────────────────────────────

/// Memory context passed to the `MemoryPruning` sleep phase.
///
/// When supplied to [`run_maintenance_audited`] the phase will call
/// [`L1PruningStore::run_pruning_pass_with`] using `elapsed` and the effective
/// floor (`floor.unwrap_or(SEMANTIC_FLOOR)`).
///
/// # Example
///
/// ```rust,ignore
/// let ctx = PruningContext { l1: &mut lifecycle.l1_memory, elapsed: 1.0, floor: None };
/// sleep::run_maintenance_audited(&agent_id, &mut audit, Some(ctx));
/// ```
pub struct PruningContext<'a> {
    /// L1 episodic memory store to prune during this cycle.
    pub l1: &'a mut L1PruningStore,
    /// Elapsed time (seconds) since nodes were last updated.
    pub elapsed: f32,
    /// Optional floor override; defaults to [`SEMANTIC_FLOOR`] when `None`.
    pub floor: Option<f32>,
}

// ── ReplayContext ─────────────────────────────────────────────────────────────

/// Context passed to the `GenerativeReplay` sleep phase (E3.6).
///
/// When supplied to [`run_maintenance_audited`] the phase will call
/// [`memory::run_replay_validation`] against `l3` using `config`, then
/// populate the [`SleepRoutineOutcome::replay`] field and return any
/// rollback nodes in [`SleepRoutineOutcome::replay_rollback_nodes`].
///
/// The L3 archive is borrowed immutably — rollback re-insertion is performed
/// by the caller after the maintenance pass completes.
pub struct ReplayContext<'a> {
    /// L3 archive to validate against.
    pub l3: &'a L3Archive,
    /// Replay configuration (threshold, sample size, rollback flag).
    pub config: ReplayConfig,
}

// ── SleepRoutineOutcome ───────────────────────────────────────────────────────

/// Outcome of a single sleep routine run.
///
/// The `pruning` field is populated only for the [`SleepRoutine::MemoryPruning`]
/// phase and only when a [`PruningContext`] was supplied.
#[derive(Debug, Clone, PartialEq)]
pub struct SleepRoutineOutcome {
    /// Routine that produced this outcome.
    pub routine: SleepRoutine,
    /// `true` when the routine completed without rollback.
    pub completed: bool,
    /// Optional human-readable notes.
    pub notes: &'static str,
    /// Pruning statistics for the `MemoryPruning` phase; `None` for other phases
    /// or when no [`PruningContext`] was provided.
    pub pruning: Option<PruningReport>,
    /// Nodes evicted during the `MemoryPruning` phase.
    ///
    /// Empty for all other phases and when no [`PruningContext`] was supplied.
    /// Populated so that callers can demote evicted nodes to L3 (E2.6).
    pub evicted_l1_nodes: Vec<(String, memory::MemoryNode)>,
    /// Replay statistics for the `GenerativeReplay` phase; `None` for other
    /// phases or when no [`ReplayContext`] was provided.
    pub replay: Option<ReplayReport>,
    /// Nodes that failed the replay check and should be re-inserted into L1
    /// (rollback).  Empty when rollback was not triggered or was disabled.
    pub replay_rollback_nodes: Vec<(String, memory::MemoryNode)>,
}

// ── SleepMaintenanceReport ────────────────────────────────────────────────────

/// Aggregated report from a sleep maintenance pass.
#[derive(Debug, Clone, PartialEq, Default)]
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

// ── Public API ────────────────────────────────────────────────────────────────

/// Runs the default sleep maintenance suite (all four routines in order) and
/// returns the aggregated report.  No audit logging is performed and no memory
/// pruning context is supplied (stubs only).
///
/// See [`run_maintenance_audited`] for the audit-logging variant used by the
/// somatic execution loop.
pub fn run_default_maintenance() -> SleepMaintenanceReport {
    SleepMaintenanceReport {
        outcomes: PHASES.iter().map(|&r| run_routine_stub(r)).collect(),
    }
}

/// Runs the default sleep maintenance suite, emitting
/// [`AuditEntry::SleepPhaseStarted`] and [`AuditEntry::SleepPhaseCompleted`]
/// entries into `audit` for each phase.
///
/// When `pruning_ctx` is `Some`, the [`SleepRoutine::MemoryPruning`] phase
/// runs real L1 emotional-decay pruning via [`L1PruningStore::run_pruning_pass_with`]
/// (E3.5).  Without a context the phase falls back to a no-op stub.
///
/// This satisfies E3.4 exit criterion 1: transitions (including every
/// maintenance phase) are audited end-to-end in the log.
pub fn run_maintenance_audited(
    agent_id: &str,
    audit: &mut AuditLog,
    mut pruning_ctx: Option<PruningContext<'_>>,
    mut replay_ctx: Option<ReplayContext<'_>>,
) -> SleepMaintenanceReport {
    let mut outcomes = Vec::with_capacity(PHASES.len());

    for &routine in PHASES {
        let phase = routine.as_str().to_owned();

        audit.push(AuditEntry::SleepPhaseStarted {
            agent_id: agent_id.to_string(),
            phase: phase.clone(),
        });

        // The MemoryPruning phase consumes the pruning context (at most once).
        let outcome = if routine == SleepRoutine::MemoryPruning {
            run_pruning_phase(pruning_ctx.take())
        } else if routine == SleepRoutine::GenerativeReplay {
            run_replay_phase(replay_ctx.take())
        } else {
            run_routine_stub(routine)
        };
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

/// Executes the `MemoryPruning` phase.
///
/// When `ctx` is `Some`, runs real L1 pruning via
/// [`L1PruningStore::run_pruning_pass_with`] and embeds the resulting
/// [`PruningReport`] in the outcome.
///
/// When `ctx` is `None`, falls back to a no-op stub.
fn run_pruning_phase(ctx: Option<PruningContext<'_>>) -> SleepRoutineOutcome {
    match ctx {
        Some(c) => {
            let floor = c.floor.unwrap_or(SEMANTIC_FLOOR);
            let (report, evicted) = c.l1.drain_pruned_with(c.elapsed, floor);
            SleepRoutineOutcome {
                routine: SleepRoutine::MemoryPruning,
                completed: true,
                notes: "decay applied, floor enforced",
                pruning: Some(report),
                evicted_l1_nodes: evicted,
                replay: None,
                replay_rollback_nodes: Vec::new(),
            }
        }
        None => SleepRoutineOutcome {
            routine: SleepRoutine::MemoryPruning,
            completed: true,
            notes: "decay applied, floor enforced (no store supplied)",
            pruning: None,
            evicted_l1_nodes: Vec::new(),
            replay: None,
            replay_rollback_nodes: Vec::new(),
        },
    }
}

/// Executes the `GenerativeReplay` phase (E3.6).
///
/// When `ctx` is `Some`, runs [`memory::run_replay_validation`] against the
/// L3 archive and populates the outcome with a [`ReplayReport`] and any
/// rollback nodes.  When `ctx` is `None`, falls back to the no-op stub.
fn run_replay_phase(ctx: Option<ReplayContext<'_>>) -> SleepRoutineOutcome {
    match ctx {
        Some(c) => {
            let (report, rollback_nodes) = memory::run_replay_validation(c.l3, &c.config);
            let notes = if report.triggered_rollback {
                "replay validated, rollback triggered"
            } else {
                "replay verified, no rollback required"
            };
            SleepRoutineOutcome {
                routine: SleepRoutine::GenerativeReplay,
                completed: true,
                notes,
                pruning: None,
                evicted_l1_nodes: Vec::new(),
                replay: Some(report),
                replay_rollback_nodes: rollback_nodes,
            }
        }
        None => run_routine_stub(SleepRoutine::GenerativeReplay),
    }
}

/// Stub execution for non-pruning phases.  The production path will call real
/// replay/dream/compiler subsystems here.
fn run_routine_stub(routine: SleepRoutine) -> SleepRoutineOutcome {
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
        pruning: None,
        evicted_l1_nodes: Vec::new(),
        replay: None,
        replay_rollback_nodes: Vec::new(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use memory::decay::{EmotionalContext, MemoryNode};

    // ── Backward-compatible tests (no pruning context) ────────────────────────

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
        let report = run_maintenance_audited("test-agent", &mut audit, None, None);

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
        run_maintenance_audited("soak-agent", &mut audit, None, None);

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

    // ── E3.5: Pruning phase with real memory context ──────────────────────────

    #[test]
    fn pruning_phase_removes_decayed_nodes_during_sleep() {
        let mut store = L1PruningStore::new();
        store.insert("fast-decay", MemoryNode::new(0.9, 20.0)); // will decay below floor at t=5
        store.insert("stable", MemoryNode::new(0.9, 0.0)); // never decays

        let ctx = PruningContext {
            l1: &mut store,
            elapsed: 5.0,
            floor: None,
        };

        let mut audit = AuditLog::new();
        let report = run_maintenance_audited("test-agent", &mut audit, Some(ctx), None);

        assert!(report.all_completed());

        // The MemoryPruning outcome should carry a populated PruningReport.
        let pruning_outcome = &report.outcomes[0];
        assert_eq!(pruning_outcome.routine, SleepRoutine::MemoryPruning);
        let pr = pruning_outcome
            .pruning
            .as_ref()
            .expect("pruning report must be populated when context is supplied");

        assert_eq!(pr.nodes_before, 2);
        assert_eq!(pr.nodes_removed, 1);
        assert_eq!(pr.nodes_retained(), 1);

        // The store must be in the post-pruned state.
        assert_eq!(
            store.len(),
            1,
            "store should have exactly 1 node after pruning"
        );
        assert!(store.get("stable").is_some());
        assert!(store.get("fast-decay").is_none());
    }

    /// E3.5 exit criterion 1: pruning bounded by configured floor under stress injection.
    #[test]
    fn pruning_bounded_by_floor_under_stress_injection() {
        let mut store = L1PruningStore::new();

        // Stressed node: high arousal keeps activation well above floor even after decay.
        let mut stressed = MemoryNode::new(0.6, 1.0);
        stressed.emotion = EmotionalContext {
            arousal: 4.0,
            surprise: 2.0,
        };
        store.insert("stressed", stressed);

        // Just-above-floor node: activation at t=1 is slightly above SEMANTIC_FLOOR.
        let marginal = MemoryNode::new(SEMANTIC_FLOOR + 0.001, 0.0);
        store.insert("marginal", marginal);

        let ctx = PruningContext {
            l1: &mut store,
            elapsed: 1.0,
            floor: None,
        };

        let mut audit = AuditLog::new();
        let report = run_maintenance_audited("test-agent", &mut audit, Some(ctx), None);

        assert!(report.all_completed());
        let pr = report.outcomes[0]
            .pruning
            .as_ref()
            .expect("pruning report required");

        assert_eq!(
            pr.nodes_removed, 0,
            "no nodes should be pruned when all have activation > floor"
        );
        assert_eq!(store.len(), 2, "both nodes survive when above floor");
    }

    /// E3.5 exit criterion 2: no retained entry has activation below the floor
    /// after a pruning pass via the sleep cycle.
    #[test]
    fn no_retained_node_below_floor_after_sleep_pruning_pass() {
        let mut store = L1PruningStore::new();
        let elapsed = 8.0_f32;

        // Insert a mix of nodes with varying decay rates.
        for i in 0..20u32 {
            let lambda = i as f32 * 0.4;
            store.insert(format!("n{i}"), MemoryNode::new(0.9, lambda));
        }

        let ctx = PruningContext {
            l1: &mut store,
            elapsed,
            floor: None,
        };

        let mut audit = AuditLog::new();
        run_maintenance_audited("invariant-agent", &mut audit, Some(ctx), None);

        // Post-pass: every surviving node must be strictly above SEMANTIC_FLOOR.
        // We verify by re-checking each stored node directly.
        for (key, node) in store.iter() {
            let activation: f32 = node.activation_at(elapsed);
            assert!(
                activation > SEMANTIC_FLOOR,
                "retained node '{key}' has activation {activation:.4} ≤ floor {SEMANTIC_FLOOR:.4}"
            );
        }
    }

    #[test]
    fn pruning_context_with_custom_floor_enforces_higher_threshold() {
        let mut store = L1PruningStore::new();
        // Node with activation between SEMANTIC_FLOOR and 0.5 at t=1.
        // activation_at(1.0) = 0.4 * e^(-0.1) ≈ 0.362 > SEMANTIC_FLOOR (0.3)
        // but < 0.4 threshold.
        store.insert("node", MemoryNode::new(0.4, 0.1));

        let ctx = PruningContext {
            l1: &mut store,
            elapsed: 1.0,
            floor: Some(0.4), // higher than SEMANTIC_FLOOR
        };

        let mut audit = AuditLog::new();
        let report = run_maintenance_audited("agent", &mut audit, Some(ctx), None);

        let pr = report.outcomes[0].pruning.as_ref().unwrap();
        assert_eq!(pr.floor_enforced, 0.4_f32);
        assert_eq!(
            pr.nodes_removed, 1,
            "node below custom floor 0.4 must be pruned"
        );
    }

    #[test]
    fn pruning_report_absent_when_no_context_supplied() {
        let mut audit = AuditLog::new();
        let report = run_maintenance_audited("no-ctx-agent", &mut audit, None, None);

        let outcome = &report.outcomes[0];
        assert_eq!(outcome.routine, SleepRoutine::MemoryPruning);
        assert!(
            outcome.pruning.is_none(),
            "pruning report must be None when no context is supplied"
        );
    }

    // ── E3.6 — Replay validation with rollback ────────────────────────────────

    /// E3.6 exit criterion 2: replay report is logged for every sleep cycle
    /// when a ReplayContext is supplied.
    #[test]
    fn replay_report_is_logged_for_every_cycle_with_context() {
        use memory::{archive_memory_node, L3Archive, MemoryNode, Provenance, SourceTier};

        let path = std::env::temp_dir().join("sleep_replay_logged.json");
        let _ = std::fs::remove_file(&path);

        let mut l3 = L3Archive::open(&path, 4, 100).unwrap();
        // Add a node with a unique embedding so retrieval is perfect.
        let node = MemoryNode::new(0.8, 0.2);
        let item = archive_memory_node(1, "k1", &node);
        let prov = Provenance::now(SourceTier::L1, "k1");
        l3.demote(item, prov).unwrap();

        for _cycle in 0..3 {
            let mut audit = AuditLog::new();
            let replay_ctx = ReplayContext {
                l3: &l3,
                config: memory::ReplayConfig::default(),
            };
            let report = run_maintenance_audited("agent", &mut audit, None, Some(replay_ctx));

            // The GenerativeReplay outcome (index 1) must carry a ReplayReport.
            let replay_outcome = &report.outcomes[1];
            assert_eq!(replay_outcome.routine, SleepRoutine::GenerativeReplay);
            assert!(
                replay_outcome.replay.is_some(),
                "replay report must be populated when ReplayContext is supplied"
            );
        }

        let _ = std::fs::remove_file(&path);
    }

    /// E3.6 exit criterion 1: rollback is triggered when accuracy is below the threshold.
    #[test]
    fn rollback_triggered_in_sleep_phase_when_accuracy_below_threshold() {
        use memory::{archive_memory_node, L3Archive, MemoryNode, Provenance, SourceTier};

        let path = std::env::temp_dir().join("sleep_replay_rollback.json");
        let _ = std::fs::remove_file(&path);

        let mut l3 = L3Archive::open(&path, 4, 100).unwrap();
        // Three nodes with identical embeddings → retrieval always returns ID=1 →
        // accuracy = 1/3 < threshold 0.5 → rollback triggered.
        let node = MemoryNode::new(0.9, 0.1);
        for i in 1..=3 {
            let item = archive_memory_node(i, &format!("key-{i}"), &node);
            let prov = Provenance::now(SourceTier::L1, &format!("key-{i}"));
            l3.demote(item, prov).unwrap();
        }

        let mut audit = AuditLog::new();
        let replay_ctx = ReplayContext {
            l3: &l3,
            config: memory::ReplayConfig {
                accuracy_threshold: 0.5,
                max_sample_size: 16,
                rollback_enabled: true,
            },
        };
        let report = run_maintenance_audited("rollback-agent", &mut audit, None, Some(replay_ctx));

        let replay_outcome = &report.outcomes[1];
        let rr = replay_outcome
            .replay
            .as_ref()
            .expect("replay report must be present");

        assert!(rr.triggered_rollback, "rollback must be triggered");
        assert!(rr.rolled_back > 0);
        assert!(
            !replay_outcome.replay_rollback_nodes.is_empty(),
            "rollback nodes must be returned in the outcome"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Without a ReplayContext, the GenerativeReplay phase runs the no-op stub.
    #[test]
    fn replay_phase_uses_stub_when_no_context_supplied() {
        let mut audit = AuditLog::new();
        let report = run_maintenance_audited("no-replay-agent", &mut audit, None, None);
        let replay_outcome = &report.outcomes[1];
        assert_eq!(replay_outcome.routine, SleepRoutine::GenerativeReplay);
        assert!(
            replay_outcome.replay.is_none(),
            "replay report must be None when no context is supplied"
        );
        assert!(replay_outcome.replay_rollback_nodes.is_empty());
    }
}
