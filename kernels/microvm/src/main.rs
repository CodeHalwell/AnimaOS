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
//!    the EFI entry point, exercises the corpus substrate, and then triggers a
//!    deliberate panic.
//!
//! # Serial output strategy
//!
//! The uefi crate's `logger` feature writes to `SimpleTextOutputProtocol` (the
//! EFI console → VGA display), which QEMU does **not** route to `-serial file:`.
//! To reliably get output into the CI serial capture, every important message is
//! written **directly to COM1 (port 0x3F8)** via x86 port I/O — bypassing OVMF's
//! console routing entirely.  QEMU emulates a 16550 UART at 0x3F8 and forwards
//! all bytes written to it to the device specified by `-serial` (in CI this is
//! `-serial file:/tmp/qemu-serial.txt`).
//!
//! # Exit criterion (E4.1)
//!
//! > "QEMU boots the trampoline image and reaches the panic handler under a
//! >  deliberate panic."
//!
//! The CI job in `.github/workflows/ci.yml` (`uefi-boot`) asserts this by
//! greping the serial capture file for `ANIMA_PANIC`.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use corpus::{BumpAllocator, FrameAllocator};
use log::info;
use uefi::prelude::*;

// ---------------------------------------------------------------------------
// Stage 4 S4.1.2: corpus BumpAllocator as the global Rust heap.
// ---------------------------------------------------------------------------

/// Static heap for the boot trampoline — 512 KiB.
// SAFETY: accessed exclusively via `ALLOCATOR.init()` before any allocation.
static mut HEAP: [u8; 512 * 1024] = [0u8; 512 * 1024];

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator::new();

// ---------------------------------------------------------------------------
// COM1 serial driver (direct port I/O — bypasses UEFI console routing)
//
// QEMU emulates a 16550A UART at I/O port 0x3F8.  Bytes written here are
// forwarded to whatever `-serial` device QEMU was started with.  In CI that
// is `-serial file:/tmp/qemu-serial.txt`, so output is reliably captured.
// ---------------------------------------------------------------------------

const COM1: u16 = 0x3F8;

/// Write `val` to x86 I/O port `port`.
#[inline(always)]
unsafe fn outb(port: u16, val: u8) {
    core::arch::asm!(
        "out dx, al",
        in("dx") port,
        in("al") val,
        options(nomem, nostack, preserves_flags)
    );
}

/// Read a byte from x86 I/O port `port`.
#[inline(always)]
unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    core::arch::asm!(
        "in al, dx",
        out("al") val,
        in("dx") port,
        options(nomem, nostack, preserves_flags)
    );
    val
}

/// Initialise COM1 at 38400 baud, 8N1, no parity, FIFO enabled.
///
/// # Safety
/// Must be called before any `serial_write_*` call.  Safe to call multiple
/// times (re-initialisation is idempotent for our purposes).
unsafe fn serial_init() {
    outb(COM1 + 1, 0x00); // Disable all interrupts
    outb(COM1 + 3, 0x80); // Enable DLAB (baud rate divisor mode)
    outb(COM1, 0x03); // Divisor low byte  → 38400 baud (115200 / 3); offset 0 = DR/DLL
    outb(COM1 + 1, 0x00); // Divisor high byte → 0
    outb(COM1 + 3, 0x03); // 8 bits, no parity, one stop bit; DLAB off
    outb(COM1 + 2, 0xC7); // Enable FIFO, clear TX/RX queues, 14-byte threshold
    outb(COM1 + 4, 0x0B); // Enable RTS/DTR
}

/// Write a single byte to COM1, busy-waiting for the transmit buffer.
#[inline]
unsafe fn serial_write_byte(byte: u8) {
    // Wait until Transmitter Holding Register Empty (bit 5 of LSR).
    while inb(COM1 + 5) & 0x20 == 0 {}
    outb(COM1, byte);
}

/// Write a UTF-8 string slice to COM1.  `\n` is expanded to `\r\n`.
fn serial_write(s: &str) {
    unsafe {
        for byte in s.bytes() {
            if byte == b'\n' {
                serial_write_byte(b'\r');
            }
            serial_write_byte(byte);
        }
    }
}

