//! E8 S8.4.3 — Sleep-cycle consolidation hook.
//!
//! Wires the [`PolicyCompilation`] sleep phase into the [`FineTuner`] pipeline
//! so the agent can fine-tune a local model on its own compiled episodic
//! experience during sleep cycles.
//!
//! # Design rationale
//!
//! The dreaming/consolidation loop described in `docs/13-local-llm-providers.md`
//! §S8.4.3 is the highest-risk item in either E7/E8: self-experience data is a
//! large distribution shift from base pretraining, and repeated self-fine-tuning
//! risks catastrophic forgetting and value drift.  This module therefore:
//!
//! 1. Is **opt-in only** — [`ConsolidationConfig`] must be explicitly installed
//!    on the [`crate::LifecycleManager`] before any fine-tuning occurs.
//! 2. Has a **minimum-pairs threshold** — sleep cycles that compile too few pairs
//!    are silently skipped, preventing degenerate adapters from tiny sessions.
//! 3. **Audits every event** — skips, starts, completions, and failures all land
//!    in the agent's audit log so the operator can verify the hook's behaviour.
//! 4. **Never trains in CI by default** — [`anima_finetune::FixtureFineTuner`]
//!    is deterministic and side-effect free; real GPU training requires the
//!    `live` feature of `crates/finetune` and is never invoked in CI.
//!
//! # Safety gate
//!
//! Any adapter produced by this hook must pass the defence evaluation
//! (E5.6 / E11 S11.5) before promotion, and should be gated behind explicit
//! operator opt-in in the E15 approval queue.  This module enforces none of
//! those gates — it only performs the fine-tune and audit.  Callers are
//! responsible for routing the returned [`ConsolidationOutcome::Completed`] id
//! through the approval pipeline.

use std::sync::{Arc, Mutex};

use anima_finetune::{
    AdapterLibrary, FineTuneConfig, FineTuneJob, FineTuner, TrainingPair as FtPair, TrainingSet,
};
use memory::compilation::TrainingPair as MemPair;

use crate::audit::{AuditEntry, AuditLog};

// ── ConsolidationConfig ───────────────────────────────────────────────────────

/// Configuration for the sleep-cycle consolidation hook (E8 S8.4.3).
///
/// Install on [`crate::LifecycleManager`] via
/// [`crate::LifecycleManager::enable_consolidation`].
///
/// # Safety
///
/// This is a **gated research spike**.  Enable only after:
/// - Running the S8.4.7 eval harness to confirm the adapter improves the
///   relevant domain without regressing core competencies.
/// - Routing the resulting adapter through the defence evaluation (E5.6).
/// - Obtaining explicit operator approval via the E15 approval queue.
#[derive(Clone)]
pub struct ConsolidationConfig {
    /// Fine-tuner backend.
    ///
    /// Use [`anima_finetune::FixtureFineTuner`] for hermetic tests and CI;
    /// use the live Unsloth trainer (behind `--features live`) for real runs.
    pub tuner: Arc<dyn FineTuner + Send + Sync>,
    /// Fine-tune configuration: base model, adaptation method, hyperparams.
    pub finetune_config: FineTuneConfig,
    /// Minimum compiled pairs required to trigger a run.
    ///
    /// Sleep cycles that compile fewer pairs than this threshold are skipped.
    /// Prevents degenerate adapters from thin sleep sessions.  Default: 4.
    pub min_pairs: usize,
    /// Optional adapter library to register the resulting artifact in.
    ///
    /// When `None` the artifact is produced and returned in the outcome but
    /// not persisted anywhere.  Callers wishing to mount the adapter later
    /// should pass a shared [`AdapterLibrary`] here.
    pub library: Option<Arc<Mutex<AdapterLibrary>>>,
}

impl ConsolidationConfig {
    /// Construct a config with a fixture tuner, default hyperparams, and no library.
    ///
    /// Suitable for hermetic tests.
    pub fn fixture(finetune_config: FineTuneConfig) -> Self {
        ConsolidationConfig {
            tuner: Arc::new(anima_finetune::FixtureFineTuner::new()),
            finetune_config,
            min_pairs: 1,
            library: None,
        }
    }
}

