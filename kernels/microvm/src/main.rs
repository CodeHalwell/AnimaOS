//! AnimaOS Stage 4 — UEFI boot trampoline (Epic E4.1).
//!
//! This binary demonstrates the three deliverables of E4.1:
//!
//! 1. **S4.1.1 — `no_std`-clean `corpus`**: the corpus crate is imported and its
//!    types are used here without any `std` dependency.
//!
//! 2. **S4.1.2 — Custom allocator integration**: `corpus::BumpAllocator` is
//!    registered as the global Rust allocator (`#[global_allocator]`), backed by
//!    a static 512 KiB byte array.  The `alloc` crate (Vec, Box, …) uses it for
//!    all heap allocations inside this binary.
//!
//! 3. **S4.1.3 — UEFI boot trampoline**: a real UEFI application that boots via
//!    the EFI entry point, prints to the UEFI console, exercises the corpus
//!    substrate (FrameAllocator + BumpAllocator), and then triggers a deliberate
//!    panic.  The panic message `ANIMA_PANIC` is written to the serial console;
//!    the CI QEMU job greps for it to verify the exit criterion.
//!
//! # Exit criterion (E4.1)
//!
//! > "QEMU boots the trampoline image and reaches the panic handler under a
//! >  deliberate panic."
//!
//! The CI job in `.github/workflows/ci.yml` (`uefi-boot`) achieves this by:
//! 1. Building this binary for `x86_64-unknown-uefi`.
//! 2. Placing the resulting `.efi` file in a FAT ESP image.
//! 3. Running `qemu-system-x86_64` with OVMF firmware and `-serial stdio`.
//! 4. Asserting that the string `ANIMA_PANIC` appears in the serial output.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use corpus::{BumpAllocator, FrameAllocator};
use log::{error, info};
use uefi::prelude::*;

// ---------------------------------------------------------------------------
// Stage 4 S4.1.2: corpus BumpAllocator as the global Rust heap.
// ---------------------------------------------------------------------------

/// Static heap for the boot trampoline: 512 KiB is ample for boot-time use.
///
/// # Safety invariant
/// `ALLOCATOR.init` is called once, at the very start of `efi_main`, before
/// any heap allocation can occur.  The UEFI environment is single-threaded at
/// this stage so there is no concurrent initialisation hazard.
// SAFETY: `static mut` is required so we can call `.as_mut_ptr()` in unsafe
// code below.  No other code accesses `HEAP` after `init` returns.
static mut HEAP: [u8; 512 * 1024] = [0u8; 512 * 1024];

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator::new();

// ---------------------------------------------------------------------------
// UEFI entry point
// ---------------------------------------------------------------------------

/// UEFI application entry point.
///
/// The `#[entry]` macro (from the `uefi` crate) sets up the C-ABI `efi_main`
/// symbol required by the UEFI firmware, stores the system table in a global,
/// and calls this function.
#[entry]
fn efi_main() -> Status {
    // -----------------------------------------------------------------
    // 1. Initialise the corpus BumpAllocator.
    //
    //    This must happen before any `alloc` use (Vec, Box, format!, …).
    //    At this point the UEFI firmware has not yet been touched so no
    //    allocation has occurred.
    // -----------------------------------------------------------------
    // SAFETY: `HEAP` is a `static mut` byte array; we have exclusive
    // ownership here because `efi_main` is the first Rust code to run in
    // this binary, and the UEFI single-threaded boot environment guarantees
    // no concurrent access.
    //
    // `addr_of_mut!` obtains a raw pointer to `HEAP` without creating any
    // reference, avoiding the `static_mut_refs` lint.
    unsafe {
        let heap_ptr = core::ptr::addr_of_mut!(HEAP) as *mut u8;
        // SAFETY: `HEAP` is a static array; its length is fixed at compile
        // time.  We use a raw pointer path to avoid a reference to static mut.
        let heap_len = core::mem::size_of::<[u8; 512 * 1024]>();
        ALLOCATOR.init(heap_ptr, heap_len);
    }

    // -----------------------------------------------------------------
    // 2. Initialise the uefi helpers (console logger, system table ref).
    // -----------------------------------------------------------------
    // Ignore errors at this early boot stage; if the console cannot be
    // initialised we will still reach the panic handler via the raw EFI
    // output protocol.
    let _ = uefi::helpers::init();

    info!("============================================================");
    info!(" AnimaOS microVM — UEFI boot trampoline                    ");
    info!(" Stage 4 / Epic E4.1                                        ");
    info!("============================================================");

    // -----------------------------------------------------------------
    // 3. Exercise S4.1.1: corpus FrameAllocator in a no_std context.
    // -----------------------------------------------------------------
    let frame_alloc = FrameAllocator::new(128);

    let fa = frame_alloc
        .allocate(8)
        .expect("FrameAllocator: 8-frame allocation should succeed");
    info!(
        "FrameAllocator: allocated {} frames starting at frame {}",
        fa.frames, fa.start_frame
    );
    assert_eq!(fa.start_frame, 0);
    assert_eq!(fa.frames, 8);

    let fb = frame_alloc
        .allocate(16)
        .expect("FrameAllocator: 16-frame allocation should succeed");
    info!(
        "FrameAllocator: allocated {} frames starting at frame {}",
        fb.frames, fb.start_frame
    );
    assert_eq!(fb.start_frame, 8, "second allocation must be contiguous");

    info!(
        "FrameAllocator: {} / {} frames used",
        frame_alloc.allocated(),
        frame_alloc.capacity()
    );

    // -----------------------------------------------------------------
    // 4. Exercise S4.1.2: corpus BumpAllocator via the alloc crate.
    //
    //    Vec<u32> exercises the GlobalAlloc path (alloc → BumpAllocator).
    // -----------------------------------------------------------------
    let mut v: Vec<u32> = Vec::new();
    for i in 0u32..32 {
        v.push(i * i);
    }
    info!(
        "BumpAllocator (via Vec<u32>): {} elements, sum = {}",
        v.len(),
        v.iter().map(|&x| x as u64).sum::<u64>()
    );
    assert_eq!(v.len(), 32);
    assert_eq!(
        v.iter().map(|&x| x as u64).sum::<u64>(),
        (0u64..32).map(|i| i * i).sum::<u64>()
    );
    info!(
        "BumpAllocator: {} bytes used so far",
        ALLOCATOR.allocated_bytes()
    );

    // -----------------------------------------------------------------
    // 5. Deliberate panic — satisfies E4.1 exit criterion.
    //
    //    The uefi `panic_handler` feature writes the panic message to the
    //    UEFI console.  With `-serial stdio` in QEMU the output also
    //    appears on the host terminal, where the CI job greps for
    //    "ANIMA_PANIC".
    // -----------------------------------------------------------------
    error!("All corpus substrate checks passed — triggering deliberate panic.");
    panic!("ANIMA_PANIC: boot trampoline reached the panic handler (E4.1 exit criterion met)");
}
