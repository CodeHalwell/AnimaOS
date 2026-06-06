//! S8.4.7 — The adaptation eval harness (the LoRA-vs-HRA decider).
//!
//! The report is emphatic that a method must *earn* its adoption per domain
//! rather than be assumed — "a well-tuned LoRA often matches HRA
//! in-distribution" (S8.4.7). This harness scores an [`AdapterArtifact`] on the
//! report's **four metrics, not one**:
//!
//! 1. *Task success* on a held-out slice of the domain.
//! 2. *OOD generalisation* on a shifted held-out set.
//! 3. *Retention / anti-forgetting* on a frozen core-competencies probe.
//! 4. *Merge fidelity* — the S8.4.6 precision-delta after requantisation.
//!
//! and applies the **adoption rule**: promote a candidate over the LoRA baseline
//! only when it clears a configurable **margin** on (1)+(2) *without* regressing
//! (3) beyond tolerance.
//!
//! ## Fixture scoring
//!
//! Real scores come from running models on held-out data (env-gated live runs).
//! Here scoring is **deterministic fixture scoring**: each [`EvalCase`] carries
//! its own recorded `expected_score`, and the harness folds the adapter's stable
//! identity into a tiny per-case perturbation so different adapters get
//! *different but reproducible* reports — exactly the "fixture datasets +
//! recorded scores for CI" the report calls for (S8.4.7 "Hermetic by default").

use crate::artifact::AdapterArtifact;
use crate::hash::Fnv1a;
use crate::method::MergePath;
use serde::{Deserialize, Serialize};

/// Which of the four S8.4.7 metrics an [`EvalCase`] contributes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    /// (1) Task success on a held-out slice of the domain.
    TaskSuccess,
    /// (2) OOD generalisation on a shifted held-out set.
    OodGeneralisation,
    /// (3) Retention / anti-forgetting on a frozen core-competencies probe.
    Retention,
}

/// One probe in the eval set: an input, the metric it measures, and the recorded
/// fixture score it would yield for a *baseline* adapter (0.0–1.0).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalCase {
    /// Stable identifier for the probe (used in the deterministic perturbation).
    pub id: String,
    /// Which metric bucket this case feeds.
    pub metric: Metric,
    /// The prompt/needle shown to the model.
    pub prompt: String,
    /// Recorded baseline score in `[0.0, 1.0]` for fixture scoring.
    pub expected_score: f32,
}

impl EvalCase {
    /// Construct a case.
    pub fn new(
        id: impl Into<String>,
        metric: Metric,
        prompt: impl Into<String>,
        expected_score: f32,
    ) -> Self {
        EvalCase {
            id: id.into(),
            metric,
            prompt: prompt.into(),
            expected_score: expected_score.clamp(0.0, 1.0),
        }
    }
}

/// The four metric scores (each in `[0.0, 1.0]`), per S8.4.7.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MetricScores {
    /// (1) Mean task-success score.
    pub task_success: f32,
    /// (2) Mean OOD-generalisation score.
    pub ood_generalisation: f32,
    /// (3) Mean retention / anti-forgetting score.
    pub retention: f32,
    /// (4) Merge fidelity = `1.0 - precision_delta` after requantisation.
    pub merge_fidelity: f32,
}

/// The result of evaluating one adapter over an eval set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalReport {
    /// The adapter that was scored.
    pub adapter_id: String,
    /// Per-metric mean scores.
    pub scores: MetricScores,
    /// Number of cases scored, per metric, for transparency.
    pub case_counts: CaseCounts,
}

/// How many cases fed each metric (cases with no examples score `1.0` neutrally
/// only via the default; see [`evaluate_adapter`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseCounts {
    /// Number of task-success cases.
    pub task_success: usize,
    /// Number of OOD cases.
    pub ood_generalisation: usize,
    /// Number of retention cases.
    pub retention: usize,
}

impl EvalReport {
    /// The S8.4.7 **adoption rule** vs a `baseline` report.
    ///
    /// Returns `true` when *this* (candidate) report beats `baseline` by at least
    /// `margin` on **both** task success and OOD generalisation, **and** does not
    /// regress retention by more than `retention_tolerance`. Otherwise the
    /// baseline (LoRA) is kept.
    pub fn beats_baseline(
        &self,
        baseline: &EvalReport,
        margin: f32,
        retention_tolerance: f32,
    ) -> bool {
        let task_gain = self.scores.task_success - baseline.scores.task_success;
        let ood_gain = self.scores.ood_generalisation - baseline.scores.ood_generalisation;
        let retention_drop = baseline.scores.retention - self.scores.retention;
        task_gain >= margin && ood_gain >= margin && retention_drop <= retention_tolerance
    }
}

