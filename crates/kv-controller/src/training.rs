//! Offline training data structures — Story S5.4.3.
//!
//! The offline training pipeline compiles [`InvocationTrace`]s into
//! `(features, label)` training pairs and bundles them into a
//! [`TrainingCorpus`] ready for an external optimiser.
//!
//! # Pipeline overview
//!
//! ```text
//! Live / synthetic / public traces
//!            │
//!     InvocationTrace (from trace.rs)
//!            │
//!  compile_training_pairs()
//!            │
//!     Vec<TrainingPair>  ← (features, teacher_label, weight)
//!            │
//!  TrainingCorpus::new()
//!            │
//!  Training corpus with provenance counts (exit criterion 3)
//! ```
//!
//! # Teacher label generation
//!
//! The teacher is the **full-cache oracle**: a block that would be retained if
//! no eviction budget were imposed. In practice, teacher labels are assigned
//! by the trace capture infrastructure: a post-hoc pass over the
//! [`InvocationTrace`] marks every needle block (`is_user_constraint = true`)
//! and every error-trace block (`is_error_trace = true`) as `teacher_label =
//! true`, reflecting that the oracle would always retain these.
//!
//! # Training objective
//!
//! The documented loss for S5.4.3 has three components:
//! - KL divergence against the teacher distribution (retention probability).
//! - Cache budget regularisation (discourages retaining more than `budget` blocks).
//! - Retrieval-safety loss from adversarially inserted needle blocks.
//!
//! The data structures here carry the per-pair weights needed by each loss term;
//! the actual gradient computation is left to the offline optimiser (Python or
//! Rust — out of scope for the CI path).

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

use crate::features::BlockFeatures;
use crate::trace::{InvocationTrace, ProvenanceCounts, TraceProvenance};

// ── Training pair ──────────────────────────────────────────────────────────────

/// A single `(features, teacher_label)` pair for offline optimisation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrainingPair {
    /// Input features for the gating model.
    pub features: BlockFeatures,
    /// Ground-truth label from the full-cache teacher oracle.
    ///
    /// `true`  = the oracle would retain this block.
    /// `false` = the oracle would evict this block.
    pub teacher_label: bool,
    /// Loss weight applied to this pair.
    ///
    /// Needle blocks (user constraints, error traces) receive a higher weight
    /// to encode the retrieval-safety loss term from S5.4.3.
    pub loss_weight: f32,
    /// Source invocation ID for provenance tracking.
    pub invocation_id: String,
    /// Provenance of the source trace (exit criterion 3).
    pub provenance: TraceProvenance,
}

impl TrainingPair {
    /// Default loss weight for non-needle, non-error blocks.
    pub const DEFAULT_WEIGHT: f32 = 1.0;
    /// Elevated loss weight for needle blocks (user constraints).
    pub const NEEDLE_WEIGHT: f32 = 3.0;
    /// Elevated loss weight for error-trace blocks.
    pub const ERROR_TRACE_WEIGHT: f32 = 2.0;

    /// Computes the appropriate loss weight for a block's features.
    pub fn weight_for(features: &BlockFeatures) -> f32 {
        if features.is_user_constraint {
            Self::NEEDLE_WEIGHT
        } else if features.is_error_trace {
            Self::ERROR_TRACE_WEIGHT
        } else {
            Self::DEFAULT_WEIGHT
        }
    }
}

// ── Compiler ───────────────────────────────────────────────────────────────────

/// Compiles an [`InvocationTrace`] into [`TrainingPair`]s.
///
/// Teacher labels are assigned as follows:
/// - Needle blocks (`is_user_constraint = true`) → `teacher_label = true`.
/// - Error-trace blocks (`is_error_trace = true`) → `teacher_label = true`.
/// - Blocks that were retained by the controller → carry-forward as `true`.
/// - All others → `teacher_label = false`.
///
/// If a block has a [`Some`] `teacher_label` set by the trace capture pass,
/// that label takes precedence.
pub fn compile_training_pairs(trace: &InvocationTrace) -> Vec<TrainingPair> {
    trace
        .blocks
        .iter()
        .map(|block| {
            let teacher_label = block.teacher_label.unwrap_or(
                block.features.is_user_constraint
                    || block.features.is_error_trace
                    || block.retained,
            );
            let loss_weight = TrainingPair::weight_for(&block.features);
            TrainingPair {
                features: block.features.clone(),
                teacher_label,
                loss_weight,
                invocation_id: trace.invocation_id.clone(),
                provenance: trace.provenance.clone(),
            }
        })
        .collect()
}

// ── Training corpus ────────────────────────────────────────────────────────────

/// Bundle of training pairs across multiple invocations, with provenance counts.
///
/// The corpus carries the [`ProvenanceCounts`] required by exit criterion 3
/// so that aggregate source statistics can be published per release.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingCorpus {
    /// All training pairs in the corpus.
    pub pairs: Vec<TrainingPair>,
    /// Number of source invocation traces.
    pub trace_count: usize,
    /// Aggregate provenance counts across all source traces.
    pub provenance_counts: ProvenanceCounts,
}

impl TrainingCorpus {
    /// Compiles a training corpus from a slice of invocation traces.
    pub fn new(traces: &[InvocationTrace]) -> Self {
        let mut pairs = Vec::new();
        for trace in traces {
            pairs.extend(compile_training_pairs(trace));
        }
        Self {
            pairs,
            trace_count: traces.len(),
            provenance_counts: ProvenanceCounts::from_traces(traces),
        }
    }