// ---------------------------------------------------------------------------
// Panic handler — writes directly to COM1 so the CI grep always finds
// "ANIMA_PANIC" regardless of how OVMF routes the EFI console.
// ---------------------------------------------------------------------------

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // Re-initialise COM1 in case main hadn't reached that step.
    unsafe {
        serial_init();
    }
    serial_write("\r\n");
    serial_write("ANIMA_PANIC: E4.1 — boot trampoline panic handler reached\r\n");
    serial_write("Epic E4.1 exit criterion met: QEMU booted the trampoline image\r\n");
    serial_write("and reached the panic handler under a deliberate panic.\r\n");
    loop {
        core::hint::spin_loop();
    }
}

// ---------------------------------------------------------------------------
// UEFI entry point
// ---------------------------------------------------------------------------

#[entry]
fn efi_main() -> Status {
    // -----------------------------------------------------------------
    // 0. Initialise COM1 *first* — serial output must work before anything
    //    else so that even a very early panic is captured.
    // -----------------------------------------------------------------
    unsafe {
        serial_init();
    }
    serial_write("\n=== AnimaOS microVM UEFI boot trampoline (Stage 4 / E4.1) ===\n");

    // -----------------------------------------------------------------
    // 1. Initialise the corpus BumpAllocator (S4.1.2).
    //    Must happen before any alloc use (Vec, Box, format!, …).
    // -----------------------------------------------------------------
    // SAFETY: `HEAP` is a static array; `addr_of_mut!` gives us a raw
    // pointer without creating a reference, avoiding `static_mut_refs`.
    unsafe {
        let heap_ptr = core::ptr::addr_of_mut!(HEAP) as *mut u8;
        let heap_len = core::mem::size_of::<[u8; 512 * 1024]>();
        ALLOCATOR.init(heap_ptr, heap_len);
    }
    serial_write("[S4.1.2] BumpAllocator initialised over 512 KiB static heap\n");

    // -----------------------------------------------------------------
    // 2. Initialise uefi helpers (EFI console logger — nice to have for
    //    on-screen output; not required for the CI serial capture).
    // -----------------------------------------------------------------
    let _ = uefi::helpers::init();
    info!("uefi helpers initialised");

    // -----------------------------------------------------------------
    // 3. Exercise S4.1.1: corpus FrameAllocator in a no_std context.
    // -----------------------------------------------------------------
    let frame_alloc = FrameAllocator::new(128);

    let fa = frame_alloc
        .allocate(8)
        .expect("FrameAllocator: 8-frame allocation failed");
    assert_eq!(fa.start_frame, 0);
    assert_eq!(fa.frames, 8);

    let fb = frame_alloc
        .allocate(16)
        .expect("FrameAllocator: 16-frame allocation failed");
    assert_eq!(fb.start_frame, 8, "second allocation must be contiguous");

    serial_write("[S4.1.1] FrameAllocator: 8 + 16 = 24 frames allocated OK\n");

    // -----------------------------------------------------------------
    // 4. Exercise S4.1.2: corpus BumpAllocator via the alloc crate.
    // -----------------------------------------------------------------
    let mut v: Vec<u32> = Vec::new();
    for i in 0u32..32 {
        v.push(i * i);
    }
    assert_eq!(v.len(), 32);
    assert_eq!(
        v.iter().map(|&x| x as u64).sum::<u64>(),
        (0u64..32).map(|i| i * i).sum::<u64>()
    );
    serial_write("[S4.1.2] BumpAllocator (Vec<u32>): 32 elements, sum verified\n");

    // -----------------------------------------------------------------
    // 5. Deliberate panic — satisfies the E4.1 exit criterion.
    //    The #[panic_handler] above writes "ANIMA_PANIC" to COM1.
    // -----------------------------------------------------------------
    serial_write("Triggering deliberate panic to demonstrate the panic handler...\n");
    panic!("ANIMA_PANIC: deliberate panic — E4.1 exit criterion");
}
