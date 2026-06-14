//! TurboQuant integration for the KV-cache controller — Epic E2.7 ↔ E5.4.
//!
//! # Purpose
//!
//! This module makes the [`crate::controller::Quantizer`] seam *real*: instead
//! of the identity [`NoQuantizer`](crate::controller::NoQuantizer), the
//! [`TurboQuantizer`] backs the controller with the production vector-quantiser
//! from the `memory` crate ([`memory::turboquant::TurboQuant`], E2.7).
//!
//! # How it fits the controller
//!
//! The controller calls [`Quantizer::similarity`](crate::controller::Quantizer::similarity)
//! with `(block_index, query)` and multiplies the returned value into its gate
//! score (see [`KvController::gate_block`](crate::controller::KvController::gate_block)).
//! The `TurboQuantizer`:
//!
//! 1. Owns a calibrated [`TurboQuant`] instance and a map from `block_index` to
//!    the quantised representation of that block's stored vector
//!    ([`register_block`](TurboQuantizer::register_block)).
//! 2. On [`similarity`](TurboQuantizer::similarity), quantises the incoming
//!    `query` and computes the bias-corrected cosine similarity between query
//!    and the stored block via [`TurboQuant::score_cosine`].
//! 3. Maps the cosine `[-1.0, 1.0]` onto the `[0.0, 1.0]` range the controller
//!    expects (`(cos + 1) / 2`), so anti-correlated blocks score near `0.0`,
//!    orthogonal blocks near `0.5`, and aligned blocks near `1.0`.
//!
//! # Unknown blocks
//!
//! If a `block_index` has never been registered, [`similarity`] returns `1.0`,
//! exactly matching [`NoQuantizer`](crate::controller::NoQuantizer). This makes
//! the quantizer a *transparent* pass-through for blocks the caller has not
//! described, so partial adoption never penalises un-ingested blocks.
//!
//! # Availability
//!
//! `memory::turboquant` is std-only, so this module — and the `TurboQuantizer`
//! type — are compiled only when the `turboquant` feature is enabled (which
//! implies `std`). The default no_std build still sees only
//! [`NoQuantizer`](crate::controller::NoQuantizer).

#![forbid(unsafe_code)]

use std::collections::HashMap;

use memory::turboquant::{QuantizedVector, TurboQuant, TurboQuantConfig};

use crate::controller::Quantizer;

/// TurboQuant-backed [`Quantizer`] for the KV-cache controller (E2.7).
///
/// Stores a quantised representation per `block_index` and computes a
/// normalised cosine similarity against the query at gate time, using the real
/// TurboQuant (de)quantisation primitives from the `memory` crate.
///
/// See the [module documentation](self) for the scoring and unknown-block
/// semantics.
pub struct TurboQuantizer {
    /// The shared, optionally-calibrated quantiser. All registered vectors and
    /// queries are encoded with this instance so their codes are comparable.
    quant: TurboQuant,
    /// Quantised representation of each registered block, keyed by block index.
    blocks: HashMap<usize, QuantizedVector>,
    /// Similarity returned for blocks that have not been registered.
    ///
    /// Defaults to `1.0` so behaviour matches
    /// [`NoQuantizer`](crate::controller::NoQuantizer) for unknown blocks.
    default_similarity: f32,
}

impl TurboQuantizer {
    /// Creates a TurboQuantizer over a fresh [`TurboQuant`] with the given
    /// configuration.
    ///
    /// Returns `None` if the configuration is invalid (e.g. `dim == 0`).
    pub fn new(config: TurboQuantConfig) -> Option<Self> {
        let quant = TurboQuant::new(config).ok()?;
        Some(Self {
            quant,
            blocks: HashMap::new(),
            default_similarity: 1.0,
        })
    }

    /// Creates a TurboQuantizer over an already-constructed (and possibly
    /// pre-calibrated) [`TurboQuant`] instance.
    ///
    /// Use this when the calibration corpus lives on the caller's side.
    pub fn with_quant(quant: TurboQuant) -> Self {
        Self {
            quant,
            blocks: HashMap::new(),
            default_similarity: 1.0,
        }
    }

