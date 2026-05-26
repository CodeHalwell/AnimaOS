//! Block-level input features for the KV-cache gating controller.
//!
//! Each block in the working context is described by a [`BlockFeatures`]
//! struct. The controller processes these features to produce a gate score
//! indicating whether the block should be retained or evicted.
//!
//! # Feature design rationale
//!
//! The feature set is chosen to capture the semantic importance signals
//! identified in the TurboQuant paper and the AnimaOS cognitive architecture:
//!
//! | Feature               | Role                                                   |
//! |-----------------------|--------------------------------------------------------|
//! | `role`                | System/User/Assistant/Tool (role priority encoding)    |
//! | `is_user_constraint`  | Block contains a user-specified constraint ("needle")  |
//! | `is_error_trace`      | Block contains error/exception information to preserve |
//! | `is_tool_output`      | Block is a tool return value                           |
//! | `recency_score`       | Normalised position: 0.0 = oldest, 1.0 = most recent  |
//! | `memory_pressure`     | Scalar memory pressure signal from interoception       |
//!
//! These map to Stories S5.4.1 (controller architecture input surface) and
//! S5.4.2 (trace capture metadata).

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

// ── Block role ─────────────────────────────────────────────────────────────────

/// The conversational role that generated a KV-cache block.
///
/// Role informs the gate's prior over block importance: system and user blocks
/// tend to carry constraint information, while assistant and tool blocks hold
/// intermediate reasoning that may be safely evicted after summarisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockRole {
    /// System prompt — typically the highest-priority retention target.
    System,
    /// User utterance — carries human-specified constraints ("needles").
    User,
    /// Assistant reasoning / generation — mid-priority, often summarisable.
    Assistant,
    /// Tool return value — lower priority; task-specific.
    Tool,
}

impl BlockRole {
    /// Returns a scalar encoding of the role used by the linear gate model.
    ///
    /// Higher values indicate higher intrinsic importance for retention.
    pub fn to_scalar(self) -> f32 {
        match self {
            Self::System => 1.0,
            Self::User => 0.85,
            Self::Tool => 0.5,
            Self::Assistant => 0.3,
        }
    }
}

// ── Block features ─────────────────────────────────────────────────────────────

/// Input feature vector for a single KV-cache block.
///
/// Constructed per block before each gate evaluation call. The values are all
/// normalised to the `[0.0, 1.0]` range so the linear gating model's weights
/// have consistent scale.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockFeatures {
    /// Unique sequential index of this block within the context window.
    /// Used for deterministic ordering in evaluation; not a gate input.
    pub block_index: usize,

    /// Role of the turn that produced this block.
    pub role: BlockRole,

    /// Block contains at least one user-specified hard constraint.
    ///
    /// This is the primary "needle" signal: blocks with user constraints are
    /// the most important to retain and are the target of the needle-recall
    /// benchmark in [`crate::eval`].
    pub is_user_constraint: bool,

    /// Block contains error or exception trace information.
    ///
    /// Error traces are high-value retention targets because they inform the
    /// cortex about past failures that should not be repeated.
    pub is_error_trace: bool,

    /// Block is a tool invocation return value.
    ///
    /// Tool outputs are worth retaining when they contain unique results not
    /// recoverable by re-running the tool (e.g., real-time data).
    pub is_tool_output: bool,

    /// Normalised recency score for this block.
    ///
    /// `0.0` = oldest block in the window, `1.0` = most recently appended
    /// block. Computed by the caller as `block_index / (total_blocks − 1)`.
    pub recency_score: f32,

    /// Current memory-pressure reading from interoception (`[0.0, 1.0]`).
    ///
    /// Higher pressure signals that the context window is constrained and
    /// more aggressive eviction is warranted. This is a *global* signal shared
    /// by all blocks in a single gate pass.
    pub memory_pressure: f32,
}