impl std::fmt::Debug for ConsolidationConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsolidationConfig")
            .field("finetune_config", &self.finetune_config)
            .field("min_pairs", &self.min_pairs)
            .field("has_library", &self.library.is_some())
            .finish()
    }
}

// ── ConsolidationOutcome ──────────────────────────────────────────────────────

/// Outcome of a single consolidation attempt during one sleep cycle.
#[derive(Debug, Clone, PartialEq)]
pub enum ConsolidationOutcome {
    /// Not enough pairs compiled; fine-tuning skipped this cycle.
    Skipped {
        /// Pairs produced by the [`PolicyCompilation`] phase this cycle.
        pairs_available: usize,
        /// Configured [`ConsolidationConfig::min_pairs`] threshold.
        min_required: usize,
    },
    /// Fine-tuning ran and an adapter artifact was produced.
    Completed {
        /// Adapter id returned by the tuner (content-addressed for the fixture).
        adapter_id: String,
        /// Number of training pairs consumed.
        pairs_trained: usize,
        /// Whether the artifact was registered in the configured library.
        registered: bool,
    },
    /// Fine-tuning failed; the raw error message is included.
    Failed {
        /// Error from the tuner, formatted as debug output.
        error: String,
    },
}

impl ConsolidationOutcome {
    /// `true` when the hook produced an adapter (not skipped, not failed).
    pub fn completed(&self) -> bool {
        matches!(self, ConsolidationOutcome::Completed { .. })
    }

    /// `true` when the hook was skipped due to insufficient pairs.
    pub fn skipped(&self) -> bool {
        matches!(self, ConsolidationOutcome::Skipped { .. })
    }
}

// ── run_consolidation ─────────────────────────────────────────────────────────

