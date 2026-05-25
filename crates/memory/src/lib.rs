#![forbid(unsafe_code)]

//! Synaptic memory layer implementing the CLS three-tier hierarchy.

pub mod archival;
pub mod decay;
pub mod l2_cache;

pub use archival::{ArchivalStore, ArchivalStoreError, ArchivedItem};
pub use decay::{EmotionalContext, MemoryNode};
pub use l2_cache::ArcCache;

/// Minimal L1 working context manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualContextManager {
    l1_token_count: u32,
    max_context: u32,
}

impl VirtualContextManager {
    /// Creates a new manager with a known L1 token count.
    pub fn new(l1_token_count: u32) -> Self {
        Self {
            l1_token_count,
            max_context: u32::MAX,
        }
    }

    /// Creates a manager with a known token count and an explicit context cap.
    pub fn with_capacity(l1_token_count: u32, max_context: u32) -> Self {
        Self {
            l1_token_count,
            max_context,
        }
    }

    /// Returns active L1 token count.
    pub fn get_l1_token_count(&self) -> u32 {
        self.l1_token_count
    }

    /// Updates active L1 token count, saturating at `max_context`.
    pub fn set_l1_token_count(&mut self, l1_token_count: u32) {
        self.l1_token_count = l1_token_count.min(self.max_context);
    }

    /// Adds `tokens` to the active count, saturating at `max_context`.
    pub fn add_tokens(&mut self, tokens: u32) {
        self.l1_token_count = self
            .l1_token_count
            .saturating_add(tokens)
            .min(self.max_context);
    }

    /// Returns the configured maximum context window.
    pub fn max_context(&self) -> u32 {
        self.max_context
    }
}