    /// Number of needle pairs (user-constraint blocks) in the corpus.
    pub fn needle_pair_count(&self) -> usize {
        self.pairs
            .iter()
            .filter(|p| p.features.is_user_constraint)
            .count()
    }

    /// Proportion of pairs where `teacher_label = true`.
    pub fn positive_rate(&self) -> f32 {
        if self.pairs.is_empty() {
            return 0.0;
        }
        let positives = self.pairs.iter().filter(|p| p.teacher_label).count();
        positives as f32 / self.pairs.len() as f32
    }

    /// Returns a summary suitable for release-note publication (exit criterion 3).
    pub fn provenance_summary(&self) -> String {
        let pc = &self.provenance_counts;
        format!(
            "Training corpus: {} pairs from {} traces \
             (live={}, synthetic={}, public_dataset={})",
            self.pairs.len(),
            self.trace_count,
            pc.live_traces,
            pc.synthetic,
            pc.public_dataset,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::{BlockFeatures, BlockRole};
    use crate::trace::{BlockTraceRecord, InvocationTrace, TraceProvenance};

    fn make_trace(
        id: &str,
        blocks: Vec<(bool, bool, bool)>, // (is_needle, is_error, retained)
    ) -> InvocationTrace {
        InvocationTrace {
            invocation_id: id.into(),
            started_at_secs: 0,
            route_id: "frontier".into(),
            blocks: blocks
                .into_iter()
                .enumerate()
                .map(|(i, (is_needle, is_error, retained))| BlockTraceRecord {
                    features: BlockFeatures::new(
                        i, 10, BlockRole::User, is_needle, is_error, false, 0.0,
                    ),
                    gate_score: if retained { 0.9 } else { 0.1 },
                    retained,
                    was_fallback: false,
                    teacher_label: None,
                })
                .collect(),
            provenance: TraceProvenance::Synthetic {
                generator: "unit-test".into(),
                seed: 42,
            },
            turns: 2,
            completed: true,
        }
    }

    #[test]
    fn compile_pairs_assigns_teacher_true_for_needle_blocks() {
        let trace = make_trace("t1", vec![(true, false, false)]);
        let pairs = compile_training_pairs(&trace);
        assert_eq!(pairs.len(), 1);
        assert!(pairs[0].teacher_label, "needle block should have teacher_label=true");
    }

    #[test]
    fn compile_pairs_assigns_needle_weight_for_needle_blocks() {
        let trace = make_trace("t1", vec![(true, false, false)]);
        let pairs = compile_training_pairs(&trace);
        assert!(
            (pairs[0].loss_weight - TrainingPair::NEEDLE_WEIGHT).abs() < 1e-6,
            "needle block should have NEEDLE_WEIGHT"
        );
    }

    #[test]
    fn compile_pairs_assigns_error_weight_for_error_trace_blocks() {
        let trace = make_trace("t1", vec![(false, true, false)]);
        let pairs = compile_training_pairs(&trace);
        assert!(pairs[0].teacher_label, "error-trace block should have teacher_label=true");
        assert!(
            (pairs[0].loss_weight - TrainingPair::ERROR_TRACE_WEIGHT).abs() < 1e-6,
        );
    }

    #[test]
    fn compile_pairs_assigns_teacher_false_for_evicted_non_special_blocks() {
        let trace = make_trace("t1", vec![(false, false, false)]);
        let pairs = compile_training_pairs(&trace);
        assert!(
            !pairs[0].teacher_label,
            "plain evicted block should have teacher_label=false"
        );
    }

    #[test]
    fn training_corpus_provenance_counts_are_correct() {
        let t1 = make_trace("t1", vec![(true, false, false)]);
        let corpus = TrainingCorpus::new(&[t1]);
        assert_eq!(corpus.trace_count, 1);
        assert_eq!(corpus.provenance_counts.synthetic, 1);
        assert_eq!(corpus.provenance_counts.live_traces, 0);
    }

    #[test]
    fn training_corpus_provenance_summary_contains_all_fields() {
        let t1 = make_trace("t1", vec![(true, false, false), (false, true, true)]);
        let corpus = TrainingCorpus::new(&[t1]);
        let summary = corpus.provenance_summary();
        assert!(summary.contains("synthetic=1"), "{summary}");
        assert!(summary.contains("live=0"), "{summary}");
    }

    #[test]
    fn training_corpus_needle_pair_count_is_correct() {
        let trace = make_trace(
            "t1",
            vec![
                (true, false, true),
                (false, false, false),
                (true, false, false),
            ],
        );
        let corpus = TrainingCorpus::new(&[trace]);
        assert_eq!(corpus.needle_pair_count(), 2);
    }

    #[test]
    fn positive_rate_reflects_teacher_label_distribution() {
        // 2 needle + 1 retained plain = 3 positives out of 4 total
        let trace = make_trace(
            "t1",
            vec![
                (true, false, false),   // needle → true
                (false, false, true),   // retained → true
                (false, false, false),  // evicted → false
                (true, false, false),   // needle → true
            ],
        );
        let corpus = TrainingCorpus::new(&[trace]);
        let expected = 3.0 / 4.0;
        assert!(
            (corpus.positive_rate() - expected).abs() < 1e-6,
            "positive_rate = {}, expected {expected}",
            corpus.positive_rate()
        );
    }
}
