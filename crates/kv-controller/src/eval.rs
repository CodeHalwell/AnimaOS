//! Evaluation harness — Story S5.4.5.
//!
//! Benchmarks the controller against LRU at a matched block budget on synthetic
//! needle-insertion traces and long-horizon coding session replays.
//!
//! # Exit criteria verified here
//!
//! 1. **Criterion 1**: at a matched block budget, controller + (no quantiser)
//!    beats LRU + (no quantiser) by ≥ 10 pp needle recall. Verified by
//!    [`controller_beats_lru_by_at_least_ten_pp_needle_recall`].
//!
//! 2. **Criterion 4**: frozen random-initialisation weights must *not* beat LRU
//!    by ≥ 10 pp needle recall. Verified by
//!    [`ablation_frozen_weights_do_not_beat_lru_by_more_than_noise`].
//!
//! # Benchmark design
//!
//! The synthetic benchmark inserts **needle blocks** (user constraints that must
//! be retrievable at the end of the task) at positions spread across the context
//! window. Under memory pressure the window must be pruned to `budget` blocks.
//! Needle recall = (needles retained) / (total needles).
//!
//! LRU evicts the oldest blocks regardless of content. The pre-trained
//! controller, which gives `w_user_constraint = 2.50`, strongly prefers needle
//! blocks and achieves near-perfect recall even at tight budgets.

#![forbid(unsafe_code)]

use alloc::vec::Vec;

use crate::controller::KvController;
use crate::features::{BlockFeatures, BlockRole};

// ── Benchmark result ───────────────────────────────────────────────────────────

/// Result of a needle-recall benchmark run.
#[derive(Debug, Clone, PartialEq)]
pub struct NeedleRecallResult {
    /// Total number of needle blocks in the synthetic trace.
    pub total_needles: usize,
    /// Number of needle blocks retained by the strategy under test.
    pub retained_needles: usize,
    /// `retained_needles / total_needles` — the headline metric.
    pub recall: f32,
    /// Block budget used in the benchmark.
    pub budget: usize,
    /// Total blocks in the synthetic trace.
    pub total_blocks: usize,
}

impl NeedleRecallResult {
    /// Recall difference (controller - LRU) in percentage points.
    pub fn recall_advantage_pp(controller: &Self, lru: &Self) -> f32 {
        (controller.recall - lru.recall) * 100.0
    }
}

// ── Synthetic trace builder ────────────────────────────────────────────────────

/// Configuration for the synthetic needle-insertion benchmark.
#[derive(Debug, Clone)]
pub struct NeedleBenchmarkConfig {
    /// Total number of blocks in the synthetic context window.
    pub total_blocks: usize,
    /// Number of needle blocks to insert (spread across the window).
    pub needle_count: usize,
    /// Block budget — context is pruned to this many blocks.
    pub budget: usize,
    /// Memory pressure signal applied to all blocks.
    pub memory_pressure: f32,
    /// If `true`, needles are distributed uniformly across the window.
    /// If `false`, needles are concentrated in the oldest half (worst case for LRU).
    pub needles_in_oldest_half: bool,
}

impl NeedleBenchmarkConfig {
    /// Standard benchmark: 20 total blocks, 5 needles in oldest half, budget of 10.
    ///
    /// This is the configuration used for exit criterion 1. Under this config:
    /// - LRU retains blocks 10–19 (newest), missing all 5 needles in oldest half.
    /// - Pre-trained controller retains all 5 needles → recall = 1.0 vs 0.0 → +100 pp.
    pub fn standard() -> Self {
        Self {
            total_blocks: 20,
            needle_count: 5,
            budget: 10,
            memory_pressure: 0.7,
            needles_in_oldest_half: true,
        }
    }

    /// Builds the synthetic block feature slice for this configuration.
    pub fn build_features(&self) -> Vec<BlockFeatures> {
        assert!(
            self.needle_count <= self.total_blocks,
            "needle_count must not exceed total_blocks"
        );
        // Distribute needles: first `needle_count` blocks in the oldest half,
        // or evenly spread if `needles_in_oldest_half` is false.
        let needle_indices: Vec<usize> = if self.needles_in_oldest_half {
            // Place all needles in the oldest half of the window.
            let half = self.total_blocks / 2;
            let step = (half).max(1) / self.needle_count.max(1);
            (0..self.needle_count).map(|i| i * step.max(1)).collect()
        } else {
            // Distribute evenly across the full window.
            let step = self.total_blocks / self.needle_count.max(1);
            (0..self.needle_count).map(|i| i * step.max(1)).collect()
        };

        let needle_set: alloc::collections::BTreeSet<usize> = needle_indices.into_iter().collect();

        (0..self.total_blocks)
            .map(|i| {
                BlockFeatures::new(
                    i,
                    self.total_blocks,
                    if needle_set.contains(&i) {
                        BlockRole::User
                    } else {
                        BlockRole::Assistant
                    },
                    needle_set.contains(&i),
                    false,
                    false,
                    self.memory_pressure,
                )
            })
            .collect()
    }
}

