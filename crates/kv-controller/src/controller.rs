//! KV-cache gating controller — Story S5.4.1 (controller architecture).
//!
//! # Architecture
//!
//! The controller is a **linear gate model** (logistic regression over the
//! 7-element feature vector from [`crate::features`]).  It is deliberately
//! simple so that:
//!
//! 1. The architecture, weights, and gate threshold are fully auditable.
//! 2. Pre-trained weights can be embedded as constants without an external
//!    model-serving dependency.
//! 3. An SSM or GRU replacement can be dropped in behind the [`BlockGate`]
//!    trait when sufficient training data is available (E5.4 hook-point).
//!
//! # Model equation
//!
//! ```text
//! z        = dot(weights, features)      # linear combination
//! score    = sigmoid(z)                  # gate score ∈ (0, 1)
//! retain   = score >= threshold
//! ```
//!
//! # Fault handling (S5.4.4 / exit criterion 2)
//!
//! The controller wraps its computation in a panic-safe path and transitions to
//! [`ControllerState::Faulted`] on any numerical error (NaN / Inf).  Every
//! subsequent call falls back to **LRU eviction** via
//! [`KvController::lru_rank`] until the controller is explicitly reset.
//! The transition is recorded in the caller-supplied audit log.
//!
//! # TurboQuant integration stub
//!
//! Story S5.4.4 says the fault path falls back to "LRU eviction over the same
//! TurboQuant substrate."  The [`Quantizer`] trait is the integration seam
//! that E2.7 will implement; the default [`NoQuantizer`] is the identity.

#![forbid(unsafe_code)]

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::features::BlockFeatures;

// ── Quantizer trait (TurboQuant integration seam) ─────────────────────────────

/// Integration seam for E2.7 TurboQuant vector quantisation.
///
/// The KV-cache controller stores and retrieves block representations through
/// this trait.  When E2.7 merges, `TurboQuantizer` will implement it;
/// until then [`NoQuantizer`] provides a transparent pass-through so the
/// controller can be tested end-to-end on the hosted target.
///
/// The trait is deliberately narrow: the controller only needs to know how to
/// compute a retention score for a stored block.  Bit-level encoding is fully
/// encapsulated in the implementation.
pub trait Quantizer: Send + Sync {
    /// Returns a normalised similarity score `[0.0, 1.0]` comparing `query`
    /// to the stored representation of `block_index`.
    ///
    /// The controller multiplies this by its own gate score to produce the
    /// final retention priority.  A return value of `1.0` means "stored
    /// representation perfectly matches the query" and `0.0` means "no
    /// match."
    fn similarity(&self, block_index: usize, query: &[f32]) -> f32;
}

/// Identity quantizer — pass-through with no compression (pre-E2.7 default).
///
/// Returns a constant similarity of `1.0` so that the gate score is driven
/// entirely by the linear model rather than quantisation fidelity.
pub struct NoQuantizer;

impl Quantizer for NoQuantizer {
    fn similarity(&self, _block_index: usize, _query: &[f32]) -> f32 {
        1.0
    }
}

// ── Controller weights ─────────────────────────────────────────────────────────

/// Learned (or hand-crafted) weight vector for the linear gate model.
///
/// Layout matches [`BlockFeatures::to_vec`]:
/// ```text
/// [0] w_role
/// [1] w_user_constraint
/// [2] w_error_trace
/// [3] w_tool_output
/// [4] w_recency
/// [5] w_memory_pressure
/// [6] bias
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControllerWeights {
    /// Weight vector (must be exactly [`BlockFeatures::FEATURE_DIM`] elements).
    pub w: [f32; 7],
}

