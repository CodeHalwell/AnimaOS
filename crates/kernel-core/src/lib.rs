#![deny(missing_docs)]

//! Minimal kernel-core trusted computing base primitives.

/// Represents a frame allocation request in the privileged substrate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameAllocation {
    /// The requested frame count.
    pub frames: usize,
}

impl FrameAllocation {
    /// Creates a bounded frame allocation request.
    pub fn new(frames: usize) -> Self {
        Self { frames }
    }
}
