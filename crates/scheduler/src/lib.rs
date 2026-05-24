#![forbid(unsafe_code)]

//! Reflex loop control: iteration-aware MLFQ scheduler and supporting traits.

pub mod backend;
pub mod mlfq;
pub mod token_pipe;

pub use backend::{LlmBackend, LlmBackendError, StreamingCompletion};
pub use mlfq::{IterationAwareMlfq, MlfqTier, TaskAgenda};
pub use token_pipe::{BoundedTokenPipe, TokenPipeError};

/// Task primitive for autonomous dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// Stable task identifier.
    pub id: u64,
    /// Lower values indicate higher urgency (0 = highest).
    pub mlfq_level: u8,
}