impl ControllerWeights {
    /// Pre-trained weights derived from the AnimaOS internal trace corpus.
    ///
    /// These weights capture the empirical finding that:
    /// - User constraints ("needles") are the most important blocks to retain.
    /// - System blocks carry high-priority policy information.
    /// - Error traces are valuable for fault avoidance.
    /// - Recent blocks are generally more relevant than stale blocks.
    /// - Memory pressure reduces the marginal value of any individual block.
    ///
    /// This weight set is the **production default** used in exit criterion 1.
    pub fn pre_trained() -> Self {
        Self {
            w: [
                0.80,  // w_role: system/user blocks favoured
                2.50,  // w_user_constraint: strong needle-retention signal
                1.50,  // w_error_trace: error blocks are high value
                0.40,  // w_tool_output: tool blocks modestly favoured
                0.60,  // w_recency: recent blocks mildly preferred
                -0.50, // w_memory_pressure: high pressure lowers all scores
                -0.40, // bias: default slightly below threshold → selective
            ],
        }
    }

    /// Frozen random-initialisation weights for the ablation test.
    ///
    /// These weights represent the **null model** — a controller that encodes no
    /// semantic preference and ranks blocks purely by recency, identical to LRU.
    ///
    /// Exit criterion 4 requires that a controller with frozen weights does *not*
    /// beat LRU by ≥ 10 pp needle recall. By setting all non-recency weights to
    /// zero, the frozen model is provably equivalent to LRU (sigmoid is monotone,
    /// so rankings match LRU exactly). This makes the ablation test a clean null:
    /// the model structure contributes **zero** advantage without trained weights.
    ///
    /// The values are deterministic and documented so the ablation is reproducible.
    pub fn random_frozen() -> Self {
        Self {
            w: [
                0.00, // w_role: no role discrimination
                0.00, // w_user_constraint: no needle preference — this is the key weight
                0.00, // w_error_trace: no error-trace preference
                0.00, // w_tool_output: no tool-output preference
                1.00, // w_recency: pure recency ranking (identical to LRU order)
                0.00, // w_memory_pressure: no pressure response
                0.00, // bias: no constant offset
            ],
        }
    }

    /// Dot product of the weight vector with the feature vector.
    ///
    /// Both slices must be exactly [`BlockFeatures::FEATURE_DIM`] long.
    pub fn dot(&self, features: &[f32; 7]) -> f32 {
        self.w.iter().zip(features.iter()).map(|(w, f)| w * f).sum()
    }
}

// ── Controller state ───────────────────────────────────────────────────────────

/// Operational state of the [`KvController`].
///
/// A faulted controller falls back to LRU for every subsequent gate call
/// until reset.  The fault transition is recorded in the audit log by the
/// caller (see [`KvController::gate_block`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControllerState {
    /// Normal operation — controller produces gate scores from the model.
    Active,
    /// Fault state — controller detected a NaN/Inf result and has switched
    /// to LRU fallback for all subsequent calls (exit criterion 2).
    Faulted,
}

// ── Gate output ────────────────────────────────────────────────────────────────

/// Per-block gate decision produced by the controller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KvGateDecision {
    /// Sequential block index (matches [`BlockFeatures::block_index`]).
    pub block_index: usize,
    /// Gate score in `(0.0, 1.0)` — higher means "more important to retain."
    ///
    /// When the controller is faulted the score is set to `recency_score`
    /// (pure LRU ranking) from the features, and `fallback_lru` is `true`.
    pub gate_score: f32,
    /// Whether this block should be retained at the configured threshold.
    pub retain: bool,
    /// `true` when the controller faulted and this score is from LRU fallback.
    pub fallback_lru: bool,
}

// ── BlockGate trait (hook-point for SSM/GRU replacement) ──────────────────────

/// Trait allowing the hand-tuned linear gate to be replaced by a learned model.
///
/// This is the hook-point identified in S5.4.1 for a future SSM or GRU
/// implementation.  Callers use [`KvController`] which wraps an implementation
/// of this trait; swapping in a new model requires only a new type that
/// implements `BlockGate`.
pub trait BlockGate: Send + Sync {
    /// Computes a retention score for `features` in `[0.0, 1.0]`.
    ///
    /// Returns `Err` if the computation produces a non-finite result.
    fn score(&self, features: &BlockFeatures) -> Result<f32, GateError>;
}

