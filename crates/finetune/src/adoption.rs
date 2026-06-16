//! S8.4.8 — the adapter **adoption gate**.
//!
//! A sleep-phase-trained adapter is *registered* in the [`AdapterLibrary`] as
//! soon as training finishes (`dataset → train → register`), but it must not be
//! *mounted* onto a live serving tier until it has **earned adoption**. This
//! module is that gate. It fuses the two independent checks the report requires
//! before the router may hot-mount an adapter:
//!
//! 1. **E8 eval harness** ([`crate::eval`]): the candidate must clear the
//!    S8.4.7 adoption rule against the LoRA baseline (margin on task-success +
//!    OOD without regressing retention) **and** keep merge fidelity above a
//!    floor (S8.4.6).
//! 2. **E13 alignment screen**: the adapter-adoption proposal must pass the
//!    constitution's alignment evals.
//!
//! The alignment result is supplied by the caller as an [`AlignmentOutcome`] so
//! `finetune` stays decoupled from the `constitution` crate — the host (which
//! depends on both) maps its `ConstitutionCheck` verdict onto this type. The
//! decision is then recorded against the library
//! ([`AdapterLibrary::record_adoption`](crate::library::AdapterLibrary::record_adoption)),
//! and only adoption-cleared adapters can be mounted via
//! [`AdapterLibrary::mount_gated`](crate::library::AdapterLibrary::mount_gated).
//!
//! [`AdapterLibrary`]: crate::library::AdapterLibrary

use crate::eval::EvalReport;
use serde::{Deserialize, Serialize};

/// The E13 alignment-screen result for an adapter-adoption proposal.
///
/// Produced by the host from the constitution's `ConstitutionCheck`; kept as a
/// plain data type here so `finetune` carries no dependency on `constitution`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum AlignmentOutcome {
    /// The adoption proposal cleared the alignment evals.
    Approved,
    /// The proposal was vetoed; carries the human-readable clause reasons.
    Vetoed {
        /// One entry per violated clause, for the audit trail / approval queue.
        reasons: Vec<String>,
    },
}

impl AlignmentOutcome {
    /// `true` when alignment approved the adoption.
    pub fn is_approved(&self) -> bool {
        matches!(self, AlignmentOutcome::Approved)
    }
}

/// Thresholds for the eval-harness half of the gate.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AdoptionPolicy {
    /// Minimum gain over baseline on **both** task-success and OOD (S8.4.7).
    pub margin: f32,
    /// Maximum tolerated retention regression vs baseline (anti-forgetting).
    pub retention_tolerance: f32,
    /// Minimum merge fidelity (`1.0 - precision_delta`) the candidate must keep.
    pub min_merge_fidelity: f32,
}

impl Default for AdoptionPolicy {
    /// The report's default operating point: a 5-point margin, a 5-point
    /// retention floor, and ≥ 0.95 merge fidelity.
    fn default() -> Self {
        Self {
            margin: 0.05,
            retention_tolerance: 0.05,
            min_merge_fidelity: 0.95,
        }
    }
}

/// The combined adoption decision for one candidate adapter.
///
/// `approved` is `true` only when **both** halves pass; `reasons` is empty on
/// approval and otherwise lists every failed check (eval- and alignment-side),
/// suitable for the audit trail and the operator approval queue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdoptionDecision {
    /// The adapter the decision is about.
    pub adapter_id: String,
    /// The `weights_digest` of the exact artifact that was evaluated. Recorded so
    /// downstream consumers (the library's adoption map, the E15 proposal bridge)
    /// can verify a decision against the currently-registered weights and reject
    /// a stale decision once the id's weights have been replaced.
    pub weights_digest: String,
    /// Whether the adapter may be mounted.
    pub approved: bool,
    /// Whether the eval-harness half passed (adoption rule + fidelity floor).
    pub eval_passed: bool,
    /// Whether the alignment screen passed.
    pub alignment_passed: bool,
    /// Human-readable reasons for any failure (empty when `approved`).
    pub reasons: Vec<String>,
}

impl AdoptionDecision {
    /// `true` when the adapter cleared both halves of the gate.
    pub fn is_approved(&self) -> bool {
        self.approved
    }
}

