#![forbid(unsafe_code)]

//! Reflex loop control: iteration-aware MLFQ scheduler and supporting traits.

pub mod backend;
pub mod mlfq;
pub mod mock;
pub mod token_pipe;

pub use backend::{
    CancellationToken, CompletionFuture, LlmBackend, LlmBackendError, StreamingCompletion,
};
pub use mlfq::{IterationAwareMlfq, MlfqTier, TaskAgenda, TaskOutcome};
pub use mock::MockLlmBackend;
pub use token_pipe::{BoundedTokenPipe, TokenPipeError};

/// Task primitive for autonomous dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// Stable task identifier.
    pub id: u64,
    /// Lower values indicate higher urgency (0 = highest).
    pub mlfq_level: u8,
    /// Prompt to drive the LLM backend on dispatch.
    pub prompt: String,
    /// Optional upper bound on tokens this task may consume in a single
    /// dispatch.  When set, [`IterationAwareMlfq::dispatch_task`] stops
    /// draining the stream once the budget is reached, ensuring per-task
    /// token-slice accounting aligns with the scheduler's resource model.
    pub token_budget: Option<u32>,
}

impl Task {
    /// Convenience constructor with no token budget.
    pub fn new(id: u64, mlfq_level: u8, prompt: impl Into<String>) -> Self {
        Self {
            id,
            mlfq_level,
            prompt: prompt.into(),
            token_budget: None,
        }
    }

    /// Attaches a per-task token budget and returns `self` (builder style).
    pub fn with_token_budget(mut self, budget: u32) -> Self {
        self.token_budget = Some(budget);
        self
    }
}