    /// Overrides the similarity returned for unregistered blocks.
    ///
    /// The default is `1.0` (pass-through, matching
    /// [`NoQuantizer`](crate::controller::NoQuantizer)).
    pub fn with_default_similarity(mut self, default_similarity: f32) -> Self {
        self.default_similarity = default_similarity.clamp(0.0, 1.0);
        self
    }

    /// Registers (or replaces) the stored vector for `block_index`.
    ///
    /// The vector is quantised immediately via [`TurboQuant::encode`] and the
    /// compact representation retained; the input slice is not kept. The vector
    /// must have the quantiser's configured dimension — see
    /// [`TurboQuant::dim`](memory::turboquant::TurboQuant::dim).
    pub fn register_block(&mut self, block_index: usize, vector: &[f32]) {
        let qv = self.quant.encode(vector);
        self.blocks.insert(block_index, qv);
    }

    /// Returns the number of registered blocks.
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Returns `true` if no blocks have been registered.
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Returns the dimension the quantiser expects for registered vectors and
    /// queries.
    pub fn dim(&self) -> usize {
        self.quant.dim()
    }

    /// Maps a cosine similarity `[-1.0, 1.0]` onto `[0.0, 1.0]`.
    #[inline]
    fn cosine_to_unit(cos: f32) -> f32 {
        ((cos + 1.0) * 0.5).clamp(0.0, 1.0)
    }
}

