//! Reflex loop control: iteration-aware MLFQ scheduler and supporting traits.
//!
//! # `no_std` support (E4.5)
//!
//! This crate is fully `no_std`-clean when the `std` feature is disabled
//! (i.e. `default-features = false`).  All public types — [`Task`],
//! [`TaskAgenda`], [`BoundedTokenPipe`], [`MlfqTier`], and the
//! [`LlmBackend`] trait — are available in both modes.
//!
//! The only exclusion in `no_std` builds is [`MockLlmBackend`], which depends
//! on test-only std infrastructure and is gated behind `#[cfg(feature = "std")]`.

#![forbid(unsafe_code)]
#![cfg_attr(not(feature = "std"), no_std)]

// In no_std mode we pull in the `alloc` crate for Vec, String, Box, etc.
#[cfg(not(feature = "std"))]
extern crate alloc;

// Bring common alloc prelude types into scope in no_std mode.
// (In std mode these come from the implicit std prelude.)
#[cfg(not(feature = "std"))]
use alloc::string::String;

pub mod backend;
pub mod mlfq;
#[cfg(feature = "std")]
pub mod mock;
pub mod token_pipe;

pub use backend::{
    CancellationToken, CompletionFuture, LlmBackend, LlmBackendError, StreamingCompletion,
};
pub use mlfq::{IterationAwareMlfq, MlfqTier, TaskAgenda, TaskOutcome};
#[cfg(feature = "std")]
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
