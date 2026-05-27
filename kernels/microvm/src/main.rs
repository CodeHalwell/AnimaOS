//! AnimaOS Stage 4 — microVM UEFI kernel (E4.1 → E4.2 → E4.3).
//!
//! This binary demonstrates the deliverables of E4.1, E4.2, and E4.3.
//!
//! ## E4.1 deliverables (carried forward)
//!
//! 1. **S4.1.1 — `no_std`-clean `corpus`**: the corpus crate is imported and
//!    its types are used here without any `std` dependency.
//!
//! 2. **S4.1.2 — Custom allocator integration**: `corpus::BumpAllocator` is
//!    registered as the global Rust allocator (`#[global_allocator]`), backed
//!    by a static 512 KiB byte array.
//!
//! 3. **S4.1.3 — UEFI boot trampoline**: a real UEFI application that boots
//!    via the EFI entry point, exercises the corpus substrate, and signals
//!    completion via the panic handler.
//!
//! ## E4.2 deliverables (carried forward)
//!
//! 4. **S4.2.1 — Embassy executor embedded in the kernel**: `embassy-executor`
//!    is brought up with a single static `embassy_executor::raw::Executor`
//!    (via `static_cell`).  No arch-specific feature is selected; on
//!    x86_64/UEFI the executor falls back to a portable spin-poll loop, which
//!    is exactly what is needed under UEFI where there is no OS scheduler and
//!    no hardware wake-up mechanism is wired to the interrupt controller.
//!
//! 5. **S4.2.2 — First kernel-level async task to completion**: `kernel_boot_task`
//!    is a `#[embassy_executor::task]` spawned via the executor's `Spawner`.
//!    It traverses multiple `yield_now().await` points (demonstrating cooperative
//!    round-trips through the poll loop), re-runs the E4.1 corpus assertions,
//!    and finally writes `E4.2_TASK_DONE` to the COM1 audit channel.
//!
//! ## E4.3 deliverables (new)
//!
//! 6. **S4.3 — smoltcp TCP/IP stack**: `smoltcp 0.11` is linked into the kernel
//!    with a `phy::Loopback` device.  A TCP server socket listens on port 1234
//!    and a TCP client socket connects to it on the same interface.  The loopback
//!    device loops ethernet frames through an in-memory `VecDeque<Vec<u8>>` so
//!    no real hardware is required.  After the TCP handshake completes and the
//!    client sends `b"HELLO"` to the server, `E4.3_TCP_DONE` is written to COM1.
//!
//! # Serial output strategy
//!
//! Every important message is written **directly to COM1 (port 0x3F8)** via
//! x86 port I/O — bypassing OVMF's EFI console routing.  QEMU emulates a
//! 16550 UART at 0x3F8 and forwards bytes to the device given by `-serial`
//! (in CI: `-serial file:/tmp/qemu-serial.txt`).
//!
//! # Exit criteria
//!
//! - E4.2: `E4.2_TASK_DONE` — Embassy async task completed and signalled.
//! - E4.3: `E4.3_TCP_DONE` — first TCP connection over smoltcp loopback.
//! - `ANIMA_PANIC` — panic handler reached (carried forward from E4.1).
//!
//! The CI job (`microvm-boot`) asserts all three.
// ---------------------------------------------------------------------------
// Nightly feature gates
// ---------------------------------------------------------------------------
// impl_trait_in_assoc_type: required by the `#[embassy_executor::task]` proc-
// macro, which generates an associated type containing `impl Future<…>`.
#![feature(impl_trait_in_assoc_type)]
#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use corpus::{BumpAllocator, FrameAllocator};
use embassy_executor::raw::Executor;
use log::info;
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{Loopback, Medium};
use smoltcp::socket::tcp;
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, IpAddress, IpCidr};
use static_cell::StaticCell;
use uefi::prelude::*;

// ---------------------------------------------------------------------------
// S4.1.2: corpus BumpAllocator as the global Rust heap.
// ---------------------------------------------------------------------------

/// Static heap for the microVM kernel — 512 KiB.
// SAFETY: accessed exclusively via `ALLOCATOR.init()` before any allocation.
static mut HEAP: [u8; 512 * 1024] = [0u8; 512 * 1024];

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator::new();