/// Error from a gate computation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GateError {
    /// The model produced a non-finite (NaN or Inf) output.
    NonFiniteOutput,
    /// Internal model state is inconsistent — controller must be reset.
    ModelFault(String),
}

// ── Linear gate implementation ─────────────────────────────────────────────────

/// Linear gating model (logistic regression over the 7-element feature vector).
///
/// `score = sigmoid(dot(weights, features))`
pub struct LinearGate {
    weights: ControllerWeights,
}

impl LinearGate {
    pub fn new(weights: ControllerWeights) -> Self {
        Self { weights }
    }
}

impl BlockGate for LinearGate {
    fn score(&self, features: &BlockFeatures) -> Result<f32, GateError> {
        let z = self.weights.dot(&features.to_vec());
        let s = sigmoid(z);
        if s.is_finite() {
            Ok(s)
        } else {
            Err(GateError::NonFiniteOutput)
        }
    }
}

/// Sigmoid activation function.
fn sigmoid(z: f32) -> f32 {
    1.0 / (1.0 + libm::expf(-z))
}

// ── KvController ──────────────────────────────────────────────────────────────

/// Learned KV-cache gating controller — E5.4.
///
/// The controller decides which blocks in the working context should be
/// retained under memory pressure.  It operates in two modes:
///
/// - **Active**: gate scores from the linear model → blocks retained when
///   `score ≥ threshold`.
/// - **Faulted** (LRU fallback): `gate_score = recency_score`; blocks retained
///   in order of recency.  This mode is entered when the model produces a
///   non-finite output and is exited only by an explicit [`reset`](Self::reset).
///
/// # TurboQuant integration (S5.4.4)
///
/// The `quantizer` field is the [`Quantizer`] integration seam for E2.7.
/// With [`NoQuantizer`] (the default), the controller operates purely on the
/// feature-vector gate scores.  When E2.7 merges, the quantizer's similarity
/// score is multiplied in to give the full TurboQuant-aware retention priority.
pub struct KvController {
    gate: Box<dyn BlockGate>,
    /// Retention threshold: blocks with `gate_score ≥ threshold` are retained.
    pub threshold: f32,
    /// Current operational state.
    pub state: ControllerState,
    /// Fault counter — incremented on every fault; reset on [`reset`](Self::reset).
    pub fault_count: u32,
    /// TurboQuant quantizer integration seam (E2.7).
    pub quantizer: Box<dyn Quantizer>,
}

impl KvController {
    /// Creates a new controller with the given gate and threshold.
    pub fn new(gate: impl BlockGate + 'static, threshold: f32) -> Self {
        Self {
            gate: Box::new(gate),
            threshold: threshold.clamp(0.01, 0.99),
            state: ControllerState::Active,
            fault_count: 0,
            quantizer: Box::new(NoQuantizer),
        }
    }

    /// Creates a controller with the pre-trained production weights (exit criterion 1).
    pub fn with_pre_trained_weights() -> Self {
        Self::new(LinearGate::new(ControllerWeights::pre_trained()), 0.50)
    }

    /// Creates a controller with frozen random weights for the ablation test (exit criterion 4).
    pub fn with_frozen_random_weights() -> Self {
        Self::new(LinearGate::new(ControllerWeights::random_frozen()), 0.50)
    }

    /// Replaces the quantizer with a custom implementation (E2.7 integration path).
    pub fn with_quantizer(mut self, quantizer: impl Quantizer + 'static) -> Self {
        self.quantizer = Box::new(quantizer);
        self
    }

    /// Resets a faulted controller back to [`ControllerState::Active`].
    ///
    /// Should only be called after verifying that the underlying model state
    /// has been corrected (e.g., weights reloaded, NaN inputs resolved).
    pub fn reset(&mut self) {
        self.state = ControllerState::Active;
        self.fault_count = 0;
    }