// ── Benchmark runner ───────────────────────────────────────────────────────────

/// Runs the needle-recall benchmark for a controller.
///
/// Returns the recall achieved by the controller at the configured block budget.
pub fn run_controller_benchmark(
    controller: &mut KvController,
    config: &NeedleBenchmarkConfig,
) -> NeedleRecallResult {
    let features = config.build_features();
    let decisions = controller.select_blocks(&features, config.budget);

    let total_needles = features.iter().filter(|f| f.is_user_constraint).count();
    let retained_needles = decisions
        .iter()
        .zip(features.iter())
        .filter(|(d, f)| d.retain && f.is_user_constraint)
        .count();

    let recall = if total_needles == 0 {
        1.0
    } else {
        retained_needles as f32 / total_needles as f32
    };

    NeedleRecallResult {
        total_needles,
        retained_needles,
        recall,
        budget: config.budget,
        total_blocks: config.total_blocks,
    }
}

/// Runs the needle-recall benchmark using LRU eviction.
pub fn run_lru_benchmark(config: &NeedleBenchmarkConfig) -> NeedleRecallResult {
    let features = config.build_features();
    let decisions = KvController::lru_rank(&features, config.budget);

    let total_needles = features.iter().filter(|f| f.is_user_constraint).count();
    let retained_needles = decisions
        .iter()
        .zip(features.iter())
        .filter(|(d, f)| d.retain && f.is_user_constraint)
        .count();

    let recall = if total_needles == 0 {
        1.0
    } else {
        retained_needles as f32 / total_needles as f32
    };

    NeedleRecallResult {
        total_needles,
        retained_needles,
        recall,
        budget: config.budget,
        total_blocks: config.total_blocks,
    }
}

// ── Feature-slice benchmark runners ───────────────────────────────────────────

/// Runs the needle-recall benchmark on a **pre-built** block-feature slice.
///
/// Unlike [`run_controller_benchmark`], this function does not synthesise its
/// own blocks via `NeedleBenchmarkConfig::build_features()`.  The caller
/// supplies the feature slice directly, enabling benchmarks over real session
/// fixtures that contain mixed needle types (`is_user_constraint`,
/// `is_error_trace`, …).
///
/// `needle_fn` is a predicate that returns `true` for any block that should be
/// counted as a needle in the recall metric.
pub fn run_controller_benchmark_on_features(
    controller: &mut KvController,
    features: &[BlockFeatures],
    budget: usize,
    needle_fn: impl Fn(&BlockFeatures) -> bool,
) -> NeedleRecallResult {
    let decisions = controller.select_blocks(features, budget);

    let total_needles = features.iter().filter(|f| needle_fn(f)).count();
    let retained_needles = decisions
        .iter()
        .zip(features.iter())
        .filter(|(d, f)| d.retain && needle_fn(f))
        .count();

    let recall = if total_needles == 0 {
        1.0
    } else {
        retained_needles as f32 / total_needles as f32
    };

    NeedleRecallResult {
        total_needles,
        retained_needles,
        recall,
        budget,
        total_blocks: features.len(),
    }
}

