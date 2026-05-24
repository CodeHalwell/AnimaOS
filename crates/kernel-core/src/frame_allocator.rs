//! Bounded frame allocator backing the autonomic memory substrate.
//!
//! The allocator tracks a fixed-size pool of physical page frames. All public
//! APIs are safe; the only `unsafe` use is internal bookkeeping that is
//! statically audited (see `SAFETY:` comments).

use core::sync::atomic::{AtomicUsize, Ordering};

/// Represents a contiguous frame allocation handed back to a caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameAllocation {
    /// Index of the first frame in the allocated range.
    pub start_frame: usize,
    /// Number of frames in the allocated range.
    pub frames: usize,
}

impl FrameAllocation {
    /// Creates a bounded frame allocation descriptor.
    pub fn new(start_frame: usize, frames: usize) -> Self {
        Self {
            start_frame,
            frames,
        }
    }
}

/// Errors returned from the frame allocator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameAllocatorError {
    /// Requested allocation exceeded the remaining capacity.
    OutOfMemory,
    /// Requested zero frames; allocation is rejected to keep callers honest.
    ZeroSizedRequest,
}

/// Simple bump-style frame allocator for boot trampoline use.
///
/// This intentionally does not implement deallocation - the boot trampoline
/// only ever grows. A free-list backed allocator can be layered above this
/// primitive for the post-boot kernel.
#[derive(Debug)]
pub struct FrameAllocator {
    capacity: usize,
    next: AtomicUsize,
}

impl FrameAllocator {
    /// Creates a new bump allocator with the given total frame capacity.
    pub const fn new(capacity: usize) -> Self {
        Self {
            capacity,
            next: AtomicUsize::new(0),
        }
    }

    /// Returns the total capacity in frames.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns the number of frames already handed out.
    pub fn allocated(&self) -> usize {
        self.next.load(Ordering::Acquire)
    }

    /// Attempts to allocate `frames` contiguous frames.
    pub fn allocate(&self, frames: usize) -> Result<FrameAllocation, FrameAllocatorError> {
        if frames == 0 {
            return Err(FrameAllocatorError::ZeroSizedRequest);
        }

        // SAFETY-equivalent reasoning (no unsafe needed here): we use
        // `fetch_update` to atomically reserve `frames` slots while ensuring
        // the resulting cursor never exceeds `capacity`.
        let result = self
            .next
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |cur| {
                cur.checked_add(frames).filter(|&end| end <= self.capacity)
            });

        match result {
            Ok(start) => Ok(FrameAllocation::new(start, frames)),
            Err(_) => Err(FrameAllocatorError::OutOfMemory),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocator_hands_out_contiguous_ranges() {
        let allocator = FrameAllocator::new(16);
        let a = allocator.allocate(4).unwrap();
        let b = allocator.allocate(8).unwrap();

        assert_eq!(a.start_frame, 0);
        assert_eq!(a.frames, 4);
        assert_eq!(b.start_frame, 4);
        assert_eq!(b.frames, 8);
        assert_eq!(allocator.allocated(), 12);
    }

    #[test]
    fn allocator_rejects_overflow() {
        let allocator = FrameAllocator::new(4);
        assert!(allocator.allocate(8).is_err());
    }

    #[test]
    fn allocator_rejects_zero_request() {
        let allocator = FrameAllocator::new(4);
        assert_eq!(
            allocator.allocate(0),
            Err(FrameAllocatorError::ZeroSizedRequest)
        );
    }
}