/// Decide whether `candidate` may be adopted (mounted) in place of `baseline`.
///
/// Combines the E8 eval harness (the S8.4.7 adoption rule + an S8.4.6 merge
/// fidelity floor) with the E13 `alignment` outcome. The adapter is approved
/// only when **both** halves pass; every failing check is recorded in
/// [`AdoptionDecision::reasons`].
///
/// `weights_digest` is the digest of the artifact being evaluated; it is stamped
/// onto the returned [`AdoptionDecision`] so the decision is bound to the exact
/// weights it was made about (pass `artifact.weights_digest`).
pub fn decide_adoption(
    candidate: &EvalReport,
    baseline: &EvalReport,
    alignment: &AlignmentOutcome,
    policy: &AdoptionPolicy,
    weights_digest: &str,
) -> AdoptionDecision {
    let mut reasons = Vec::new();

    // ── E8 eval half ──────────────────────────────────────────────────────────
    let beats = candidate.beats_baseline(baseline, policy.margin, policy.retention_tolerance);
    if !beats {
        reasons.push(format!(
            "eval: did not beat baseline by margin {:.2} on task-success and OOD \
             without regressing retention beyond {:.2}",
            policy.margin, policy.retention_tolerance
        ));
    }
    let fidelity_ok = candidate.scores.merge_fidelity >= policy.min_merge_fidelity;
    if !fidelity_ok {
        reasons.push(format!(
            "eval: merge fidelity {:.3} below minimum {:.3}",
            candidate.scores.merge_fidelity, policy.min_merge_fidelity
        ));
    }
    let eval_passed = beats && fidelity_ok;

    // ── E13 alignment half ────────────────────────────────────────────────────
    let alignment_passed = alignment.is_approved();
    if let AlignmentOutcome::Vetoed { reasons: clauses } = alignment {
        if clauses.is_empty() {
            reasons.push("alignment: vetoed".to_string());
        } else {
            for clause in clauses {
                reasons.push(format!("alignment: {clause}"));
            }
        }
    }

    AdoptionDecision {
        adapter_id: candidate.adapter_id.clone(),
        weights_digest: weights_digest.to_string(),
        approved: eval_passed && alignment_passed,
        eval_passed,
        alignment_passed,
        reasons,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{CaseCounts, EvalReport, MetricScores};

    fn report(id: &str, task: f32, ood: f32, retention: f32, fidelity: f32) -> EvalReport {
        EvalReport {
            adapter_id: id.to_string(),
            scores: MetricScores {
                task_success: task,
                ood_generalisation: ood,
                retention,
                merge_fidelity: fidelity,
            },
            case_counts: CaseCounts {
                task_success: 1,
                ood_generalisation: 1,
                retention: 1,
            },
        }
    }

    fn baseline() -> EvalReport {
        report("lora", 0.70, 0.60, 0.90, 1.0)
    }

    #[test]
    fn approves_when_eval_and_alignment_both_pass() {
        let candidate = report("hra", 0.80, 0.70, 0.89, 0.99);
        let d = decide_adoption(
            &candidate,
            &baseline(),
            &AlignmentOutcome::Approved,
            &AdoptionPolicy::default(),
            "test-digest",
        );
        assert!(d.approved);
        assert!(d.eval_passed && d.alignment_passed);
        assert_eq!(
            d.weights_digest, "test-digest",
            "decision records the evaluated digest"
        );
        assert!(d.reasons.is_empty(), "approved decision carries no reasons");
    }

    #[test]
    fn rejects_when_eval_fails_even_if_aligned() {
        // Gains task but not OOD → fails the adoption rule.
        let candidate = report("hra", 0.90, 0.61, 0.89, 0.99);
        let d = decide_adoption(
            &candidate,
            &baseline(),
            &AlignmentOutcome::Approved,
            &AdoptionPolicy::default(),
            "test-digest",
        );
        assert!(!d.approved);
        assert!(!d.eval_passed);
        assert!(d.alignment_passed);
        assert!(d.reasons.iter().any(|r| r.starts_with("eval:")));
    }

    #[test]
    fn rejects_low_merge_fidelity() {
        let candidate = report("hra", 0.80, 0.70, 0.89, 0.80); // fidelity < 0.95
        let d = decide_adoption(
            &candidate,
            &baseline(),
            &AlignmentOutcome::Approved,
            &AdoptionPolicy::default(),
            "test-digest",
        );
        assert!(!d.approved);
        assert!(!d.eval_passed);
        assert!(d.reasons.iter().any(|r| r.contains("merge fidelity")));
    }

    #[test]
    fn rejects_when_alignment_vetoes_even_if_eval_passes() {
        let candidate = report("hra", 0.80, 0.70, 0.89, 0.99);
        let d = decide_adoption(
            &candidate,
            &baseline(),
            &AlignmentOutcome::Vetoed {
                reasons: vec!["violates no-deception clause".to_string()],
            },
            &AdoptionPolicy::default(),
            "test-digest",
        );
        assert!(!d.approved);
        assert!(d.eval_passed);
        assert!(!d.alignment_passed);
        assert!(d
            .reasons
            .iter()
            .any(|r| r.contains("violates no-deception clause")));
    }

    #[test]
    fn collects_both_eval_and_alignment_reasons() {
        let candidate = report("hra", 0.71, 0.60, 0.89, 0.80);
        let d = decide_adoption(
            &candidate,
            &baseline(),
            &AlignmentOutcome::Vetoed {
                reasons: vec!["clause-7".to_string()],
            },
            &AdoptionPolicy::default(),
            "test-digest",
        );
        assert!(!d.approved);
        assert!(d.reasons.iter().any(|r| r.starts_with("eval:")));
        assert!(d.reasons.iter().any(|r| r.starts_with("alignment:")));
    }

    #[test]
    fn decision_serde_round_trip() {
        let candidate = report("hra", 0.80, 0.70, 0.89, 0.99);
        let d = decide_adoption(
            &candidate,
            &baseline(),
            &AlignmentOutcome::Approved,
            &AdoptionPolicy::default(),
            "test-digest",
        );
        let json = serde_json::to_string(&d).unwrap();
        let back: AdoptionDecision = serde_json::from_str(&json).unwrap();
        assert_eq!(d, back);
    }
}
