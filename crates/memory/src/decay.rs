//! Emotionally modulated exponential decay model for episodic memory nodes.

// In no_std mode, f32::exp() is provided by the `libm` crate (expf).
// In std mode, f32::exp() resolves via libc's libm — no import needed.
#[cfg(feature = "libm")]
use libm::expf;

/// Absolute semantic floor below which a node's activation cannot decay.
pub const SEMANTIC_FLOOR: f32 = 0.3;

/// Emotional context applied while computing decay.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmotionalContext {
    /// Arousal coefficient (0.0..).
    pub arousal: f32,
    /// Surprise coefficient (0.0..).
    pub surprise: f32,
}

impl Default for EmotionalContext {
    fn default() -> Self {
        Self {
            arousal: 0.0,
            surprise: 0.0,
        }
    }
}

/// Episodic memory node.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryNode {
    /// Baseline activation value at `t = 0`.
    pub initial_activation: f32,
    /// Decay rate (lambda) per time unit.
    pub lambda: f32,
    /// Per-node emotional modulators.
    pub emotion: EmotionalContext,
    /// Arousal weight (alpha).
    pub alpha: f32,
    /// Surprise weight (sigma).
    pub sigma: f32,
}

impl MemoryNode {
    /// Creates a memory node with default emotional weights.
    pub fn new(initial_activation: f32, lambda: f32) -> Self {
        Self {
            initial_activation,
            lambda,
            emotion: EmotionalContext::default(),
            alpha: 1.5,
            sigma: 2.0,
        }
    }

    /// Computes the activation at elapsed time `t`.
    ///
    /// `S(t) = max(S_floor, S_0 * e^{-lambda*t} * (1 + alpha*arousal + sigma*surprise))`
    pub fn activation_at(&self, t: f32) -> f32 {
        let modulator =
            1.0 + self.alpha * self.emotion.arousal + self.sigma * self.emotion.surprise;
        #[cfg(not(feature = "libm"))]
        let exponent = (-self.lambda * t).exp();
        #[cfg(feature = "libm")]
        let exponent = expf(-self.lambda * t);
        let raw = self.initial_activation * exponent * modulator;
        raw.max(SEMANTIC_FLOOR)
    }

    /// Returns true if the node should be evicted at time `t`.
    pub fn should_evict(&self, t: f32, threshold: f32) -> bool {
        self.activation_at(t) <= threshold.max(SEMANTIC_FLOOR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_starts_at_initial_value_at_zero() {
        let node = MemoryNode::new(0.9, 0.1);
        assert!((node.activation_at(0.0) - 0.9).abs() < 1e-6);
    }

    #[test]
    fn activation_floors_at_semantic_floor() {
        let node = MemoryNode::new(0.9, 10.0);
        // After significant time decay, activation should clamp to SEMANTIC_FLOOR.
        assert!((node.activation_at(100.0) - SEMANTIC_FLOOR).abs() < 1e-6);
    }

    #[test]
    fn default_emotional_weights_match_design_constants() {
        // Pins alpha and sigma to the values documented in
        // docs/02-subsystems.md §1.3 (S(t) modulator). Surprise is weighted
        // slightly higher than arousal by design.
        let node = MemoryNode::new(1.0, 0.0);
        assert!((node.alpha - 1.5).abs() < f32::EPSILON);
        assert!((node.sigma - 2.0).abs() < f32::EPSILON);
    }

    #[test]
    fn high_arousal_boosts_activation() {
        let mut excited = MemoryNode::new(0.5, 0.1);
        excited.emotion = EmotionalContext {
            arousal: 5.0,
            surprise: 0.0,
        };
        excited.alpha = 0.5;
        let plain = MemoryNode::new(0.5, 0.1);
        assert!(excited.activation_at(1.0) > plain.activation_at(1.0));
    }
}
