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
//!   All subsequent heap reads and writes occur exclusively through `alloc`
//!   (and the returned pointers); no other code may alias those addresses.
//! * `init` must **complete** — i.e., the `Release` stores to `heap_start`,
//!   `heap_end`, and `cursor` must be visible — before any thread calls
//!   `alloc`.  Concurrent calls to `alloc` are safe once `init` has
//!   returned; concurrent calls to `init` are not.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr;
use core::sync::atomic::{AtomicUsize, Ordering};

/// Alignment helper: rounds `addr` up to the next multiple of `align`.
///
/// `align` must be a power of two; the result is undefined otherwise.
///
/// Wrapping arithmetic is used intentionally: the caller is responsible for
/// ensuring that `addr + align - 1` does not require a usize wider than the
/// target architecture.  In practice, `addr` values used here are heap
/// offsets derived from `checked_add`, so the inputs are already bounded.
#[inline]
fn align_up(addr: usize, align: usize) -> usize {
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
/// All three internal fields — `heap_start`, `heap_end`, and `cursor` — are
/// `AtomicUsize`:
///
/// * `init` stores all three with `Release` ordering.
/// * `alloc` loads `heap_start` and `heap_end` with `Acquire` ordering.
/// * `alloc` updates `cursor` with `AcqRel` / `Acquire` ordering.
///
/// This establishes a proper happens-before edge: every write in `init` is
/// visible to any subsequent `alloc` call.  Concurrent calls to `alloc` are
/// also safe: `cursor` is advanced atomically via `fetch_update(AcqRel)`.
///
/// The only precondition is that `init` completes before any thread calls
/// `alloc` — a single-CPU UEFI boot path trivially satisfies this.
pub struct BumpAllocator {
    /// Base address of the owned heap region (set by `init`).
    heap_start: AtomicUsize,
    /// One-past-the-end address of the owned heap region (set by `init`).
    heap_end: AtomicUsize,
    /// Byte offset from `heap_start` of the next free byte.
    cursor: AtomicUsize,
}

// `BumpAllocator` contains only `AtomicUsize` fields which are `Send + Sync`
// by definition.  The derived `Send + Sync` is therefore sound.
// (No explicit unsafe impl is required — `AtomicUsize: Send + Sync` is
// implemented by the standard library and propagates automatically.)

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
            heap_start: AtomicUsize::new(0),
            heap_end: AtomicUsize::new(0),
            cursor: AtomicUsize::new(0),
        }
    }

    /// Returns the heap start address previously passed to `init`, or `0` if
    /// `init` has not been called.
    pub fn heap_start(&self) -> usize {
        self.heap_start.load(Ordering::Acquire)
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
    /// * The region `[heap_start, heap_start + heap_size)` must be valid for
    ///   reads and writes for the entire lifetime of the allocator.
    /// * The region must be exclusively owned by this allocator: no other
    ///   code may access those addresses except through pointers returned by
    ///   [`GlobalAlloc::alloc`].
    /// * `init` must be called at most once and must **complete** before any
    ///   thread calls [`GlobalAlloc::alloc`].
    pub unsafe fn init(&self, heap_start: *mut u8, heap_size: usize) {
        let start = heap_start as usize;
        let end = start.saturating_add(heap_size);
        // Release stores: all subsequent Acquire loads in `alloc` will
        // observe these values.
        self.heap_start.store(start, Ordering::Release);
        self.heap_end.store(end, Ordering::Release);
        self.cursor.store(0, Ordering::Release);
    }
}

unsafe impl GlobalAlloc for BumpAllocator {
    /// Allocates `layout.size()` bytes with at least `layout.align()` alignment.
    ///
    /// Returns a null pointer when the heap is exhausted or when the allocator
    /// has not yet been initialised.  All arithmetic is overflow-safe:
    /// intermediate overflow is treated as an out-of-memory condition and
    /// results in a null return rather than an out-of-bounds pointer.
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // Acquire loads: pairs with the Release stores in `init`, establishing
        // a happens-before edge for all three atomic fields.
        let heap_start = self.heap_start.load(Ordering::Acquire);
        let heap_end = self.heap_end.load(Ordering::Acquire);

        if heap_start == 0 {
            // Allocator not yet initialised — return null rather than corrupt
            // address zero.
            return ptr::null_mut();
        }

        // Atomically claim an aligned byte range.
        //
        // All intermediate arithmetic is checked (returning None on overflow)
        // so that an adversarially large `layout` cannot bypass the bounds
        // check via address wrapping.
        // The nightly toolchain renames `fetch_update` to `try_update`
        // (unstable `atomic_try_update`); suppress the deprecation so both
        // stable and nightly builds pass until the feature stabilises.
        #[allow(deprecated)]
        let result = self
            .cursor
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |cursor| {
                // Compute the remaining heap size (cannot overflow: end ≥ start).
                let heap_size = heap_end.checked_sub(heap_start)?;
                // Absolute address of the current cursor position.
                let current_addr = heap_start.checked_add(cursor)?;
                // Round up to the required alignment.
                // `align_up` uses wrapping arithmetic; the subsequent
                // `checked_sub` below catches any wrap-around.
                let alloc_start = align_up(current_addr, layout.align());
                // Convert back to a heap-relative offset.
                let offset = alloc_start.checked_sub(heap_start)?;
                // Add the requested size.
                let alloc_end = offset.checked_add(layout.size())?;
                // Reject if the allocation would exceed the heap.
                if alloc_end > heap_size {
                    return None; // Out of heap space.
                }
                Some(alloc_end)
            });

        match result {
            Ok(old_cursor) => {
                // Reconstruct the start address from the cursor value that was
                // in place when we won the CAS.  This uses the same arithmetic
                // as the `fetch_update` closure; `wrapping_add` is safe here
                // because the closure already verified the result is in range.
                let current_addr = heap_start.wrapping_add(old_cursor);
                let alloc_start = align_up(current_addr, layout.align());
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
    /// full stack; `heap` is alive for the duration of the test function.
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

    /// Regression: overflow in alloc_end must return null, not an out-of-bounds
    /// pointer.  A size of `usize::MAX` ensures the addition wraps if unchecked.
    #[test]
    fn bump_allocator_returns_null_for_overflowing_size() {
        let mut heap = [0u8; 256];
        let alloc = make_allocator(&mut heap);
        // Layout::from_size_align with size=usize::MAX would fail validation;
        // use the largest size that passes Layout checks instead.
        let large_size = usize::MAX / 2;
        // SAFETY: Layout::from_size_align validates internally.
        if let Ok(layout) = Layout::from_size_align(large_size, 1) {
            let ptr = unsafe { alloc.alloc(layout) };
            assert!(ptr.is_null(), "overflow-sized allocation must return null");
        }
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
