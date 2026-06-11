# `corpus` — Unsafe Code Audit

This file is the audit record required by `docs/04-verification.md` §2.1:
every `unsafe` site inside the Trusted Computing Base is listed here with the
invariant it relies on. A PR that adds, removes, or changes an `unsafe` block
in `corpus` (or the microVM kernel binary, which is part of the same TCB)
must update this file in the same change.

Every non-TCB crate in the workspace declares `#![forbid(unsafe_code)]`.
Two mechanisms keep the quarantine exhaustive: the compiler rejects `unsafe`
inside any crate that declares the attribute, and the `unsafe-quarantine` CI
job (`ci.yml`) asserts the attribute is present at the root of every non-TCB
library crate — so workspace `unsafe` is confined to the TCB files listed
below. What no tool can check is that *this markdown file* documents every
unsafe site within the TCB: that remains a review/process requirement
(§4) enforced at PR time.

## Audit summary

| Module | Unsafe sites | Verified by |
|---|---|---|
| `crates/corpus/src/heap_allocator.rs` | 4 (1 `unsafe fn`, 1 `unsafe impl`, 2 trait methods) + test-local callers | unit tests, Miri (nightly CI), Kani harnesses in `frame_allocator.rs` cover the bounds math shared by both allocators |
| `crates/corpus/src/frame_allocator.rs` | 0 — safe API; all bookkeeping is `AtomicUsize` | 4 Kani proofs, unit tests |
| `crates/corpus/src/{lib,pcb,syscall}.rs` | 0 | — |
| `kernels/microvm/src/main.rs` (TCB binary) | port I/O + boot-time init (see §3) | QEMU boot job greps COM1 markers in CI |

## 1. `heap_allocator.rs` — `BumpAllocator`

The boot-time bump allocator for the bare-metal microVM target. It is the
`#[global_allocator]` in the UEFI kernel, so it cannot be expressed in safe
Rust: `GlobalAlloc` is an `unsafe trait` by definition.

### 1.1 `pub unsafe fn init(&self, heap_start: *mut u8, heap_size: usize)` (line ~136)

- **Why unsafe:** takes a raw pointer to a caller-owned memory region and
  publishes it as the heap.
- **Invariant upheld by the caller:** the region
  `[heap_start, heap_start + heap_size)` is valid for reads and writes for the
  allocator's entire lifetime, is exclusively owned by the allocator, and
  `init` completes (Release stores visible) before any thread calls `alloc`.
  Concurrent `init` calls are not permitted.
- **Mitigations:** `init` itself performs no memory access — it only stores
  the bounds into `AtomicUsize` fields with `Release` ordering, paired with
  `Acquire` loads in `alloc`. `saturating_add` prevents end-address overflow.
- **Call sites:** `kernels/microvm/src/main.rs` boot path (single-threaded
  UEFI entry, called exactly once over a `static` heap array) and
  stack-owned test arrays in `#[cfg(test)]`.

### 1.2 `unsafe impl GlobalAlloc for BumpAllocator` (line ~147)

- **Why unsafe:** `GlobalAlloc` is an unsafe trait; the implementor promises
  that returned pointers are valid, aligned, and non-overlapping.
- **Invariant upheld here:** allocation is a single atomic
  `fetch_update(AcqRel)` on the cursor. All intermediate arithmetic
  (`checked_sub`/`checked_add`) treats overflow as out-of-memory and returns
  null instead of wrapping, so an adversarially large `Layout` cannot escape
  the heap bounds. Distinct successful calls return disjoint ranges because
  each CAS winner advances the cursor past its own allocation.

### 1.3 `unsafe fn alloc(&self, layout: Layout) -> *mut u8` (line ~154)

- **Why unsafe:** required signature of the trait method.
- **Invariant upheld here:** returns null when uninitialised (`heap_start ==
  0`) or exhausted; never fabricates an address outside
  `[heap_start, heap_end)`. Alignment is produced by `align_up`, whose
  wrap-around risk is caught by the subsequent `checked_sub`.

### 1.4 `unsafe fn dealloc(&self, ...)` (line ~208)

- **Why unsafe:** required signature of the trait method.
- **Invariant upheld here:** intentional no-op — a bump allocator never
  reclaims. Sound because forgetting memory is safe; the heap region outlives
  all allocations by construction (static array in the kernel).

### 1.5 Test-local `unsafe` (`#[cfg(test)]`, lines ~226–360)

Calls to `alloc`/`dealloc`/`init` over stack-owned arrays inside the test
module, plus `ptr::offset_from` on pointers proven to come from the same
allocation. These exercise the contracts above and run under Miri in nightly
CI (`nightly.yml`).

## 2. `frame_allocator.rs` — `FrameAllocator`

Contains **no unsafe code**. The allocator hands out frame *indices*, not
pointers; all bookkeeping is `AtomicUsize`. Its bounds and non-overlap
invariants are proven by the four Kani harnesses at the bottom of the file
(`frame_allocator_never_exceeds_bounds`, sequential non-overlap, etc.) and it
is exercised by Miri in nightly CI.

## 3. `kernels/microvm/src/main.rs` — TCB binary unsafe

The UEFI kernel binary sits inside the same trust boundary as `corpus` (it
links the allocator and owns the hardware). Its `unsafe` falls into three
groups:

| Site | Purpose | Invariant |
|---|---|---|
| `outb` / `inb` / `serial_init` / `serial_write_byte` (lines ~161–208) | x86 port I/O to the COM1 UART (0x3F8) via inline asm | Port constants are fixed; QEMU/Firecracker emulate a 16550A at that address; single-threaded boot path serialises access |
| heap/executor init blocks (lines ~226, ~578, ~592) | one-time `BumpAllocator::init` over a `static` heap and Embassy executor setup | executed once, before any allocation, on the single boot CPU |
| `executor.poll()` (line ~644) | raw Embassy executor poll loop | the no-arch executor requires the caller to guarantee single-threaded polling, satisfied by the spin loop |
| `tls.rs` (2 sites) | entropy via `RDRAND` (CPUID-guarded) | CPUID check precedes use; absence is a hard error before any TLS bytes are produced |
| `net.rs` `unsafe impl Hal for KernelHal` (4 trait methods) | DMA + MMIO contract for `virtio-drivers` | UEFI boot services identity-map RAM, so virtual = physical for every contract: `dma_alloc` hands out zeroed, page-aligned, never-reused bump-allocator pages; `mmio_phys_to_virt`/`share`/`unshare` are the identity with no bounce buffers |
| `net.rs` `MmioCam::new` (ECAM candidate scan) | PCI configuration access | each candidate window is identity-mapped device space; a wrong candidate reads `0x0000`/`0xFFFF` vendor ids and is rejected by the host-bridge validity check — reads never touch unmapped memory |

These are exercised end-to-end by the `microvm-boot` CI job, which boots the
image under QEMU/OVMF and asserts the `E4.1_*`…`E4.5_SOAK_DONE` serial
markers.

## 4. Review discipline

1. Every new `unsafe` block needs a `// SAFETY:` comment at the site, an
   entry here, and second-reviewer sign-off at PR time (docs/04 §2.1).
2. If an `unsafe` site is removed, delete its entry — this file must not
   drift from the code. CI's quarantine check (`forbid(unsafe_code)` in all
   non-TCB crates) bounds the audit surface to the files listed above.