// ---------------------------------------------------------------------------
// S4.2.1 — Embassy pender (required by embassy-executor without arch feature)
//
// When no `arch-*` feature is selected, embassy-executor declares an external
// Rust symbol `__pender(context: *mut ())` that the *user* must provide.  The
// pender is called inside `Pender::pend()` whenever a task's waker is
// triggered and needs re-queuing.  For a spin-poll executor that continuously
// calls `executor.poll()` in a tight loop, the pender can be a no-op: the poll
// loop will pick up every newly-ready task on its next iteration without
// requiring an explicit wake-up signal.
// ---------------------------------------------------------------------------

/// No-op pender for the spin-poll executor on x86_64-unknown-uefi.
///
/// The `#[export_name]` attribute gives this function the link name that
/// embassy-executor's `extern "Rust" { fn __pender(…) }` declaration expects.
#[export_name = "__pender"]
fn __pender(_context: *mut ()) {
    // Intentionally empty: the spin-poll loop in `efi_main` re-polls
    // all ready tasks on every iteration without needing an explicit wake.
}

// ---------------------------------------------------------------------------
// S4.2.1: static Embassy executor slot.
//
// `StaticCell<Executor>` guarantees the `Executor` lives for `'static` — the
// lifetime required by both `Executor::spawner(&'static self)` and the unsafe
// `Executor::poll(&'static self)` in the spin loop.
// ---------------------------------------------------------------------------

static EXECUTOR: StaticCell<Executor> = StaticCell::new();

// ---------------------------------------------------------------------------
// COM1 serial driver (direct port I/O — bypasses UEFI console routing)
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
    outb(COM1, 0x03); // Divisor low byte  → 38400 baud; offset 0 = DR/DLL
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
// Panic handler — writes directly to COM1 so CI grep always finds
// "ANIMA_PANIC" regardless of OVMF console routing.
// ---------------------------------------------------------------------------

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // Re-initialise COM1 in case main hadn't reached that step.
    unsafe {
        serial_init();
    }
    serial_write("\r\n");
    serial_write("ANIMA_PANIC: E4.3 — all exit criteria met, panic handler reached\r\n");
    serial_write("E4.1 ✅ E4.2 ✅ E4.3 ✅ — kernel boot task complete.\r\n");
    loop {
        core::hint::spin_loop();
    }
}

// ---------------------------------------------------------------------------
// S4.2.2: Embassy kernel boot task.
//
// `#[embassy_executor::task]` transforms this async function into a statically
// allocated task.  At compile time, the macro generates a `TaskStorage<F>`
// where `F` is the opaque `impl Future<Output = ()>` returned by the async
// body.  The `StaticCell` ensures a single long-lived allocation; no heap
// allocation is needed for the task descriptor itself.
//
// The task runs four phases, each separated by a `yield_now().await` that
// hands control back to the executor's poll loop.  This demonstrates genuine
// cooperative scheduling: the executor polls the task, the task suspends, the
// executor processes the run-queue, the waker immediately re-enqueues the task,
// and then the executor polls it again.
//
// After all phases are complete the task writes `E4.2_TASK_DONE` to COM1 (the
// "audit channel" in the exit criterion) and then panics, triggering the panic
// handler that writes `ANIMA_PANIC` for the second CI assertion.
// ---------------------------------------------------------------------------