impl Quantizer for TurboQuantizer {
    /// Returns the normalised TurboQuant cosine similarity between `query` and
    /// the stored representation of `block_index`, in `[0.0, 1.0]`.
    ///
    /// Returns the configured default (1.0 unless overridden) when the block
    /// has not been registered or when `query` does not match the quantiser's
    /// dimension — in both cases the quantizer is a transparent pass-through.
    fn similarity(&self, block_index: usize, query: &[f32]) -> f32 {
        let Some(stored) = self.blocks.get(&block_index) else {
            return self.default_similarity;
        };
        if query.len() != self.quant.dim() {
            return self.default_similarity;
        }
        let q_enc = self.quant.encode(query);
        let cos = self.quant.score_cosine(&q_enc, stored);
        Self::cosine_to_unit(cos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::{KvController, NoQuantizer};
    use crate::features::{BlockFeatures, BlockRole};
    use memory::turboquant::{BitDepth, Metric};

    fn config(dim: usize) -> TurboQuantConfig {
        TurboQuantConfig {
            dim,
            bit_depth: BitDepth::Four,
            metric: Metric::Cosine,
            rotation_seed: 1234,
        }
    }

    #[test]
    fn identical_vectors_score_near_one() {
        let mut tq = TurboQuantizer::new(config(64)).unwrap();
        let v: Vec<f32> = (1..=64).map(|i| i as f32).collect();
        tq.register_block(0, &v);
        // Same vector as the query → cosine ≈ 1 → unit ≈ 1.
        let sim = tq.similarity(0, &v);
        assert!(sim > 0.95, "identical vectors should score ~1.0, got {sim}");
    }

    #[test]
    fn divergent_vectors_score_notably_lower() {
        let mut tq = TurboQuantizer::new(config(64)).unwrap();
        let v: Vec<f32> = (1..=64).map(|i| i as f32).collect();
        // Anti-correlated query (points the opposite direction).
        let anti: Vec<f32> = v.iter().map(|&x| -x).collect();
        tq.register_block(0, &v);

        let same = tq.similarity(0, &v);
        let opposite = tq.similarity(0, &anti);
        assert!(
            opposite < same - 0.3,
            "anti-correlated ({opposite}) should score well below aligned ({same})"
        );
        // Opposite direction → cosine ≈ -1 → unit ≈ 0.
        assert!(opposite < 0.1, "anti-correlated should score ~0.0, got {opposite}");
    }

    #[test]
    fn orthogonal_vectors_score_around_a_half() {
        let dim = 64;
        let mut tq = TurboQuantizer::new(config(dim)).unwrap();
        // Two disjoint-support vectors are orthogonal → cosine ≈ 0 → unit ≈ 0.5.
        let mut a = vec![0.0_f32; dim];
        let mut b = vec![0.0_f32; dim];
        for i in 0..dim / 2 {
            a[i] = (i + 1) as f32;
        }
        for i in dim / 2..dim {
            b[i] = (i + 1) as f32;
        }
        tq.register_block(0, &a);
        let sim = tq.similarity(0, &b);
        assert!(
            (sim - 0.5).abs() < 0.2,
            "orthogonal vectors should score ~0.5, got {sim}"
        );
        // And it must be notably lower than the self-similarity.
        let self_sim = tq.similarity(0, &a);
        assert!(sim < self_sim - 0.3, "orthogonal {sim} should be well below self {self_sim}");
    }

    #[test]
    fn unknown_block_returns_default_one() {
        let tq = TurboQuantizer::new(config(64)).unwrap();
        // No block registered → pass-through default (matches NoQuantizer).
        let q = vec![1.0_f32; 64];
        assert_eq!(tq.similarity(99, &q), 1.0);
        assert_eq!(tq.similarity(99, &q), NoQuantizer.similarity(99, &q));
    }

    #[test]
    fn wrong_dimension_query_returns_default() {
        let mut tq = TurboQuantizer::new(config(64)).unwrap();
        tq.register_block(0, &vec![1.0_f32; 64]);
        // Query has the wrong dimension → transparent pass-through.
        let q = vec![1.0_f32; 8];
        assert_eq!(tq.similarity(0, &q), 1.0);
    }

    #[test]
    fn custom_default_is_respected() {
        let tq = TurboQuantizer::new(config(64))
            .unwrap()
            .with_default_similarity(0.25);
        assert_eq!(tq.similarity(7, &vec![1.0_f32; 64]), 0.25);
    }

    /// End-to-end: a TurboQuantizer that scores one block as a poor match must
    /// change the controller's retention ranking versus the NoQuantizer default.
    ///
    /// The query is the 7-element feature vector (what the controller passes as
    /// `query`), so the quantiser is configured for `FEATURE_DIM`.
    #[test]
    fn turboquantizer_changes_retention_ranking_vs_noquantizer() {
        // Two blocks identical in features (including recency) except their
        // index, so the linear gate alone ranks them by the budget tie-break
        // (ascending index). `total_blocks = 1` pins `recency_score = 1.0` for
        // both blocks, so the only difference the gate sees is the index used by
        // the tie-break — not the recency term (`w_recency > 0`), which would
        // otherwise favour the higher index on its own.
        let features = [
            BlockFeatures::new(0, 1, BlockRole::User, true, false, false, 0.2),
            BlockFeatures::new(1, 1, BlockRole::User, true, false, false, 0.2),
        ];

        // Baseline: NoQuantizer (default). With budget 1, the tie-break keeps
        // the lower index (block 0).
        let mut baseline = KvController::with_pre_trained_weights();
        let base = baseline.select_blocks(&features, 1);
        let base_retained: Vec<usize> =
            base.iter().filter(|d| d.retain).map(|d| d.block_index).collect();
        assert_eq!(base_retained, vec![0], "NoQuantizer keeps the lowest index");

        // TurboQuantizer: register block 0 with a representation that is the
        // *opposite direction* of the feature-vector query (so its similarity is
        // ~0, heavily down-weighting block 0). Block 1 is left unregistered, so
        // it keeps the pass-through similarity of 1.0. Now block 1 should win.
        let dim = BlockFeatures::FEATURE_DIM;
        let mut tq = TurboQuantizer::new(config(dim)).unwrap();
        let query0 = features[0].to_vec();
        let opposite: Vec<f32> = query0.iter().map(|&x| -x).collect();
        tq.register_block(0, &opposite);

        let mut quantised = KvController::with_pre_trained_weights().with_quantizer(tq);
        let q = quantised.select_blocks(&features, 1);
        let q_retained: Vec<usize> =
            q.iter().filter(|d| d.retain).map(|d| d.block_index).collect();

        assert_eq!(
            q_retained,
            vec![1],
            "TurboQuantizer down-weights block 0, flipping the ranking to block 1"
        );
        assert_ne!(
            base_retained, q_retained,
            "TurboQuantizer must change the retention ranking vs NoQuantizer"
        );
    }
}