impl BlockFeatures {
    /// Constructs a [`BlockFeatures`] from raw block-level metadata.
    ///
    /// `block_index` and `total_blocks` are used to derive `recency_score`;
    /// `total_blocks` must be ≥ 1.
    pub fn new(
        block_index: usize,
        total_blocks: usize,
        role: BlockRole,
        is_user_constraint: bool,
        is_error_trace: bool,
        is_tool_output: bool,
        memory_pressure: f32,
    ) -> Self {
        let recency_score = if total_blocks <= 1 {
            1.0
        } else {
            block_index as f32 / (total_blocks - 1) as f32
        };
        Self {
            block_index,
            role,
            is_user_constraint,
            is_error_trace,
            is_tool_output,
            recency_score: recency_score.clamp(0.0, 1.0),
            memory_pressure: memory_pressure.clamp(0.0, 1.0),
        }
    }

    /// Returns the feature values as a fixed-length float slice.
    ///
    /// Layout (7 elements):
    /// ```text
    /// [0] role scalar          [0.0 .. 1.0]
    /// [1] is_user_constraint   {0.0, 1.0}
    /// [2] is_error_trace       {0.0, 1.0}
    /// [3] is_tool_output       {0.0, 1.0}
    /// [4] recency_score        [0.0 .. 1.0]
    /// [5] memory_pressure      [0.0 .. 1.0]
    /// [6] bias term            1.0 (always)
    /// ```
    pub fn to_vec(&self) -> [f32; 7] {
        [
            self.role.to_scalar(),
            if self.is_user_constraint { 1.0 } else { 0.0 },
            if self.is_error_trace { 1.0 } else { 0.0 },
            if self.is_tool_output { 1.0 } else { 0.0 },
            self.recency_score,
            self.memory_pressure,
            1.0, // bias
        ]
    }

    /// Number of features in the feature vector (including the bias term).
    pub const FEATURE_DIM: usize = 7;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_features_recency_is_zero_for_oldest_block() {
        let f = BlockFeatures::new(0, 10, BlockRole::User, false, false, false, 0.0);
        assert!((f.recency_score - 0.0).abs() < 1e-6);
    }

    #[test]
    fn block_features_recency_is_one_for_most_recent_block() {
        let f = BlockFeatures::new(9, 10, BlockRole::User, false, false, false, 0.0);
        assert!((f.recency_score - 1.0).abs() < 1e-6);
    }

    #[test]
    fn block_features_single_block_has_recency_one() {
        let f = BlockFeatures::new(0, 1, BlockRole::System, false, false, false, 0.0);
        assert!((f.recency_score - 1.0).abs() < 1e-6);
    }

    #[test]
    fn feature_vector_has_correct_dimension() {
        let f = BlockFeatures::new(2, 5, BlockRole::Assistant, true, false, false, 0.5);
        assert_eq!(f.to_vec().len(), BlockFeatures::FEATURE_DIM);
    }

    #[test]
    fn bias_term_is_always_one() {
        let f = BlockFeatures::new(0, 1, BlockRole::Tool, false, false, false, 0.0);
        let v = f.to_vec();
        assert!((v[6] - 1.0).abs() < 1e-6, "bias term must be 1.0");
    }

    #[test]
    fn user_constraint_flag_sets_feature_correctly() {
        let with_constraint =
            BlockFeatures::new(0, 1, BlockRole::User, true, false, false, 0.0);
        let without_constraint =
            BlockFeatures::new(0, 1, BlockRole::User, false, false, false, 0.0);
        assert!((with_constraint.to_vec()[1] - 1.0).abs() < 1e-6);
        assert!((without_constraint.to_vec()[1] - 0.0).abs() < 1e-6);
    }

    #[test]
    fn role_scalars_are_ordered_correctly() {
        assert!(BlockRole::System.to_scalar() > BlockRole::User.to_scalar());
        assert!(BlockRole::User.to_scalar() > BlockRole::Tool.to_scalar());
        assert!(BlockRole::Tool.to_scalar() > BlockRole::Assistant.to_scalar());
    }
}