/// Runs the needle-recall benchmark using LRU eviction on a **pre-built**
/// block-feature slice.
///
/// See [`run_controller_benchmark_on_features`] for the motivation.  `needle_fn`
/// must be the same predicate used for the paired controller benchmark so that
/// the recall advantage metric (`recall_advantage_pp`) is computed on the same
/// needle set.
pub fn run_lru_benchmark_on_features(
    features: &[BlockFeatures],
    budget: usize,
    needle_fn: impl Fn(&BlockFeatures) -> bool,
) -> NeedleRecallResult {
    let decisions = KvController::lru_rank(features, budget);

    let total_needles = features.iter().filter(|f| needle_fn(f)).count();
    let retained_needles = decisions
        .iter()
        .zip(features.iter())
        .filter(|(d, f)| d.retain && needle_fn(f))
        .count();

    let recall = if total_needles == 0 {
        1.0
    } else {
        retained_needles as f32 / total_needles as f32
    };

    NeedleRecallResult {
        total_needles,
        retained_needles,
        recall,
        budget,
        total_blocks: features.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::KvController;

    // ── Exit criterion 1 ─────────────────────────────────────────────────────

    /// At a matched block budget, the pre-trained controller beats LRU by ≥ 10 pp
    /// needle recall on the standard benchmark (exit criterion 1).
    ///
    /// Benchmark setup:
    /// - 20 total blocks, budget = 10 (50% retention)
    /// - 5 needle blocks placed in the oldest 10 blocks (indices 0–4 approx.)
    /// - LRU retains indices 10–19 → misses all 5 needles → recall = 0.0
    /// - Pre-trained controller strongly prefers is_user_constraint blocks
    ///   → retains all 5 needles → recall ≥ 1.0 → advantage ≥ 100 pp
    ///
    /// Note: the test asserts ≥ 10 pp to match the plan's stated requirement;
    /// the actual advantage from the pre-trained weights is ~100 pp.
    #[test]
    fn controller_beats_lru_by_at_least_ten_pp_needle_recall() {
        let config = NeedleBenchmarkConfig::standard();
        let mut ctrl = KvController::with_pre_trained_weights();

        let ctrl_result = run_controller_benchmark(&mut ctrl, &config);
        let lru_result = run_lru_benchmark(&config);

        let advantage_pp = NeedleRecallResult::recall_advantage_pp(&ctrl_result, &lru_result);

        assert!(
            advantage_pp >= 10.0,
            "controller advantage {advantage_pp:.1} pp is less than the required 10 pp \
             (controller recall = {:.3}, LRU recall = {:.3})",
            ctrl_result.recall,
            lru_result.recall,
        );
    }

    // ── Exit criterion 4 (ablation) ───────────────────────────────────────────

    /// Frozen random-initialisation weights must NOT beat LRU by ≥ 10 pp
    /// (exit criterion 4 — ablation test).
    ///
    /// With `w_user_constraint = 0.05` (near-zero) the frozen controller has
    /// essentially the same recency bias as LRU. The advantage is expected to
    /// be well below 10 pp, confirming that the pre-trained weights are
    /// responsible for the improvement, not the model structure itself.
    #[test]
    fn ablation_frozen_weights_do_not_beat_lru_by_more_than_noise() {
        let config = NeedleBenchmarkConfig::standard();
        let mut ctrl = KvController::with_frozen_random_weights();

        let ctrl_result = run_controller_benchmark(&mut ctrl, &config);
        let lru_result = run_lru_benchmark(&config);

        let advantage_pp = NeedleRecallResult::recall_advantage_pp(&ctrl_result, &lru_result);

        assert!(
            advantage_pp < 10.0,
            "frozen-weights controller advantage {advantage_pp:.1} pp exceeds the 10 pp noise \
             threshold — the ablation hypothesis is refuted. \
             Controller recall = {:.3}, LRU recall = {:.3}",
            ctrl_result.recall,
            lru_result.recall,
        );
    }

    // ── Benchmark correctness ─────────────────────────────────────────────────

    #[test]
    fn standard_config_has_expected_needle_distribution() {
        let config = NeedleBenchmarkConfig::standard();
        let features = config.build_features();
        let needles: Vec<usize> = features
            .iter()
            .filter(|f| f.is_user_constraint)
            .map(|f| f.block_index)
            .collect();
        // All needles should be in the oldest half (indices 0–9 for total=20).
        assert_eq!(needles.len(), config.needle_count);
        for &idx in &needles {
            assert!(
                idx < config.total_blocks / 2,
                "needle at index {idx} is not in the oldest half"
            );
        }
    }

    #[test]
    fn lru_evicts_all_needles_in_oldest_half_on_standard_config() {
        let config = NeedleBenchmarkConfig::standard();
        let lru_result = run_lru_benchmark(&config);
        // LRU keeps the 10 newest blocks (indices 10–19), all needles are at 0–9.
        assert_eq!(
            lru_result.retained_needles, 0,
            "LRU should retain 0 needles on the standard config"
        );
        assert!((lru_result.recall - 0.0).abs() < 1e-6);
    }

    #[test]
    fn controller_retains_all_needles_with_pre_trained_weights_on_standard_config() {
        let config = NeedleBenchmarkConfig::standard();
        let mut ctrl = KvController::with_pre_trained_weights();
        let result = run_controller_benchmark(&mut ctrl, &config);
        assert_eq!(
            result.retained_needles, config.needle_count,
            "pre-trained controller should retain all needles"
        );
        assert!((result.recall - 1.0).abs() < 1e-6);
    }

    #[test]
    fn benchmark_respects_block_budget() {
        let config = NeedleBenchmarkConfig::standard();
        let mut ctrl = KvController::with_pre_trained_weights();
        let result = run_controller_benchmark(&mut ctrl, &config);
        let features = config.build_features();
        // Re-run to count retained blocks.
        let decisions = ctrl.select_blocks(&features, config.budget);
        let retained = decisions.iter().filter(|d| d.retain).count();
        assert_eq!(retained, config.budget);
        // The result's budget field should match.
        assert_eq!(result.budget, config.budget);
    }

    /// Evenly-distributed needle benchmark (sanity check — needles across full window).
    #[test]
    fn controller_also_beats_lru_when_needles_are_spread_evenly() {
        let config = NeedleBenchmarkConfig {
            needles_in_oldest_half: false,
            ..NeedleBenchmarkConfig::standard()
        };
        let mut ctrl = KvController::with_pre_trained_weights();
        let ctrl_result = run_controller_benchmark(&mut ctrl, &config);
        let lru_result = run_lru_benchmark(&config);
        // When needles are spread evenly, LRU retains some (those in newer half).
        // The controller should still match or exceed LRU.
        assert!(
            ctrl_result.recall >= lru_result.recall,
            "controller recall {:.3} < LRU recall {:.3} with evenly-distributed needles",
            ctrl_result.recall,
            lru_result.recall
        );
    }
}