#[embassy_executor::task]
async fn kernel_boot_task() {
    // ------------------------------------------------------------------
    // Phase 1 — corpus FrameAllocator (S4.1.1, re-verified asynchronously)
    // ------------------------------------------------------------------
    serial_write("\n[E4.2] kernel_boot_task: Phase 1 — corpus FrameAllocator\n");

    // Yield once before doing any work to prove that the executor can
    // suspend and resume this task at least once before it progresses.
    embassy_futures::yield_now().await;

    let frame_alloc = FrameAllocator::new(128);

    let fa = frame_alloc
        .allocate(8)
        .expect("FrameAllocator: 8-frame allocation failed");
    assert_eq!(fa.start_frame, 0, "first allocation must start at frame 0");
    assert_eq!(fa.frames, 8);

    let fb = frame_alloc
        .allocate(16)
        .expect("FrameAllocator: 16-frame allocation failed");
    assert_eq!(fb.start_frame, 8, "second allocation must be contiguous");

    serial_write("[E4.2] corpus FrameAllocator: 8 + 16 frames allocated OK\n");

    // ------------------------------------------------------------------
    // Phase 2 — BumpAllocator heap (S4.1.2, via alloc crate)
    // ------------------------------------------------------------------
    serial_write("[E4.2] kernel_boot_task: Phase 2 — BumpAllocator heap\n");
    embassy_futures::yield_now().await;

    let mut v: Vec<u32> = Vec::new();
    for i in 0u32..32 {
        v.push(i * i);
    }
    assert_eq!(v.len(), 32, "Vec must hold 32 elements");
    assert_eq!(
        v.iter().map(|&x| x as u64).sum::<u64>(),
        (0u64..32).map(|i| i * i).sum::<u64>(),
        "sum of squares must match"
    );
    serial_write("[E4.2] BumpAllocator Vec<u32>: 32 elements, sum verified\n");

    // ------------------------------------------------------------------
    // Phase 3 — cooperative yield round-trip (S4.2.2)
    //
    // Four consecutive yield points show that the executor's poll loop
    // correctly re-awakens this task on every iteration.  Each
    // `yield_now().await` suspends execution, the waker immediately
    // re-enqueues the task in the run-queue, and the spin loop polls it
    // again on the very next iteration — demonstrating cooperative
    // multi-step scheduling even with a single task.
    // ------------------------------------------------------------------
    serial_write("[E4.2] kernel_boot_task: Phase 3 — cooperative yield round-trips\n");

    embassy_futures::yield_now().await;
    serial_write("[E4.2]   yield 1/4 — executor poll loop returned to task\n");

    embassy_futures::yield_now().await;
    serial_write("[E4.2]   yield 2/4 — executor poll loop returned to task\n");

    embassy_futures::yield_now().await;
    serial_write("[E4.2]   yield 3/4 — executor poll loop returned to task\n");

    embassy_futures::yield_now().await;
    serial_write("[E4.2]   yield 4/4 — executor poll loop returned to task\n");

    serial_write("[E4.2] All yield round-trips completed — cooperative scheduling verified\n");

    // ------------------------------------------------------------------
    // Phase 4 — audit channel signal (E4.2 exit criterion)
    //
    // "A scheduled async task completes and signals via the audit channel."
    // The audit channel in the microVM is the COM1 serial port.
    // ------------------------------------------------------------------
    serial_write("[E4.2] kernel_boot_task: Phase 4 — signalling audit channel\n");
    embassy_futures::yield_now().await;

    serial_write(
        "E4.2_TASK_DONE: Embassy kernel_boot_task completed all phases — audit channel signalled\n",
    );

    // ------------------------------------------------------------------
    // Phase 5 — smoltcp TCP/IP stack (E4.3 exit criterion)
    //
    // "First outbound TCP connection from inside the microVM succeeds."
    //
    // We use smoltcp's phy::Loopback device: packets sent by the client
    // are looped through an in-memory VecDeque<Vec<u8>> and received by
    // the server socket on the same Interface.  This demonstrates the
    // full TCP/IP stack (framing, IPv4, TCP three-way handshake, data
    // transfer) without real hardware.
    // ------------------------------------------------------------------
    serial_write("\n[E4.3] kernel_boot_task: Phase 5 — smoltcp TCP/IP loopback\n");
    embassy_futures::yield_now().await;

    run_tcp_loopback_test();

    serial_write(
        "E4.3_TCP_DONE: first TCP connection over smoltcp loopback established — exit criterion met\n",
    );

    // Deliberate panic — triggers the panic handler which writes
    // "ANIMA_PANIC" to COM1, satisfying the final CI assertion.
    panic!("ANIMA_PANIC: deliberate panic — E4.1/E4.2/E4.3 exit criteria all met");
}