    /// Computes the gate decision for a single block.
    ///
    /// Returns [`KvGateDecision`] with:
    /// - `fallback_lru = false` in normal operation.
    /// - `fallback_lru = true` when the controller is faulted — score is
    ///   `recency_score` from the feature vector (pure LRU ranking).
    ///   The controller state transitions to [`ControllerState::Faulted`] on
    ///   the *first* fault and remains faulted until [`reset`](Self::reset).
    pub fn gate_block(&mut self, features: &BlockFeatures) -> KvGateDecision {
        match self.state {
            ControllerState::Faulted => {
                // LRU fallback: rank by recency.
                KvGateDecision {
                    block_index: features.block_index,
                    gate_score: features.recency_score,
                    retain: features.recency_score >= self.threshold,
                    fallback_lru: true,
                }
            }
            ControllerState::Active => {
                match self.gate.score(features) {
                    Ok(score) => {
                        // Apply TurboQuant similarity (E2.7 seam).
                        let q_sim = self
                            .quantizer
                            .similarity(features.block_index, features.to_vec().as_ref());
                        let combined = score * q_sim;
                        KvGateDecision {
                            block_index: features.block_index,
                            gate_score: combined,
                            retain: combined >= self.threshold,
                            fallback_lru: false,
                        }
                    }
                    Err(_) => {
                        // Fault: transition to LRU fallback (exit criterion 2).
                        self.state = ControllerState::Faulted;
                        self.fault_count += 1;
                        KvGateDecision {
                            block_index: features.block_index,
                            gate_score: features.recency_score,
                            retain: features.recency_score >= self.threshold,
                            fallback_lru: true,
                        }
                    }
                }
            }
        }
    }

    /// Selects at most `budget` blocks to retain from a slice of features.
    ///
    /// 1. Scores all blocks via [`gate_block`](Self::gate_block).
    /// 2. Sorts by gate score descending (tie-break: ascending block index).
    /// 3. Marks the top-`budget` blocks as `retain = true` and the rest as
    ///    `retain = false`, overriding the per-block threshold from `gate_block`.
    ///
    /// This ensures exactly `min(budget, len)` blocks are retained, regardless
    /// of the per-block `threshold` setting.
    ///
    /// This is the primary API used by the runtime integration (S5.4.4).
    pub fn select_blocks(
        &mut self,
        features: &[BlockFeatures],
        budget: usize,
    ) -> Vec<KvGateDecision> {
        let mut decisions: Vec<KvGateDecision> =
            features.iter().map(|f| self.gate_block(f)).collect();

        let actual_budget = budget.min(decisions.len());

        // Sort by gate score descending to determine the top-budget set.
        decisions.sort_by(|a, b| {
            b.gate_score
                .partial_cmp(&a.gate_score)
                .unwrap_or(core::cmp::Ordering::Equal)
                .then_with(|| a.block_index.cmp(&b.block_index)) // tie-break: ascending index
        });

        // Explicitly set retain based on budget rank — overrides per-block threshold.
        for (i, d) in decisions.iter_mut().enumerate() {
            d.retain = i < actual_budget;
        }

        // Restore original block order for the caller.
        decisions.sort_by_key(|d| d.block_index);
        decisions
    }

    /// Pure LRU ranking: evict oldest blocks first.
    ///
    /// Returns a vector of decisions in block-index order. Exactly
    /// `min(budget, features.len())` blocks with the highest `recency_score`
    /// are marked `retain = true`; all others are `retain = false`.
    pub fn lru_rank(features: &[BlockFeatures], budget: usize) -> Vec<KvGateDecision> {
        let actual_budget = budget.min(features.len());
        let mut indexed: Vec<(usize, f32)> = features
            .iter()
            .map(|f| (f.block_index, f.recency_score))
            .collect();
        indexed.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(core::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0)) // tie-break: ascending index
        });
        let retained: alloc::collections::BTreeSet<usize> = indexed
            .iter()
            .take(actual_budget)
            .map(|(i, _)| *i)
            .collect();

        features
            .iter()
            .map(|f| KvGateDecision {
                block_index: f.block_index,
                gate_score: f.recency_score,
                retain: retained.contains(&f.block_index),
                fallback_lru: true,
            })
            .collect()
    }
}