/// S8.4.7 — score `artifact` over `cases` using deterministic fixture scoring.
///
/// For each case the baseline `expected_score` is perturbed by a tiny,
/// adapter-specific amount derived from a stable hash of
/// `(adapter_id, weights_digest, case.id)`. The perturbation is bounded to
/// `±0.10` so recorded baselines stay meaningful while distinct adapters get
/// distinct, reproducible reports. Merge fidelity is derived from the adapter's
/// [`MergePath`]: the clean path keeps near-perfect fidelity; the Hadamard path
/// incurs a small, deterministic requantisation delta (S8.4.6).
pub fn evaluate_adapter(artifact: &AdapterArtifact, cases: &[EvalCase]) -> EvalReport {
    let mut sums = [0.0f32; 3];
    let mut counts = [0usize; 3];

    for case in cases {
        let score = perturbed_score(artifact, case);
        let idx = metric_index(case.metric);
        sums[idx] += score;
        counts[idx] += 1;
    }

    let mean = |idx: usize| -> f32 {
        if counts[idx] == 0 {
            // No probe for this metric: report a neutral perfect score so a
            // missing bucket never spuriously fails the adoption rule.
            1.0
        } else {
            (sums[idx] / counts[idx] as f32).clamp(0.0, 1.0)
        }
    };

    let scores = MetricScores {
        task_success: mean(metric_index(Metric::TaskSuccess)),
        ood_generalisation: mean(metric_index(Metric::OodGeneralisation)),
        retention: mean(metric_index(Metric::Retention)),
        merge_fidelity: merge_fidelity(artifact),
    };

    EvalReport {
        adapter_id: artifact.adapter_id.clone(),
        scores,
        case_counts: CaseCounts {
            task_success: counts[metric_index(Metric::TaskSuccess)],
            ood_generalisation: counts[metric_index(Metric::OodGeneralisation)],
            retention: counts[metric_index(Metric::Retention)],
        },
    }
}

fn metric_index(m: Metric) -> usize {
    match m {
        Metric::TaskSuccess => 0,
        Metric::OodGeneralisation => 1,
        Metric::Retention => 2,
    }
}

/// Deterministic per-case perturbation in `[-0.10, +0.10]` folded onto the
/// recorded baseline score.
fn perturbed_score(artifact: &AdapterArtifact, case: &EvalCase) -> f32 {
    let mut h = Fnv1a::new();
    h.write_str("anima-finetune.eval.v1");
    h.write_str(&artifact.adapter_id);
    h.write_str(&artifact.weights_digest);
    h.write_str(&case.id);
    let digest = h.finish();
    // Map the low bits to [-0.10, +0.10].
    let unit = (digest % 2001) as f32 / 2000.0; // [0, 1]
    let delta = (unit - 0.5) * 0.2; // [-0.10, +0.10]
    (case.expected_score + delta).clamp(0.0, 1.0)
}