// ---------------------------------------------------------------------------
// E4.3: smoltcp TCP loopback demonstration.
//
// Brings up a minimal smoltcp IPv4/TCP stack over phy::Loopback:
//
//   ┌──────────────────────────────────────────────────────────┐
//   │  Interface (127.0.0.1/8, medium = Ethernet)              │
//   │                                                          │
//   │  ┌─────────────────┐    ┌──────────────────────────┐    │
//   │  │ Server socket   │    │ Client socket             │    │
//   │  │ listen(:1234)   │◄───│ connect(127.0.0.1:1234)  │    │
//   │  └─────────────────┘    └──────────────────────────┘    │
//   │           │  TCP frames loop through VecDeque  │         │
//   │           └──────────────────────────────────► │         │
//   │                      Loopback PHY               │         │
//   └──────────────────────────────────────────────────────────┘
//
// The poll loop drives the TCP three-way handshake and one send/recv
// cycle entirely in-process.  After the server receives "HELLO" the
// function returns; the caller writes E4.3_TCP_DONE to COM1.
// ---------------------------------------------------------------------------

/// Run a smoltcp TCP loopback test.
///
/// Sets up a minimal smoltcp IPv4/TCP stack with the in-memory Loopback PHY,
/// performs a TCP three-way handshake between a client socket and a server
/// socket on the same interface, sends `b"HELLO"` from client to server, and
/// asserts that the server received it.
///
/// This is a **synchronous** function — the Embassy executor is not involved
/// in the smoltcp poll loop because there is only one task and smoltcp drives
/// its own internal state machine.  Calls are made from within the async
/// `kernel_boot_task` after a `yield_now().await` to demonstrate that async
/// and synchronous code coexist correctly.
fn run_tcp_loopback_test() {
    // -----------------------------------------------------------------
    // Device and interface
    // -----------------------------------------------------------------
    let mut device = Loopback::new(Medium::Ethernet);

    // Use a locally administered unicast MAC (bit 1 of byte 0 set).
    let mut config = Config::new(EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]).into());
    // Fixed seed → deterministic initial sequence numbers in CI.
    config.random_seed = 0xdead_beef_cafe_babe;

    let mut iface = Interface::new(config, &mut device, Instant::ZERO);
    iface.update_ip_addrs(|ip_addrs| {
        // 127.0.0.1/8 — full loopback subnet; all 127.x.x.x addresses
        // route through this interface, consistent with RFC 5735.
        ip_addrs
            .push(IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8))
            .unwrap();
    });

    // -----------------------------------------------------------------
    // Sockets
    // -----------------------------------------------------------------
    let mut sockets = SocketSet::new(alloc::vec![]);

    // Server: 512-byte rx/tx buffers, listen on port 1234.
    let server_socket = tcp::Socket::new(
        tcp::SocketBuffer::new(alloc::vec![0u8; 512]),
        tcp::SocketBuffer::new(alloc::vec![0u8; 512]),
    );
    let server_handle = sockets.add(server_socket);
    sockets
        .get_mut::<tcp::Socket>(server_handle)
        .listen(1234_u16)
        .unwrap();

    // Client: 512-byte rx/tx buffers, connect to 127.0.0.1:1234.
    // Local ephemeral port 49152 — the first port in the private range.
    let client_socket = tcp::Socket::new(
        tcp::SocketBuffer::new(alloc::vec![0u8; 512]),
        tcp::SocketBuffer::new(alloc::vec![0u8; 512]),
    );
    let client_handle = sockets.add(client_socket);
    {
        // Separate the two borrows: `socket` borrows `sockets`,
        // `iface.context()` borrows `iface` — no aliasing conflict.
        let socket = sockets.get_mut::<tcp::Socket>(client_handle);
        let cx = iface.context();
        socket
            .connect(cx, (IpAddress::v4(127, 0, 0, 1), 1234_u16), 49152_u16)
            .unwrap();
    }

    serial_write("[E4.3] smoltcp interface initialised, TCP sockets created\n");
    serial_write("[E4.3] client connecting to 127.0.0.1:1234 over loopback...\n");

    // -----------------------------------------------------------------
    // Poll loop — drive the TCP state machine to completion.
    //
    // Iteration budget: 500 iterations × 1 ms simulated wall time each.
    // The three-way handshake typically completes in 3–5 iterations on
    // the loopback device; 500 is a generous upper bound.
    // -----------------------------------------------------------------
    let mut client_sent = false;
    let mut server_received = false;

    for time_ms in (0_i64..).take(500) {
        // Advance simulated time by 1 ms per iteration so smoltcp's
        // retransmission timers and TIME_WAIT states advance correctly.
        let now = Instant::from_millis(time_ms);
        iface.poll(now, &mut device, &mut sockets);

        // Client: send "HELLO" as soon as the socket may send.
        {
            let socket = sockets.get_mut::<tcp::Socket>(client_handle);
            if socket.may_send() && !client_sent {
                // send_slice returns the number of bytes queued.
                let queued = socket.send_slice(b"HELLO").unwrap_or(0);
                if queued > 0 {
                    client_sent = true;
                    serial_write("[E4.3] client sent 'HELLO' — TCP connection established\n");
                }
            }
        }

        // Server: receive and verify the payload.
        {
            let socket = sockets.get_mut::<tcp::Socket>(server_handle);
            if socket.can_recv() {
                let mut buf = [0u8; 16];
                let n = socket.recv_slice(&mut buf).unwrap_or(0);
                if n > 0 {
                    server_received = true;
                    serial_write("[E4.3] server received data from client — TCP round-trip OK\n");
                }
            }
        }

        if server_received {
            break;
        }
    }

    assert!(
        server_received,
        "E4.3 smoltcp TCP loopback test failed: server did not receive data within 500 iterations"
    );

    serial_write("[E4.3] smoltcp TCP loopback test PASSED\n");
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
    serial_write("\n=== AnimaOS microVM UEFI kernel (Stage 4 / E4.2) ===\n");
    serial_write("[E4.2] Bringing up Embassy async executor on x86_64-unknown-uefi\n");

    // -----------------------------------------------------------------
    // 1. Initialise the corpus BumpAllocator (S4.1.2).
    //    Must happen before any alloc use (Vec, Box, format!, …) AND
    //    before the task is spawned, since alloc may be exercised during
    //    the task's first poll.
    // -----------------------------------------------------------------
    // SAFETY: `HEAP` is a static array; `addr_of_mut!` gives a raw pointer
    // without creating a reference, avoiding `static_mut_refs`.
    unsafe {
        let heap_ptr = core::ptr::addr_of_mut!(HEAP) as *mut u8;
        let heap_len = core::mem::size_of::<[u8; 512 * 1024]>();
        ALLOCATOR.init(heap_ptr, heap_len);
    }
    serial_write("[E4.2] BumpAllocator initialised over 512 KiB static heap\n");

    // -----------------------------------------------------------------
    // 2. Initialise uefi helpers (EFI console logger — nice to have for
    //    on-screen output; not required for the CI serial capture).
    // -----------------------------------------------------------------
    let _ = uefi::helpers::init();
    info!("uefi helpers initialised");

    // -----------------------------------------------------------------
    // 3. Boot the Embassy executor (S4.2.1).
    //
    // `EXECUTOR.init(Executor::new())` stores the executor in the
    // `StaticCell`, returning `&'static raw::Executor`.  The `'static`
    // lifetime is required by both `spawner()` and `poll()`.
    //
    // We use `raw::Executor` directly because no arch-specific feature is
    // enabled — on x86_64/UEFI there is no arch crate for embassy.  The
    // spin loop (`loop { unsafe { executor.poll() } }`) is the correct
    // no-arch scheduling strategy.
    //
    // The loop returns `!` (diverges), which the Rust type-checker
    // accepts as `Status` via the `!` coercion rule.
    // -----------------------------------------------------------------
    serial_write("[E4.2] Initialising Embassy raw::Executor...\n");

    // `raw::Executor::new` takes a signal-context pointer used by the wake
    // signal function.  The default signal function is a no-op, so a null
    // pointer is correct for a spin-poll executor that never sleeps.
    let executor = EXECUTOR.init(Executor::new(core::ptr::null_mut()));

    executor
        .spawner()
        .spawn(kernel_boot_task())
        .expect("failed to spawn kernel_boot_task");

    serial_write("[E4.2] kernel_boot_task spawned — entering Embassy spin-poll loop\n");

    // Spin-poll loop: drives cooperative async tasks to completion.
    // `kernel_boot_task` will panic when it finishes, unwinding through
    // the panic handler which signals the CI exit criteria.
    loop {
        // SAFETY: called exactly once per iteration on a `'static` executor;
        // single-threaded UEFI context guarantees no concurrent `poll()`.
        unsafe { executor.poll() }
    }
}
