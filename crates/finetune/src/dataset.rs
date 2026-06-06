//! S8.4.3 — The consolidation / "dreaming" output format.
//!
//! A [`TrainingPair`] is one prompt→response example. It is the format the
//! dreaming-phase consolidation loop (S8.4.3, E11 S11.5) emits when it distils
//! the agent's episodic experience into a curated dataset that the
//! [`crate::tuner::FineTuner`] then trains on. Wiring episodic memory into this
//! source is a gated research spike; this crate only fixes the *shape* of the
//! data and gives a deterministic [`TrainingSet`] builder so the rest of the
//! pipeline is testable.

use crate::hash::Fnv1a;
use serde::{Deserialize, Serialize};

/// A single supervised fine-tuning example: a prompt and its target response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrainingPair {
    /// The input/instruction shown to the model.
    pub prompt: String,
    /// The desired completion the model should learn to produce.
    pub response: String,
}

impl TrainingPair {
    /// Construct a pair from any string-like prompt and response.
    pub fn new(prompt: impl Into<String>, response: impl Into<String>) -> Self {
        TrainingPair {
            prompt: prompt.into(),
            response: response.into(),
        }
    }

    /// Absorb this pair into a deterministic fingerprint (order matters; the
    /// caller hashes pairs in sequence).
    pub(crate) fn hash_into(&self, h: &mut Fnv1a) {
        h.write_str(&self.prompt);
        h.write_str(&self.response);
    }
}

/// An ordered, immutable collection of [`TrainingPair`]s ready for training.
///
/// Build one with [`TrainingSet::from_pairs`] (or [`TrainingSet::builder`] for
/// incremental construction). The set records a stable
/// [`fingerprint`](TrainingSet::fingerprint) of its contents so a
/// [`crate::tuner::FineTuner`] can derive reproducible adapter ids.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrainingSet {
    pairs: Vec<TrainingPair>,
}

impl TrainingSet {
    /// Build a set from a slice of pairs (the consolidation hook's output).
    pub fn from_pairs(pairs: &[TrainingPair]) -> Self {
        TrainingSet {
            pairs: pairs.to_vec(),
        }
    }

    /// Start an incremental [`TrainingSetBuilder`].
    pub fn builder() -> TrainingSetBuilder {
        TrainingSetBuilder::default()
    }

    /// The pairs in this set, in order.
    pub fn pairs(&self) -> &[TrainingPair] {
        &self.pairs
    }

    /// Number of training pairs.
    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    /// Whether the set has no pairs.
    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    /// A stable, content-derived 64-bit fingerprint of the whole set.
    ///
    /// Identical pair sequences always produce the same fingerprint, on any
    /// machine or Rust version; reordering or editing pairs changes it. This is
    /// the dataset half of a fixture adapter's reproducible id.
    pub fn fingerprint(&self) -> u64 {
        let mut h = Fnv1a::new();
        h.write_u64(self.pairs.len() as u64);
        for p in &self.pairs {
            p.hash_into(&mut h);
        }
        h.finish()
    }
}

/// Incremental builder for a [`TrainingSet`].
#[derive(Debug, Default, Clone)]
pub struct TrainingSetBuilder {
    pairs: Vec<TrainingPair>,
}

impl TrainingSetBuilder {
    /// Append one prompt/response pair.
    pub fn push(mut self, prompt: impl Into<String>, response: impl Into<String>) -> Self {
        self.pairs.push(TrainingPair::new(prompt, response));
        self
    }

    /// Append an existing [`TrainingPair`].
    pub fn push_pair(mut self, pair: TrainingPair) -> Self {
        self.pairs.push(pair);
        self
    }

    /// Extend with many pairs at once.
    pub fn extend(mut self, pairs: impl IntoIterator<Item = TrainingPair>) -> Self {
        self.pairs.extend(pairs);
        self
    }

    /// Finalise into an immutable [`TrainingSet`].
    pub fn build(self) -> TrainingSet {
        TrainingSet { pairs: self.pairs }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<TrainingPair> {
        vec![
            TrainingPair::new("what is 2+2?", "4"),
            TrainingPair::new("capital of France?", "Paris"),
        ]
    }

    #[test]
    fn from_pairs_preserves_order_and_len() {
        let set = TrainingSet::from_pairs(&sample());
        assert_eq!(set.len(), 2);
        assert!(!set.is_empty());
        assert_eq!(set.pairs()[0].response, "4");
        assert_eq!(set.pairs()[1].prompt, "capital of France?");
    }

    #[test]
    fn builder_matches_from_pairs() {
        let built = TrainingSet::builder()
            .push("what is 2+2?", "4")
            .push_pair(TrainingPair::new("capital of France?", "Paris"))
            .build();
        assert_eq!(built, TrainingSet::from_pairs(&sample()));
    }

    #[test]
    fn fingerprint_is_deterministic() {
        let a = TrainingSet::from_pairs(&sample());
        let b = TrainingSet::from_pairs(&sample());
        assert_eq!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn fingerprint_changes_on_reorder() {
        let mut reordered = sample();
        reordered.reverse();
        let a = TrainingSet::from_pairs(&sample());
        let b = TrainingSet::from_pairs(&reordered);
        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn empty_set_is_empty() {
        let set = TrainingSet::from_pairs(&[]);
        assert!(set.is_empty());
        assert_eq!(set.len(), 0);
    }

    #[test]
    fn pair_serde_round_trip() {
        let p = TrainingPair::new("p", "r");
        let json = serde_json::to_string(&p).unwrap();
        let back: TrainingPair = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }
}