/// Merge fidelity = `1.0 - precision_delta` (S8.4.6). The clean-merge path is
/// effectively lossless; the Hadamard path incurs a small, deterministic delta.
fn merge_fidelity(artifact: &AdapterArtifact) -> f32 {
    match artifact.merge_path {
        MergePath::Clean => 1.0,
        MergePath::Hadamard => {
            // Deterministic small delta seeded by the weights digest, in
            // [0.00, 0.05], modelling requantisation drift.
            let unit = (crate::hash::hash_bytes(artifact.weights_digest.as_bytes()) % 1001) as f32
                / 1000.0;
            1.0 - unit * 0.05
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{AdapterFormat, Provenance};
    use crate::method::{AdaptationMethod, HraKind, ServingTier};

    fn artifact(id: &str, digest: &str, merge: MergePath) -> AdapterArtifact {
        AdapterArtifact {
            adapter_id: id.to_string(),
            description: "d".to_string(),
            format: AdapterFormat::LoraAdapter,
            merge_path: merge,
            serving_tier: ServingTier::MountableAdapter,
            weights_digest: digest.to_string(),
            provenance: Provenance {
                base_model: "base-q4".to_string(),
                method: AdaptationMethod::default(),
                source_job: "job".to_string(),
                created_at_ns: 0,
            },
        }
    }

    fn cases() -> Vec<EvalCase> {
        vec![
            EvalCase::new("t1", Metric::TaskSuccess, "solve x", 0.8),
            EvalCase::new("t2", Metric::TaskSuccess, "solve y", 0.8),
            EvalCase::new("o1", Metric::OodGeneralisation, "shifted", 0.6),
            EvalCase::new("r1", Metric::Retention, "core fact", 0.95),
        ]
    }

    #[test]
    fn report_is_deterministic() {
        let a = artifact("ad", "dig", MergePath::Clean);
        let r1 = evaluate_adapter(&a, &cases());
        let r2 = evaluate_adapter(&a, &cases());
        assert_eq!(r1, r2);
    }

    #[test]
    fn scores_are_in_unit_range() {
        let a = artifact("ad", "dig", MergePath::Clean);
        let r = evaluate_adapter(&a, &cases());
        for s in [
            r.scores.task_success,
            r.scores.ood_generalisation,
            r.scores.retention,
            r.scores.merge_fidelity,
        ] {
            assert!((0.0..=1.0).contains(&s), "score out of range: {s}");
        }
    }

    #[test]
    fn case_counts_are_reported() {
        let a = artifact("ad", "dig", MergePath::Clean);
        let r = evaluate_adapter(&a, &cases());
        assert_eq!(r.case_counts.task_success, 2);
        assert_eq!(r.case_counts.ood_generalisation, 1);
        assert_eq!(r.case_counts.retention, 1);
    }

    #[test]
    fn different_adapters_yield_different_reports() {
        let a = artifact("ad-a", "dig-a", MergePath::Clean);
        let b = artifact("ad-b", "dig-b", MergePath::Clean);
        let ra = evaluate_adapter(&a, &cases());
        let rb = evaluate_adapter(&b, &cases());
        // Same baseline cases, but distinct adapter identity => distinct scores.
        assert_ne!(ra.scores, rb.scores);
    }

    #[test]
    fn clean_path_is_full_fidelity_hadamard_is_lower() {
        let clean = artifact("c", "dig", MergePath::Clean);
        let hadamard = artifact("h", "dig", MergePath::Hadamard);
        let rc = evaluate_adapter(&clean, &cases());
        let rh = evaluate_adapter(&hadamard, &cases());
        assert_eq!(rc.scores.merge_fidelity, 1.0);
        assert!(rh.scores.merge_fidelity < 1.0);
        assert!(rh.scores.merge_fidelity >= 0.95);
    }

    #[test]
    fn empty_cases_give_neutral_scores() {
        let a = artifact("ad", "dig", MergePath::Clean);
        let r = evaluate_adapter(&a, &[]);
        assert_eq!(r.scores.task_success, 1.0);
        assert_eq!(r.scores.ood_generalisation, 1.0);
        assert_eq!(r.scores.retention, 1.0);
        assert_eq!(r.case_counts.task_success, 0);
    }

    #[test]
    fn adoption_rule_requires_margin_on_both_axes() {
        let baseline = EvalReport {
            adapter_id: "lora".to_string(),
            scores: MetricScores {
                task_success: 0.70,
                ood_generalisation: 0.60,
                retention: 0.90,
                merge_fidelity: 1.0,
            },
            case_counts: CaseCounts {
                task_success: 1,
                ood_generalisation: 1,
                retention: 1,
            },
        };
        // Clears margin on both axes, retention steady -> adopt.
        let good = EvalReport {
            adapter_id: "hra".to_string(),
            scores: MetricScores {
                task_success: 0.80,
                ood_generalisation: 0.70,
                retention: 0.89,
                merge_fidelity: 0.99,
            },
            ..baseline.clone()
        };
        assert!(good.beats_baseline(&baseline, 0.05, 0.05));

        // Gains task but not OOD -> keep baseline.
        let lopsided = EvalReport {
            scores: MetricScores {
                task_success: 0.90,
                ood_generalisation: 0.61,
                ..good.scores
            },
            ..good.clone()
        };
        assert!(!lopsided.beats_baseline(&baseline, 0.05, 0.05));

        // Gains both but regresses retention past tolerance -> keep baseline.
        let forgetful = EvalReport {
            scores: MetricScores {
                task_success: 0.85,
                ood_generalisation: 0.70,
                retention: 0.70,
                merge_fidelity: 0.99,
            },
            ..good.clone()
        };
        assert!(!forgetful.beats_baseline(&baseline, 0.05, 0.05));
    }

    #[test]
    fn report_serde_round_trip() {
        let a = artifact("ad", "dig", MergePath::Hadamard);
        let r = evaluate_adapter(&a, &cases());
        let json = serde_json::to_string(&r).unwrap();
        let back: EvalReport = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn hra_artifact_evaluates_with_lower_fidelity() {
        // An HiRA (Hadamard) adapter explicitly carries the lossy merge path.
        let mut a = artifact("hira", "dig-hira", MergePath::Hadamard);
        a.provenance.method = AdaptationMethod::Hra {
            family: HraKind::Hira,
            rank: 128,
        };
        let r = evaluate_adapter(&a, &cases());
        assert!(r.scores.merge_fidelity < 1.0);
    }
}
