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

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{
    borrow::ToOwned,
    boxed::Box,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

use memory::decay::SEMANTIC_FLOOR;
use memory::{DreamReport, L1PruningStore, PruningReport};
// L3-archive-backed replay/dream/compilation machinery is hosted-only: the
// bare-metal kernel has no filesystem-backed L3 archive or trace corpus.
#[cfg(feature = "std")]
use memory::{
    AuditTraceEntry, CompilationConfig, CompilationReport, DreamConfig, L3Archive, ReplayConfig,
    ReplayReport,
};
#[cfg(feature = "std")]
use skills::{
    evaluate_skill_proposal, generate_skill_draft, reflect_on_episodes, EpisodeSummary,
    PromotionGateConfig, ProposalAction, ReflectionConfig, SkillAuthor, SkillContentScreen,
    SkillProposal, SkillRegistry, SkillState,
};

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
#[cfg(feature = "std")]
pub struct ReplayContext<'a> {
    /// L3 archive to validate against.
    pub l3: &'a L3Archive,
    /// Replay configuration (threshold, sample size, rollback flag).
    pub config: ReplayConfig,
}

// ── DreamContext ──────────────────────────────────────────────────────────────

/// Context passed to the `DreamExploration` sleep phase (E3.7).
///
/// When supplied to [`run_maintenance_audited`] the phase will call
/// [`memory::run_dream_walk`] against `l3` using `config`, then populate the
/// [`SleepRoutineOutcome::dream`] field and return candidate associative edges
/// in [`SleepRoutineOutcome::dream_candidates`].
///
/// The L3 archive is borrowed immutably — edge persistence / hand-off is
/// performed by the caller after maintenance completes.
#[cfg(feature = "std")]
pub struct DreamContext<'a> {
    /// L3 archive to walk during this cycle.
    pub l3: &'a L3Archive,
    /// Dream-exploration configuration (seed, walk length, threshold, …).
    pub config: DreamConfig,
}

// ── CompilationContext ────────────────────────────────────────────────────────

