//! Boot-time bump heap allocator for the bare-metal microVM kernel.
//!
//! [`BumpAllocator`] implements [`core::alloc::GlobalAlloc`] so it can be
//! registered as the Rust global allocator (`#[global_allocator]`) in any
//! binary that does not have access to an OS heap.  The allocator is
//! **non-deallocating**: `dealloc` is a no-op.  This is appropriate for
//! the Stage 4 boot trampoline, which performs one-time initialisation and
//! never reclaims memory.
//!
//! # Usage
//!
//! ```no_run
//! use corpus::BumpAllocator;
//! use core::alloc::Layout;
//!
//! // Static heap – 256 KiB.
//! static mut HEAP: [u8; 262_144] = [0u8; 262_144];
//!
//! #[global_allocator]
//! static ALLOCATOR: BumpAllocator = BumpAllocator::new();
//!
//! fn main() {
//!     unsafe {
//!         ALLOCATOR.init(HEAP.as_mut_ptr(), HEAP.len());
//!     }
//!     // Heap allocations (Box, Vec, …) are now available.
//! }
//! ```
//!
//! # Safety invariants
//!
//! * `init` must be called exactly once before any allocation, on a memory
//!   region that is exclusively owned by the allocator for its entire lifetime.
//! * The allocator is not safe to use from multiple threads simultaneously
//!   unless the caller guarantees that `init` completes before any allocation
//!   begins (a single-CPU boot path satisfies this).

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::{AtomicUsize, Ordering};

/// Alignment helper: rounds `addr` up to the next multiple of `align`.
///
/// `align` must be a power of two; the result is undefined otherwise.
#[inline]
fn align_up(addr: usize, align: usize) -> usize {
    // SAFETY-equivalent: power-of-two bit-mask trick, no unsafe required.
    (addr.wrapping_add(align).wrapping_sub(1)) & !(align.wrapping_sub(1))
}

/// A lock-free bump allocator suitable for boot-time heap use in `no_std`
/// environments.
///
/// The allocator manages a contiguous byte region `[heap_start, heap_start +
/// heap_size)`.  Allocations are served by atomically advancing an internal
/// cursor; deallocation is intentionally unsupported.
///
/// # Thread safety
///
/// The bump cursor uses an [`AtomicUsize`] with `AcqRel` / `Acquire` ordering,
/// so concurrent calls to `alloc` are safe on multi-CPU systems.  However,
/// `init` must complete (with a `Release`-store) before any thread calls
/// `alloc`.
pub struct BumpAllocator {
    /// Base address of the owned heap region (set by `init`).
    heap_start: UnsafeCell<usize>,
    /// One-past-the-end address of the owned heap region (set by `init`).
    heap_end: UnsafeCell<usize>,
    /// Byte offset from `heap_start` of the next free byte.
    cursor: AtomicUsize,
}

// SAFETY: The only mutability in `BumpAllocator` is the atomic cursor and the
// `UnsafeCell` fields set once by `init`.  All concurrent allocations go through
// the `AtomicUsize`; callers must ensure `init` is not concurrent with `alloc`.
unsafe impl Send for BumpAllocator {}
unsafe impl Sync for BumpAllocator {}

impl Default for BumpAllocator {
    /// Creates a new, uninitialised bump allocator (alias for [`BumpAllocator::new`]).
    fn default() -> Self {
        Self::new()
    }
}

impl BumpAllocator {
    /// Creates a new, uninitialised bump allocator.
    ///
    /// Call [`BumpAllocator::init`] before issuing any allocation.
    pub const fn new() -> Self {
        Self {
            heap_start: UnsafeCell::new(0),
            heap_end: UnsafeCell::new(0),
            cursor: AtomicUsize::new(0),
        }
    }

    /// Returns the heap start address previously passed to `init`, or `0` if
    /// `init` has not been called.
    pub fn heap_start(&self) -> usize {
        // SAFETY: `heap_start` is only written by `init` before any allocation;
        // reads here observe a stable value once `init` has completed.
        unsafe { *self.heap_start.get() }
    }

    /// Returns the number of bytes allocated so far.
    pub fn allocated_bytes(&self) -> usize {
        self.cursor.load(Ordering::Acquire)
    }

    /// Initialises the allocator over the region `[heap_start, heap_start +
    /// heap_size)`.
    ///
    /// # Safety
    ///
    /// * The region must be valid for reads and writes.
    /// * The region must be exclusively owned by this allocator for its
    ///   entire lifetime.
    /// * `init` must be called at most once and must complete before any
    ///   thread calls `alloc`.
    pub unsafe fn init(&self, heap_start: *mut u8, heap_size: usize) {
        // SAFETY: Caller guarantees exclusive access and that `init` is not
        // called concurrently with `alloc`.
        *self.heap_start.get() = heap_start as usize;
        *self.heap_end.get() = (heap_start as usize).saturating_add(heap_size);
        self.cursor.store(0, Ordering::Release);
    }
}

unsafe impl GlobalAlloc for BumpAllocator {
    /// Allocates `layout.size()` bytes with at least `layout.align()` alignment.
    ///
    /// Returns a null pointer when the heap is exhausted.
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: `heap_start` and `heap_end` are only written by `init`;
        // by the time `alloc` is called, they are stable.
        let heap_start = *self.heap_start.get();
        let heap_end = *self.heap_end.get();

        if heap_start == 0 {
            // Allocator not yet initialised — return null rather than corrupt
            // address-zero.
            return ptr::null_mut();
        }