/// Run the consolidation hook on the compiled training pairs from one sleep cycle.
///
/// 1. If `pairs.len() < config.min_pairs`, emits
///    [`AuditEntry::ConsolidationSkipped`] and returns
///    [`ConsolidationOutcome::Skipped`].
/// 2. Otherwise converts `pairs` to [`anima_finetune::TrainingPair`]s, wraps
///    them in a [`anima_finetune::TrainingSet`], and calls
///    [`FineTuner::run_job`].
/// 3. On success, optionally registers the artifact in `config.library`, emits
///    [`AuditEntry::ConsolidationCompleted`], and returns
///    [`ConsolidationOutcome::Completed`].
/// 4. On failure, emits [`AuditEntry::ConsolidationFailed`] and returns
///    [`ConsolidationOutcome::Failed`].
pub fn run_consolidation(
    pairs: &[MemPair],
    agent_id: &str,
    config: &ConsolidationConfig,
    audit: &mut AuditLog,
) -> ConsolidationOutcome {
    // ── Threshold gate ────────────────────────────────────────────────────────
    if pairs.len() < config.min_pairs {
        audit.push(AuditEntry::ConsolidationSkipped {
            agent_id: agent_id.to_string(),
            pairs_available: pairs.len(),
            min_required: config.min_pairs,
        });
        return ConsolidationOutcome::Skipped {
            pairs_available: pairs.len(),
            min_required: config.min_pairs,
        };
    }

    // ── Convert memory pairs → finetune pairs ─────────────────────────────────
    let ft_pairs: Vec<FtPair> = pairs
        .iter()
        .map(|p| FtPair::new(&p.prompt, &p.response))
        .collect();
    let training_set = TrainingSet::from_pairs(&ft_pairs);

    audit.push(AuditEntry::ConsolidationStarted {
        agent_id: agent_id.to_string(),
        pairs_trained: training_set.len(),
    });

    // ── Submit to tuner ───────────────────────────────────────────────────────
    let job = FineTuneJob::new(
        format!("consolidation-{agent_id}"),
        config.finetune_config.clone(),
    );

    match config.tuner.run_job(&job, training_set.pairs()) {
        Ok(artifact) => {
            let adapter_id = artifact.adapter_id.clone();
            let pairs_trained = pairs.len();
            let mut registered = false;

            // Optionally register in the adapter library.
            if let Some(ref lib_arc) = config.library {
                if let Ok(mut lib) = lib_arc.lock() {
                    let _ = lib.register(artifact);
                    registered = true;
                }
            }

            audit.push(AuditEntry::ConsolidationCompleted {
                agent_id: agent_id.to_string(),
                adapter_id: adapter_id.clone(),
                pairs_trained,
                registered,
            });

            ConsolidationOutcome::Completed {
                adapter_id,
                pairs_trained,
                registered,
            }
        }
        Err(e) => {
            let error = format!("{e:?}");
            audit.push(AuditEntry::ConsolidationFailed {
                agent_id: agent_id.to_string(),
                error: error.clone(),
            });
            ConsolidationOutcome::Failed { error }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use anima_finetune::{AdapterLibrary, FineTuneConfig, FixtureFineTuner};
    use crate::audit::{AuditEntry, AuditLog};
    use memory::compilation::TrainingPair as MemPair;

    fn config() -> ConsolidationConfig {
        ConsolidationConfig {
            tuner: Arc::new(FixtureFineTuner::new()),
            finetune_config: FineTuneConfig::new(
                "base-q4",
                "episodic://test",
                "test-adapter",
            ),
            min_pairs: 2,
            library: None,
        }
    }

    fn mem_pair(prompt: &str, response: &str) -> MemPair {
        MemPair {
            prompt: prompt.to_string(),
            response: response.to_string(),
            tier: 0,
            task_id: 1,
        }
    }

    fn two_pairs() -> Vec<MemPair> {
        vec![mem_pair("q1", "a1"), mem_pair("q2", "a2")]
    }

    // ── Skip behaviour ────────────────────────────────────────────────────────

    #[test]
    fn skipped_when_fewer_pairs_than_min() {
        let mut audit = AuditLog::new();
        let pairs = vec![mem_pair("q", "a")]; // 1 < min_pairs=2
        let outcome = run_consolidation(&pairs, "agent", &config(), &mut audit);

        assert!(outcome.skipped(), "must be skipped with 1 pair < min=2");
        assert!(
            matches!(
                outcome,
                ConsolidationOutcome::Skipped { pairs_available: 1, min_required: 2 }
            ),
            "outcome carries correct counts"
        );
        let entries = audit.entries();
        assert_eq!(entries.len(), 1);
        assert!(matches!(
            &entries[0],
            AuditEntry::ConsolidationSkipped { pairs_available: 1, min_required: 2, .. }
        ));
    }

    #[test]
    fn skipped_when_no_pairs() {
        let mut audit = AuditLog::new();
        let outcome = run_consolidation(&[], "agent", &config(), &mut audit);
        assert!(outcome.skipped());
        assert!(matches!(
            &audit.entries()[0],
            AuditEntry::ConsolidationSkipped { pairs_available: 0, .. }
        ));
    }

    // ── Completion behaviour ──────────────────────────────────────────────────

    #[test]
    fn completed_when_pairs_meet_threshold() {
        let mut audit = AuditLog::new();
        let outcome = run_consolidation(&two_pairs(), "agent", &config(), &mut audit);

        assert!(outcome.completed(), "must complete with 2 pairs >= min=2");
        let ConsolidationOutcome::Completed { pairs_trained, registered, .. } = outcome else {
            panic!("expected Completed");
        };
        assert_eq!(pairs_trained, 2);
        assert!(!registered, "no library configured");
    }

    #[test]
    fn completed_emits_started_then_completed_audit_entries() {
        let mut audit = AuditLog::new();
        run_consolidation(&two_pairs(), "agent-x", &config(), &mut audit);

        let entries = audit.entries();
        assert_eq!(entries.len(), 2, "started + completed");
        assert!(matches!(
            &entries[0],
            AuditEntry::ConsolidationStarted { agent_id, pairs_trained: 2 }
            if agent_id == "agent-x"
        ));
        assert!(matches!(
            &entries[1],
            AuditEntry::ConsolidationCompleted { agent_id, pairs_trained: 2, registered: false, .. }
            if agent_id == "agent-x"
        ));
    }

    #[test]
    fn fixture_tuner_is_deterministic_across_calls() {
        let pairs = two_pairs();
        let cfg = config();

        let mut audit1 = AuditLog::new();
        let out1 = run_consolidation(&pairs, "agent", &cfg, &mut audit1);

        let mut audit2 = AuditLog::new();
        let out2 = run_consolidation(&pairs, "agent", &cfg, &mut audit2);

        assert_eq!(out1, out2, "fixture tuner must be deterministic");
    }

    #[test]
    fn different_pairs_yield_different_adapter_ids() {
        let cfg = config();
        let pairs_a = two_pairs();
        let pairs_b = vec![mem_pair("x", "y"), mem_pair("u", "v"), mem_pair("p", "q")];

        let mut audit = AuditLog::new();
        let out_a = run_consolidation(&pairs_a, "a", &cfg, &mut audit);
        let out_b = run_consolidation(&pairs_b, "a", &cfg, &mut audit);

        let id_a = match out_a { ConsolidationOutcome::Completed { adapter_id, .. } => adapter_id, _ => panic!() };
        let id_b = match out_b { ConsolidationOutcome::Completed { adapter_id, .. } => adapter_id, _ => panic!() };
        assert_ne!(id_a, id_b, "different input pairs must produce different adapter ids");
    }

    // ── Library registration ──────────────────────────────────────────────────

    #[test]
    fn adapter_registered_in_library_when_configured() {
        let lib = Arc::new(Mutex::new(AdapterLibrary::new(10)));
        let mut cfg = config();
        cfg.library = Some(Arc::clone(&lib));

        let mut audit = AuditLog::new();
        let outcome = run_consolidation(&two_pairs(), "agent", &cfg, &mut audit);

        let ConsolidationOutcome::Completed { adapter_id, registered, .. } = outcome else {
            panic!("expected Completed");
        };
        assert!(registered, "registered flag must be true");

        let lib_guard = lib.lock().unwrap();
        assert!(
            lib_guard.get(&adapter_id).is_some(),
            "adapter must be retrievable from library by id"
        );
    }

    #[test]
    fn completed_audit_entry_reflects_registration() {
        let lib = Arc::new(Mutex::new(AdapterLibrary::new(10)));
        let mut cfg = config();
        cfg.library = Some(Arc::clone(&lib));

        let mut audit = AuditLog::new();
        run_consolidation(&two_pairs(), "agent", &cfg, &mut audit);

        let completed = audit
            .entries()
            .iter()
            .find(|e| matches!(e, AuditEntry::ConsolidationCompleted { .. }))
            .expect("ConsolidationCompleted entry must be present");

        assert!(matches!(
            completed,
            AuditEntry::ConsolidationCompleted { registered: true, .. }
        ));
    }

    // ── ConsolidationConfig::fixture helper ───────────────────────────────────

    #[test]
    fn fixture_config_accepts_single_pair() {
        let cfg = ConsolidationConfig::fixture(FineTuneConfig::new("m", "d", "a"));
        // min_pairs = 1 so even one pair triggers a run.
        let mut audit = AuditLog::new();
        let outcome = run_consolidation(&[mem_pair("q", "a")], "agent", &cfg, &mut audit);
        assert!(outcome.completed());
    }

    // ── ConsolidationOutcome helpers ──────────────────────────────────────────

    #[test]
    fn outcome_helpers_report_correct_state() {
        let skipped = ConsolidationOutcome::Skipped { pairs_available: 0, min_required: 2 };
        let completed = ConsolidationOutcome::Completed {
            adapter_id: "x".into(),
            pairs_trained: 2,
            registered: false,
        };
        let failed = ConsolidationOutcome::Failed { error: "oops".into() };

        assert!(skipped.skipped());
        assert!(!skipped.completed());
        assert!(completed.completed());
        assert!(!completed.skipped());
        assert!(!failed.completed());
        assert!(!failed.skipped());
    }
}