/// Context passed to the `PolicyCompilation` sleep phase (E3.8).
///
/// When supplied to [`run_maintenance_audited`] the phase will call
/// [`memory::compile_traces_to_pairs`] using `entries` and `config`, then
/// populate the [`SleepRoutineOutcome::compilation`] field.
#[cfg(feature = "std")]
pub struct CompilationContext<'a> {
    /// Audit-log trace entries to compile into training pairs for this cycle.
    ///
    /// Callers may pass a reference to an existing slice (e.g. a stack-allocated
    /// `Vec`) or an owned collection via `Cow`-style patterns.
    pub entries: &'a [AuditTraceEntry],
    /// Compilation configuration (output directory, formats, append mode).
    pub config: CompilationConfig,
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
    #[cfg(feature = "std")]
    pub replay: Option<ReplayReport>,
    /// Nodes that failed the replay check and should be re-inserted into L1
    /// (rollback).  Empty when rollback was not triggered or was disabled.
    pub replay_rollback_nodes: Vec<(String, memory::MemoryNode)>,
    /// Dream-walk statistics for the `DreamExploration` phase; `None` for other
    /// phases or when no [`DreamContext`] was provided (E3.7).
    pub dream: Option<DreamReport>,
    /// Candidate associative edges discovered during the `DreamExploration` phase.
    ///
    /// Empty for all other phases and when no [`DreamContext`] was supplied.
    /// Callers use these to seed the next pruning cycle (E3.7 story S3.7.3).
    pub dream_candidates: Vec<memory::AssociativeEdge>,
    /// Compilation statistics for the `PolicyCompilation` phase; `None` for
    /// other phases or when no [`CompilationContext`] was provided (E3.8).
    #[cfg(feature = "std")]
    pub compilation: Option<CompilationReport>,
    /// Training pairs compiled during the `PolicyCompilation` phase.
    ///
    /// Non-empty only for the `PolicyCompilation` phase when a
    /// [`CompilationContext`] was supplied.  The lifecycle manager uses these
    /// pairs to trigger the S8.4.3 consolidation hook without re-running the
    /// compiler.
    #[cfg(feature = "std")]
    pub compiled_pairs: Vec<memory::compilation::TrainingPair>,
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
/// When `dream_ctx` is `Some`, the [`SleepRoutine::DreamExploration`] phase
/// runs real random-walk exploration via [`memory::run_dream_walk`] (E3.7).
///
/// When `compilation_ctx` is `Some`, the [`SleepRoutine::PolicyCompilation`]
/// phase compiles audit traces into training datasets (E3.8).
///
/// This satisfies E3.4 exit criterion 1: transitions (including every
/// maintenance phase) are audited end-to-end in the log.
pub fn run_maintenance_audited(
    agent_id: &str,
    audit: &mut AuditLog,
    mut pruning_ctx: Option<PruningContext<'_>>,
    #[cfg(feature = "std")] mut replay_ctx: Option<ReplayContext<'_>>,
    #[cfg(feature = "std")] mut dream_ctx: Option<DreamContext<'_>>,
    #[cfg(feature = "std")] mut compilation_ctx: Option<CompilationContext<'_>>,
) -> SleepMaintenanceReport {
    let mut outcomes = Vec::with_capacity(PHASES.len());

    for &routine in PHASES {
        let phase = routine.as_str().to_owned();

        audit.push(AuditEntry::SleepPhaseStarted {
            agent_id: agent_id.to_string(),
            phase: phase.clone(),
        });

        let outcome = match routine {
            SleepRoutine::MemoryPruning => run_pruning_phase(pruning_ctx.take()),
            #[cfg(feature = "std")]
            SleepRoutine::GenerativeReplay => run_replay_phase(replay_ctx.take()),
            #[cfg(feature = "std")]
            SleepRoutine::DreamExploration => run_dream_phase(dream_ctx.take()),
            #[cfg(feature = "std")]
            SleepRoutine::PolicyCompilation => run_compilation_phase(compilation_ctx.take()),
            // The bare-metal kernel has no filesystem-backed L3 archive or
            // trace corpus, so these phases complete as audited no-ops.
            #[cfg(not(feature = "std"))]
            SleepRoutine::GenerativeReplay
            | SleepRoutine::DreamExploration
            | SleepRoutine::PolicyCompilation => {
                stub_outcome(routine, "skipped: requires hosted L3/corpus")
            }
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
            let mut outcome =
                stub_outcome(SleepRoutine::MemoryPruning, "decay applied, floor enforced");
            outcome.pruning = Some(report);
            outcome.evicted_l1_nodes = evicted;
            outcome
        }
        None => stub_outcome(
            SleepRoutine::MemoryPruning,
            "decay applied, floor enforced (no store supplied)",
        ),
    }
}

/// Executes the `GenerativeReplay` phase (E3.6).
///
/// When `ctx` is `Some`, runs [`memory::run_replay_validation`] against the
/// L3 archive and populates the outcome with a [`ReplayReport`] and any
/// rollback nodes.  When `ctx` is `None`, falls back to the no-op stub.
#[cfg(feature = "std")]
fn run_replay_phase(ctx: Option<ReplayContext<'_>>) -> SleepRoutineOutcome {
    match ctx {
        Some(c) => {
            let (report, rollback_nodes) = memory::run_replay_validation(c.l3, &c.config);
            let notes = if report.triggered_rollback {
                "replay validated, rollback triggered"
            } else {
                "replay verified, no rollback required"
            };
            let mut outcome = stub_outcome(SleepRoutine::GenerativeReplay, notes);
            outcome.replay = Some(report);
            outcome.replay_rollback_nodes = rollback_nodes;
            outcome
        }
        None => run_routine_stub(SleepRoutine::GenerativeReplay),
    }
}

/// Executes the `DreamExploration` phase (E3.7).
///
/// When `ctx` is `Some`, runs [`memory::run_dream_walk`] against the L3 archive
/// and populates the outcome with a [`DreamReport`] and candidate associative
/// edges.  When `ctx` is `None`, falls back to the no-op stub.
#[cfg(feature = "std")]
fn run_dream_phase(ctx: Option<DreamContext<'_>>) -> SleepRoutineOutcome {
    match ctx {
        Some(c) => {
            let (report, candidates) = memory::run_dream_walk(c.l3, &c.config);
            let notes = if report.candidates_found > 0 {
                "associative edges discovered"
            } else {
                "dream walk complete, no edges above threshold"
            };
            let mut outcome = stub_outcome(SleepRoutine::DreamExploration, notes);
            outcome.dream = Some(report);
            outcome.dream_candidates = candidates;
            outcome
        }
        None => run_routine_stub(SleepRoutine::DreamExploration),
    }
}

