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
}

impl Task {
    /// Convenience constructor.
    pub fn new(id: u64, mlfq_level: u8, prompt: impl Into<String>) -> Self {
        Self {
            id,
            mlfq_level,
            prompt: prompt.into(),
        }
    }
}
