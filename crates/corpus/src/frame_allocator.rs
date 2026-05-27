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

// ---------------------------------------------------------------------------
// Kani formal verification proof harnesses
// ---------------------------------------------------------------------------
//
// These harnesses are compiled only when running `cargo kani` (the `kani` cfg
// flag is set by the Kani tool-chain and is never active in a normal build).
// They prove four key invariants of the bump-style frame allocator:
//
//   1. `allocated()` never exceeds `capacity()` after any allocation attempt.
//   2. `allocate(0)` always returns [`FrameAllocatorError::ZeroSizedRequest`].
//   3. Two consecutive successful allocations produce non-overlapping ranges.
//   4. A successful allocation's end index stays within `capacity`.
//
// Epic E4.6 exit criterion 1: all declared Kani proofs pass in nightly CI.

/// Kani formal verification proofs for [`FrameAllocator`] invariants.
#[cfg(kani)]
mod kani_proofs {
    use super::*;

    /// Prove: `allocated()` ≤ `capacity()` after any single `allocate` call,
    /// starting from an **arbitrary** (possibly non-zero) initial state.
    ///
    /// A symbolic initial allocation puts the allocator into any valid
    /// occupancy before the second call under test, making the proof hold
    /// for all reachable states, not just the fresh-allocator case.
    #[kani::proof]
    fn allocated_never_exceeds_capacity_after_allocate() {
        let capacity: usize = kani::any();
        kani::assume(capacity > 0 && capacity <= 128);

        let allocator = FrameAllocator::new(capacity);

        // Drive the allocator into an arbitrary initial state.
        let initial_alloc: usize = kani::any();
        let _ = allocator.allocate(initial_alloc);

        let n: usize = kani::any();
        kani::assume(n <= 128);

        // Attempt allocation — may succeed or fail.
        let _ = allocator.allocate(n);

        // Invariant holds in both outcomes and from any prior occupancy.
        assert!(
            allocator.allocated() <= allocator.capacity(),
            "allocated must never exceed capacity"
        );
    }

    /// Prove: `allocate(0)` always returns `ZeroSizedRequest`.
    #[kani::proof]
    fn zero_sized_request_always_returns_zero_sized_request_error() {
        let capacity: usize = kani::any();
        kani::assume(capacity <= 128);

        let allocator = FrameAllocator::new(capacity);
        let result = allocator.allocate(0);

        assert!(
            matches!(result, Err(FrameAllocatorError::ZeroSizedRequest)),
            "allocate(0) must always return ZeroSizedRequest"
        );
    }

    /// Prove: two consecutive successful allocations produce non-overlapping
    /// frame ranges.
    ///
    /// If both calls return `Ok`, `a2.start_frame ≥ a1.start_frame + a1.frames`.
    #[kani::proof]
    fn sequential_allocations_produce_non_overlapping_ranges() {
        let capacity: usize = kani::any();
        kani::assume(capacity > 0 && capacity <= 64);

        let allocator = FrameAllocator::new(capacity);

        let n1: usize = kani::any();
        let n2: usize = kani::any();
        kani::assume(n1 > 0 && n1 <= 32);
        kani::assume(n2 > 0 && n2 <= 32);

        if let (Ok(a1), Ok(a2)) = (allocator.allocate(n1), allocator.allocate(n2)) {
            assert!(
                a2.start_frame >= a1.start_frame + a1.frames,
                "sequential allocations must not overlap"
            );
        }
    }

    /// Prove: a successful allocation's range stays within `[0, capacity)`,
    /// starting from an **arbitrary** initial state.
    ///
    /// For every `Ok(alloc)` result: `alloc.start_frame + alloc.frames ≤ capacity`.
    /// The initial symbolic allocation drives the allocator into a non-trivial
    /// occupancy so the proof generalises beyond the fresh-allocator case.
    #[kani::proof]
    fn successful_allocation_stays_within_capacity_bounds() {
        let capacity: usize = kani::any();
        kani::assume(capacity > 0 && capacity <= 128);

        let allocator = FrameAllocator::new(capacity);

        // Drive the allocator into an arbitrary initial state.
        let initial_alloc: usize = kani::any();
        let _ = allocator.allocate(initial_alloc);

        let n: usize = kani::any();
        kani::assume(n > 0 && n <= 128);

        if let Ok(alloc) = allocator.allocate(n) {
            assert!(
                alloc.start_frame + alloc.frames <= capacity,
                "allocation range must not exceed capacity"
            );
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