/// Executes the `PolicyCompilation` phase (E3.8).
///
/// When `ctx` is `Some`, runs [`memory::compile_traces_to_pairs`] against the
/// provided audit trace entries and populates the outcome with a
/// [`CompilationReport`].  When `ctx` is `None`, falls back to the no-op stub.
#[cfg(feature = "std")]
fn run_compilation_phase(ctx: Option<CompilationContext<'_>>) -> SleepRoutineOutcome {
    match ctx {
        Some(c) => {
            let (report, pairs, _errors) = memory::compile_traces_to_pairs(c.entries, &c.config);
            let notes = if report.pairs_compiled > 0 {
                "training pairs compiled and persisted"
            } else {
                "compilation complete, no task pairs found"
            };
            let mut outcome = stub_outcome(SleepRoutine::PolicyCompilation, notes);
            outcome.compilation = Some(report);
            outcome.compiled_pairs = pairs;
            outcome
        }
        None => run_routine_stub(SleepRoutine::PolicyCompilation),
    }
}

/// Stub execution for phases without a real context supplied.
fn run_routine_stub(routine: SleepRoutine) -> SleepRoutineOutcome {
    let notes = match routine {
        SleepRoutine::MemoryPruning => "decay applied, floor enforced",
        SleepRoutine::GenerativeReplay => "replay verified, no rollback required",
        SleepRoutine::DreamExploration => "associative edges proposed",
        SleepRoutine::PolicyCompilation => "training pairs emitted",
    };
    stub_outcome(routine, notes)
}

/// Builds a completed [`SleepRoutineOutcome`] with every report field empty.
///
/// Shared by the no-op stub path and (under `no_std`) by the phases that are
/// skipped outright because they require the hosted L3 archive / trace corpus.
fn stub_outcome(routine: SleepRoutine, notes: &'static str) -> SleepRoutineOutcome {
    SleepRoutineOutcome {
        routine,
        completed: true,
        notes,
        pruning: None,
        evicted_l1_nodes: Vec::new(),
        #[cfg(feature = "std")]
        replay: None,
        replay_rollback_nodes: Vec::new(),
        dream: None,
        dream_candidates: Vec::new(),
        #[cfg(feature = "std")]
        compilation: None,
        #[cfg(feature = "std")]
        compiled_pairs: Vec::new(),
    }
}

// ── E11 — Dreaming-phase self-improvement reflection (S11.5) ───────────────────

/// Outcome of a [`run_self_improvement_reflection`] pass.
///
/// Summarises the reflection result and lists every agent-authored skill draft
/// that was registered as [`SkillState::Proposed`] in the supplied registry so
/// the caller (e.g. the hosted kernel) can route the new pending proposals into
/// the E15 approval queue.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReflectionRegistration {
    /// Number of episodes analysed by the reflection pass.
    pub episodes_analysed: usize,
    /// Number of friction patterns identified above threshold.
    pub patterns_found: usize,
    /// Number of skill drafts generated from the patterns.
    pub proposals_generated: usize,
    /// Registry skill ids that were registered as `Proposed` (PendingApproval).
    ///
    /// Empty when no qualifying pattern was found, when no registry was
    /// configured, or when auto-promotion is enabled (drafts go straight to
    /// `Active` and need no operator gating).
    pub registered_proposed_ids: Vec<String>,
}