        // Use `fetch_update` to atomically claim the aligned byte range.
        let result = self
            .cursor
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |cursor| {
                let alloc_start = align_up(heap_start.wrapping_add(cursor), layout.align());
                let offset = alloc_start.wrapping_sub(heap_start);
                let alloc_end = offset.checked_add(layout.size())?;
                if heap_start.wrapping_add(alloc_end) > heap_end {
                    return None; // Out of heap space.
                }
                Some(alloc_end)
            });

        match result {
            Ok(old_cursor) => {
                let alloc_start = align_up(heap_start.wrapping_add(old_cursor), layout.align());
                alloc_start as *mut u8
            }
            Err(_) => ptr::null_mut(),
        }
    }

    /// No-op: bump allocators do not reclaim memory.
    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Intentional no-op — see module-level documentation.
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use core::alloc::Layout;

    /// Build a small `BumpAllocator` over a stack-allocated array.
    ///
    /// This is fine in tests because the test binary links `std` and owns a
    /// full stack; `HEAP` is alive for the duration of the test.
    fn make_allocator(heap: &mut [u8]) -> BumpAllocator {
        let alloc = BumpAllocator::new();
        unsafe {
            alloc.init(heap.as_mut_ptr(), heap.len());
        }
        alloc
    }

    #[test]
    fn bump_allocator_returns_non_null_for_reasonable_request() {
        let mut heap = [0u8; 1024];
        let alloc = make_allocator(&mut heap);
        let layout = Layout::from_size_align(64, 8).unwrap();
        let ptr = unsafe { alloc.alloc(layout) };
        assert!(
            !ptr.is_null(),
            "allocator should satisfy a 64-byte/8-align request from a 1 KiB heap"
        );
    }

    #[test]
    fn bump_allocator_advances_cursor_between_allocations() {
        let mut heap = [0u8; 4096];
        let alloc = make_allocator(&mut heap);
        let layout = Layout::from_size_align(128, 8).unwrap();

        let before = alloc.allocated_bytes();
        let _ptr = unsafe { alloc.alloc(layout) };
        let after = alloc.allocated_bytes();

        assert!(after > before, "cursor should advance after an allocation");
        assert!(
            after - before >= 128,
            "cursor should advance by at least the allocation size"
        );
    }

    #[test]
    fn bump_allocator_honours_alignment() {
        let mut heap = [0u8; 4096];
        let alloc = make_allocator(&mut heap);

        for &align in &[1usize, 2, 4, 8, 16, 32, 64] {
            let layout = Layout::from_size_align(align, align).unwrap();
            let ptr = unsafe { alloc.alloc(layout) };
            assert!(
                !ptr.is_null(),
                "allocation with align={align} should succeed"
            );
            assert_eq!(
                ptr as usize % align,
                0,
                "pointer {ptr:p} should be {align}-aligned"
            );
        }
    }

    #[test]
    fn bump_allocator_returns_null_when_heap_exhausted() {
        let mut heap = [0u8; 64];
        let alloc = make_allocator(&mut heap);
        let layout = Layout::from_size_align(128, 1).unwrap();
        let ptr = unsafe { alloc.alloc(layout) };
        assert!(
            ptr.is_null(),
            "allocation larger than the heap should return null"
        );
    }

    #[test]
    fn bump_allocator_returns_null_before_init() {
        let alloc = BumpAllocator::new();
        let layout = Layout::from_size_align(8, 8).unwrap();
        let ptr = unsafe { alloc.alloc(layout) };
        assert!(ptr.is_null(), "un-initialised allocator should return null");
    }

    #[test]
    fn bump_allocator_dealloc_is_a_noop() {
        let mut heap = [0u8; 256];
        let alloc = make_allocator(&mut heap);
        let layout = Layout::from_size_align(32, 8).unwrap();
        let ptr = unsafe { alloc.alloc(layout) };
        assert!(!ptr.is_null());
        let cursor_before = alloc.allocated_bytes();
        // Dealloc should not panic and should not change the cursor.
        unsafe { alloc.dealloc(ptr, layout) };
        assert_eq!(
            alloc.allocated_bytes(),
            cursor_before,
            "dealloc should not change the cursor"
        );
    }

    #[test]
    fn bump_allocator_sequential_allocations_do_not_overlap() {
        let mut heap = [0u8; 2048];
        let alloc = make_allocator(&mut heap);
        let layout = Layout::from_size_align(256, 8).unwrap();

        let p1 = unsafe { alloc.alloc(layout) };
        let p2 = unsafe { alloc.alloc(layout) };
        let p3 = unsafe { alloc.alloc(layout) };

        assert!(!p1.is_null() && !p2.is_null() && !p3.is_null());

        // No two returned pointers should alias.
        assert_ne!(p1, p2);
        assert_ne!(p2, p3);
        assert_ne!(p1, p3);

        // Each range must not overlap: p1 < p2 < p3 and gaps ≥ size.
        assert!(
            unsafe { p2.offset_from(p1) } >= 256,
            "p2 must start at least 256 bytes after p1"
        );
        assert!(
            unsafe { p3.offset_from(p2) } >= 256,
            "p3 must start at least 256 bytes after p2"
        );
    }

    #[test]
    fn align_up_is_identity_for_aligned_inputs() {
        assert_eq!(align_up(0, 8), 0);
        assert_eq!(align_up(8, 8), 8);
        assert_eq!(align_up(16, 4), 16);
        assert_eq!(align_up(64, 64), 64);
    }

    #[test]
    fn align_up_rounds_up_unaligned_inputs() {
        assert_eq!(align_up(1, 8), 8);
        assert_eq!(align_up(9, 8), 16);
        assert_eq!(align_up(3, 4), 4);
        assert_eq!(align_up(5, 16), 16);
    }
}