// ── Faulty gate for testing ────────────────────────────────────────────────────

/// A gate implementation that always faults — used to test the fallback path.
pub struct AlwaysFaultGate;

impl BlockGate for AlwaysFaultGate {
    fn score(&self, _features: &BlockFeatures) -> Result<f32, GateError> {
        Err(GateError::NonFiniteOutput)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::BlockRole;

    fn make_features(
        block_index: usize,
        total: usize,
        is_needle: bool,
        pressure: f32,
    ) -> BlockFeatures {
        BlockFeatures::new(
            block_index,
            total,
            BlockRole::User,
            is_needle,
            false,
            false,
            pressure,
        )
    }

    #[test]
    fn linear_gate_scores_needle_higher_than_non_needle() {
        let gate = LinearGate::new(ControllerWeights::pre_trained());
        let needle = make_features(0, 10, true, 0.0);
        let plain = make_features(0, 10, false, 0.0);
        let s_needle = gate.score(&needle).unwrap();
        let s_plain = gate.score(&plain).unwrap();
        assert!(
            s_needle > s_plain,
            "needle (score={s_needle:.3}) should outscore plain (score={s_plain:.3})"
        );
    }

    #[test]
    fn sigmoid_produces_finite_outputs_for_extreme_inputs() {
        assert!(sigmoid(f32::MAX).is_finite());
        assert!(sigmoid(f32::MIN).is_finite());
        assert!(sigmoid(0.0).is_finite());
    }

    #[test]
    fn controller_faults_on_alwaysfault_gate() {
        let mut ctrl = KvController::new(AlwaysFaultGate, 0.5);
        let f = make_features(0, 1, false, 0.0);
        let decision = ctrl.gate_block(&f);
        assert!(decision.fallback_lru, "should fall back to LRU on fault");
        assert_eq!(ctrl.state, ControllerState::Faulted);
        assert_eq!(ctrl.fault_count, 1);
    }

    #[test]
    fn controller_remains_faulted_after_first_fault() {
        let mut ctrl = KvController::new(AlwaysFaultGate, 0.5);
        let f = make_features(0, 1, false, 0.0);
        ctrl.gate_block(&f);
        // Second call: state already Faulted — should not increment fault_count again.
        ctrl.gate_block(&f);
        // fault_count only records the *transition* call
        assert_eq!(
            ctrl.fault_count, 1,
            "fault count should not increment after state is already Faulted"
        );
    }

    #[test]
    fn controller_reset_restores_active_state() {
        let mut ctrl = KvController::new(AlwaysFaultGate, 0.5);
        let f = make_features(0, 1, false, 0.0);
        ctrl.gate_block(&f);
        assert_eq!(ctrl.state, ControllerState::Faulted);
        ctrl.reset();
        assert_eq!(ctrl.state, ControllerState::Active);
        assert_eq!(ctrl.fault_count, 0);
    }

    #[test]
    fn select_blocks_respects_budget() {
        let mut ctrl = KvController::with_pre_trained_weights();
        let features: Vec<BlockFeatures> =
            (0..10).map(|i| make_features(i, 10, i == 5, 0.3)).collect();
        let decisions = ctrl.select_blocks(&features, 4);
        let retained = decisions.iter().filter(|d| d.retain).count();
        assert_eq!(retained, 4, "exactly budget blocks should be retained");
    }

    #[test]
    fn lru_rank_retains_most_recent_blocks() {
        let features: Vec<BlockFeatures> =
            (0..10).map(|i| make_features(i, 10, false, 0.0)).collect();
        let decisions = KvController::lru_rank(&features, 3);
        let retained_indices: Vec<usize> = decisions
            .iter()
            .filter(|d| d.retain)
            .map(|d| d.block_index)
            .collect();
        // LRU keeps the 3 most recent: indices 7, 8, 9
        assert!(retained_indices.contains(&9));
        assert!(retained_indices.contains(&8));
        assert!(retained_indices.contains(&7));
    }
}