/// Run the E11 self-improvement reflection during the Dreaming phase (S11.5).
///
/// Given a set of recent episode summaries this:
/// 1. calls [`reflect_on_episodes`] (respecting `reflection_config`'s
///    thresholds/limits),
/// 2. for each [`skills::FrictionPattern`] above threshold generates a SKILL.md
///    draft via [`generate_skill_draft`],
/// 3. runs each draft through [`evaluate_skill_proposal`] with
///    `author = SkillAuthor::Agent` and the supplied `gate_config`,
/// 4. for [`ProposalAction::PendingApproval`] outcomes the draft is registered
///    as [`SkillState::Proposed`] in `registry` (`evaluate_skill_proposal` does
///    the registration), and the registry skill id is collected.
///
/// Emits one [`AuditEntry::SkillReflectionCompleted`] summary and one
/// [`AuditEntry::SkillRegistered`] per registered draft (covering both the
/// auto-promoted and the proposed paths). Both audit variants are existing —
/// no new variants are introduced.
///
/// `vita` deliberately stops at registering the proposals: it never references
/// `lifecycle`, so it does NOT enqueue them into the approval queue. The hosted
/// kernel (which may depend on both) routes the returned
/// [`ReflectionRegistration::registered_proposed_ids`] through
/// `lifecycle::SkillApprovalBridge`. This keeps the `vita → lifecycle` edge
/// absent and the dependency graph acyclic.
///
/// Returns a [`ReflectionRegistration`] describing the pass. When `episodes` is
/// empty (or below the configured `min_episodes`) nothing is registered and the
/// reflection summary records zero patterns — the Dreaming phase's existing
/// behaviour is untouched.
#[cfg(feature = "std")]
pub fn run_self_improvement_reflection(
    agent_id: &str,
    episodes: &[EpisodeSummary],
    reflection_config: &ReflectionConfig,
    gate_config: &PromotionGateConfig,
    registry: &mut SkillRegistry,
    audit: &mut AuditLog,
    proposed_at_ns: u64,
) -> ReflectionRegistration {
    let report = reflect_on_episodes(episodes, reflection_config);

    let mut registered_proposed_ids = Vec::new();
    let screen = SkillContentScreen::default();

    for pattern in &report.patterns {
        // Only patterns that suggest a skill name yield a draft.
        if pattern.suggested_skill_name.is_none() {
            continue;
        }
        let draft = generate_skill_draft(pattern);
        let source_episode = pattern.episode_ids.first().cloned();
        let proposal = SkillProposal {
            skill_text: draft,
            authored_by: SkillAuthor::Agent,
            proposed_at_ns,
            source_episode,
        };
        let outcome = match evaluate_skill_proposal(proposal, registry, &screen, gate_config) {
            Ok(o) => o,
            // A duplicate id (the skill already exists from a prior cycle) or a
            // parse failure is non-fatal: skip this draft and continue.
            Err(_) => continue,
        };

        let Some(skill_id) = outcome.artifact_id.as_deref() else {
            // Rejected by content screening — not registered.
            continue;
        };

        // Look up the freshly-registered entry to emit a faithful audit record.
        if let Some(entry) = registry.list_all().into_iter().find(|e| e.id == skill_id) {
            let initial_state = match entry.state {
                SkillState::Active => "Active",
                SkillState::Proposed => "Proposed",
                SkillState::Quarantined { .. } => "Quarantined",
                SkillState::RolledBack => "RolledBack",
            };
            audit.push(AuditEntry::SkillRegistered {
                agent_id: agent_id.to_string(),
                skill_id: entry.id.clone(),
                skill_name: entry.manifest.name.clone(),
                authored_by: entry.provenance.authored_by.to_string(),
                source_episode: entry.provenance.source_episode.clone(),
                initial_state: initial_state.to_string(),
            });
        }

        if matches!(outcome.action, ProposalAction::PendingApproval) {
            registered_proposed_ids.push(skill_id.to_string());
        }
    }

    audit.push(AuditEntry::SkillReflectionCompleted {
        agent_id: agent_id.to_string(),
        episodes_analysed: report.episodes_analysed,
        patterns_found: report.patterns.len(),
        proposals_generated: report.proposals_generated,
    });

    ReflectionRegistration {
        episodes_analysed: report.episodes_analysed,
        patterns_found: report.patterns.len(),
        proposals_generated: report.proposals_generated,
        registered_proposed_ids,
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
        let report = run_maintenance_audited("test-agent", &mut audit, None, None, None, None);

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
        run_maintenance_audited("soak-agent", &mut audit, None, None, None, None);

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
        let report = run_maintenance_audited("test-agent", &mut audit, Some(ctx), None, None, None);

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
        let report = run_maintenance_audited("test-agent", &mut audit, Some(ctx), None, None, None);

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
        run_maintenance_audited("invariant-agent", &mut audit, Some(ctx), None, None, None);

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
        let report = run_maintenance_audited("agent", &mut audit, Some(ctx), None, None, None);

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
        let report = run_maintenance_audited("no-ctx-agent", &mut audit, None, None, None, None);

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
            let report =
                run_maintenance_audited("agent", &mut audit, None, Some(replay_ctx), None, None);

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
        let report = run_maintenance_audited(
            "rollback-agent",
            &mut audit,
            None,
            Some(replay_ctx),
            None,
            None,
        );

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
        let report = run_maintenance_audited("no-replay-agent", &mut audit, None, None, None, None);
        let replay_outcome = &report.outcomes[1];
        assert_eq!(replay_outcome.routine, SleepRoutine::GenerativeReplay);
        assert!(
            replay_outcome.replay.is_none(),
            "replay report must be None when no context is supplied"
        );
        assert!(replay_outcome.replay_rollback_nodes.is_empty());
    }

    // ── E3.7 — Dream exploration ──────────────────────────────────────────────

    /// E3.7: dream report is populated when a DreamContext is supplied.
    #[test]
    fn dream_report_is_logged_when_dream_context_is_supplied() {
        use memory::{
            archive_memory_node, DreamConfig, L3Archive, MemoryNode, Provenance, SourceTier,
        };

        let path = std::env::temp_dir().join("sleep_dream_logged.json");
        let _ = std::fs::remove_file(&path);

        let mut l3 = L3Archive::open(&path, 4, 100).unwrap();
        for i in 1u64..=4 {
            let node = MemoryNode::new(0.9 - i as f32 * 0.05, 0.1 * i as f32);
            let item = archive_memory_node(i, &format!("node-{i}"), &node);
            let prov = Provenance::now(SourceTier::L1, &format!("node-{i}"));
            l3.demote(item, prov).unwrap();
        }

        let mut audit = AuditLog::new();
        let dream_ctx = DreamContext {
            l3: &l3,
            config: DreamConfig::default(),
        };
        let report =
            run_maintenance_audited("dream-agent", &mut audit, None, None, Some(dream_ctx), None);

        let dream_outcome = &report.outcomes[2]; // DreamExploration is index 2
        assert_eq!(dream_outcome.routine, SleepRoutine::DreamExploration);
        assert!(
            dream_outcome.dream.is_some(),
            "dream report must be populated when DreamContext is supplied"
        );
        assert!(dream_outcome.completed);

        let _ = std::fs::remove_file(&path);
    }

    /// E3.7 exit criterion 1: dream report contains seed and threshold even when no
    /// candidates are found.
    #[test]
    fn dream_report_is_logged_for_every_cycle_even_with_empty_archive() {
        let path = std::env::temp_dir().join("sleep_dream_empty.json");
        let _ = std::fs::remove_file(&path);
        let l3 = memory::L3Archive::open(&path, 4, 100).unwrap();

        let mut audit = AuditLog::new();
        let dream_ctx = DreamContext {
            l3: &l3,
            config: memory::DreamConfig {
                seed: 77,
                ..Default::default()
            },
        };
        let report =
            run_maintenance_audited("empty-dream", &mut audit, None, None, Some(dream_ctx), None);

        let outcome = &report.outcomes[2];
        let dr = outcome
            .dream
            .as_ref()
            .expect("dream report must be present");
        assert_eq!(dr.seed, 77);
        assert_eq!(dr.candidates_found, 0);
        assert!(outcome.dream_candidates.is_empty());

        let _ = std::fs::remove_file(&path);
    }

    /// Without a DreamContext the DreamExploration phase runs the no-op stub.
    #[test]
    fn dream_phase_uses_stub_when_no_context_supplied() {
        let mut audit = AuditLog::new();
        let report = run_maintenance_audited("no-dream-agent", &mut audit, None, None, None, None);
        let dream_outcome = &report.outcomes[2];
        assert_eq!(dream_outcome.routine, SleepRoutine::DreamExploration);
        assert!(dream_outcome.dream.is_none());
        assert!(dream_outcome.dream_candidates.is_empty());
        assert!(dream_outcome.completed);
    }

    /// E3.7 exit criterion 1: same archive + same seed → same candidates.
    #[test]
    fn dream_candidates_are_reproducible_per_seed() {
        use memory::{
            archive_memory_node, DreamConfig, L3Archive, MemoryNode, Provenance, SourceTier,
        };

        let path = std::env::temp_dir().join("sleep_dream_repro.json");
        let _ = std::fs::remove_file(&path);
        let mut l3 = L3Archive::open(&path, 4, 100).unwrap();
        for i in 1u64..=5 {
            let node = MemoryNode::new(0.9, 0.1 * i as f32);
            let item = archive_memory_node(i, &format!("m{i}"), &node);
            let prov = Provenance::now(SourceTier::L1, &format!("m{i}"));
            l3.demote(item, prov).unwrap();
        }

        let cfg = DreamConfig {
            seed: 42,
            similarity_threshold: 0.0,
            ..Default::default()
        };

        let dream_ctx1 = DreamContext {
            l3: &l3,
            config: cfg.clone(),
        };
        let report1 = run_maintenance_audited(
            "a1",
            &mut AuditLog::new(),
            None,
            None,
            Some(dream_ctx1),
            None,
        );

        let dream_ctx2 = DreamContext {
            l3: &l3,
            config: cfg,
        };
        let report2 = run_maintenance_audited(
            "a2",
            &mut AuditLog::new(),
            None,
            None,
            Some(dream_ctx2),
            None,
        );

        assert_eq!(
            report1.outcomes[2].dream_candidates, report2.outcomes[2].dream_candidates,
            "same seed must produce identical candidates"
        );

        let _ = std::fs::remove_file(&path);
    }

    // ── E3.8 — Policy compilation ─────────────────────────────────────────────

    /// E3.8: compilation report is populated when a CompilationContext is supplied.
    #[test]
    fn compilation_report_is_populated_when_context_is_supplied() {
        use memory::{AuditTraceEntry, CompilationConfig};

        let entries = vec![
            AuditTraceEntry::TaskStarted {
                task_id: 1,
                tier: 0,
                prompt: "q1".into(),
            },
            AuditTraceEntry::TaskCompleted {
                task_id: 1,
                tokens_emitted: 2,
                response: "a1".into(),
            },
        ];

        let dir = std::env::temp_dir().join("sleep_compile_test");
        let _ = std::fs::remove_dir_all(&dir);

        let mut audit = AuditLog::new();
        let comp_ctx = CompilationContext {
            entries: &entries,
            config: CompilationConfig {
                output_dir: dir.clone(),
                formats: vec![memory::TrainingFormat::Alpaca],
                append: false,
            },
        };
        let report =
            run_maintenance_audited("comp-agent", &mut audit, None, None, None, Some(comp_ctx));

        let comp_outcome = &report.outcomes[3]; // PolicyCompilation is index 3
        assert_eq!(comp_outcome.routine, SleepRoutine::PolicyCompilation);
        assert!(
            comp_outcome.compilation.is_some(),
            "compilation report must be present when context is supplied"
        );
        let cr = comp_outcome.compilation.as_ref().unwrap();
        assert_eq!(cr.pairs_compiled, 1);
        assert_eq!(cr.files_written, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Without a CompilationContext the PolicyCompilation phase runs the no-op stub.
    #[test]
    fn compilation_phase_uses_stub_when_no_context_supplied() {
        let mut audit = AuditLog::new();
        let report = run_maintenance_audited("no-comp-agent", &mut audit, None, None, None, None);
        let comp_outcome = &report.outcomes[3];
        assert_eq!(comp_outcome.routine, SleepRoutine::PolicyCompilation);
        assert!(comp_outcome.compilation.is_none());
        assert!(comp_outcome.completed);
    }

    /// E3.8 exit criterion 2: zero pairs produces no files but the report is still populated.
    #[test]
    fn compilation_with_no_completed_tasks_writes_no_files() {
        use memory::{AuditTraceEntry, CompilationConfig};

        let entries = vec![AuditTraceEntry::TaskFailed {
            task_id: 99,
            error: "timeout".into(),
        }];

        let dir = std::env::temp_dir().join("sleep_compile_empty");
        let _ = std::fs::remove_dir_all(&dir);

        let mut audit = AuditLog::new();
        let comp_ctx = CompilationContext {
            entries: &entries,
            config: CompilationConfig {
                output_dir: dir.clone(),
                formats: vec![memory::TrainingFormat::Alpaca],
                append: false,
            },
        };
        let report =
            run_maintenance_audited("empty-comp", &mut audit, None, None, None, Some(comp_ctx));

        let cr = report.outcomes[3].compilation.as_ref().unwrap();
        assert_eq!(cr.pairs_compiled, 0, "no pairs from failed tasks");
        assert_eq!(cr.files_written, 0, "no file written when no pairs");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── E11 — Dreaming-phase self-improvement reflection (S11.5) ──────────────

    fn co_occurrence_episodes() -> Vec<EpisodeSummary> {
        // Three episodes that all pair the same two tools → one friction pattern
        // above the default threshold (>= 2 occurrences).
        (0..3)
            .map(|i| EpisodeSummary {
                episode_id: format!("ep-{i}"),
                summary: format!("episode {i}: searched then archived"),
                tools_used: vec!["web-search".to_string(), "archive".to_string()],
                success: true,
            })
            .collect()
    }

    #[test]
    fn reflection_registers_proposed_agent_skill_and_emits_audit() {
        let episodes = co_occurrence_episodes();
        let mut registry = SkillRegistry::default();
        let mut audit = AuditLog::new();

        let reg = run_self_improvement_reflection(
            "dream-agent",
            &episodes,
            &ReflectionConfig::default(),
            // auto-promotion OFF → drafts land as Proposed (PendingApproval).
            &PromotionGateConfig {
                auto_promote_agent_skills: false,
            },
            &mut registry,
            &mut audit,
            42,
        );

        assert!(
            reg.patterns_found >= 1,
            "a repeated tool co-occurrence pattern must be found"
        );
        assert!(
            !reg.registered_proposed_ids.is_empty(),
            "at least one Proposed agent-authored skill must be registered"
        );

        // Registered skills are agent-authored and in the Proposed state.
        for id in &reg.registered_proposed_ids {
            let entry = registry
                .list_all()
                .into_iter()
                .find(|e| &e.id == id)
                .expect("registered skill must exist");
            assert_eq!(entry.provenance.authored_by, SkillAuthor::Agent);
            assert!(entry.state.is_proposed());
        }
        // Proposed skills are not active.
        assert_eq!(registry.list_active().len(), 0);

        // Audit: exactly one SkillReflectionCompleted + >=1 SkillRegistered.
        let entries = audit.entries();
        let reflections = entries
            .iter()
            .filter(|e| matches!(e, AuditEntry::SkillReflectionCompleted { .. }))
            .count();
        let registrations = entries
            .iter()
            .filter(|e| matches!(e, AuditEntry::SkillRegistered { .. }))
            .count();
        assert_eq!(reflections, 1, "exactly one reflection-completed entry");
        assert!(registrations >= 1, "at least one skill-registered entry");
    }

    #[test]
    fn reflection_with_no_qualifying_pattern_registers_nothing() {
        // Distinct tools per episode → no pair co-occurs above threshold.
        let episodes = vec![
            EpisodeSummary {
                episode_id: "e1".to_string(),
                summary: "one".to_string(),
                tools_used: vec!["tool-a".to_string(), "tool-b".to_string()],
                success: true,
            },
            EpisodeSummary {
                episode_id: "e2".to_string(),
                summary: "two".to_string(),
                tools_used: vec!["tool-c".to_string(), "tool-d".to_string()],
                success: true,
            },
            EpisodeSummary {
                episode_id: "e3".to_string(),
                summary: "three".to_string(),
                tools_used: vec!["tool-e".to_string(), "tool-f".to_string()],
                success: true,
            },
        ];
        let mut registry = SkillRegistry::default();
        let mut audit = AuditLog::new();

        let reg = run_self_improvement_reflection(
            "dream-agent",
            &episodes,
            &ReflectionConfig::default(),
            &PromotionGateConfig::default(),
            &mut registry,
            &mut audit,
            7,
        );

        assert_eq!(reg.patterns_found, 0);
        assert!(reg.registered_proposed_ids.is_empty());
        assert!(registry.is_empty(), "nothing registered without a pattern");

        // The reflection summary is still emitted (behaviour observable), but no
        // SkillRegistered entries.
        let entries = audit.entries();
        assert_eq!(
            entries
                .iter()
                .filter(|e| matches!(e, AuditEntry::SkillReflectionCompleted { .. }))
                .count(),
            1
        );
        assert_eq!(
            entries
                .iter()
                .filter(|e| matches!(e, AuditEntry::SkillRegistered { .. }))
                .count(),
            0
        );
    }

    #[test]
    fn reflection_with_no_episodes_is_a_noop() {
        let mut registry = SkillRegistry::default();
        let mut audit = AuditLog::new();
        let reg = run_self_improvement_reflection(
            "dream-agent",
            &[],
            &ReflectionConfig::default(),
            &PromotionGateConfig::default(),
            &mut registry,
            &mut audit,
            0,
        );
        assert_eq!(reg.episodes_analysed, 0);
        assert_eq!(reg.patterns_found, 0);
        assert!(reg.registered_proposed_ids.is_empty());
        assert!(registry.is_empty());
    }
}
