# 22 — Remaining Hardware-Gated Work (and Bare-Metal Runbook)

> **Status:** Living tracker. Everything in §1 is *software-complete behind a
> fixture/feature gate* and only needs **real hardware or a live external
> dependency** to close — none of it blocks further software work. §2 is the
> bare-metal onboarding runbook (closes E9 S9.6).

The somatic core (E1–E6), the bare-metal microVM (Stage 4 / E4.x), the
autonomous-agent layer (E7–E17), and the operational wave (E18–E30) are all
merged and green in CI. What remains is the set of "live" tails that were
deliberately stubbed with fixtures + an env/feature gate so the workspace stays
hermetic.

## 1. The hardware-gated tails

| Item | Epic | State today | What closes it |
|---|---|---|---|
| **30-day soak run** | E4.7 | 🟡 Harness, manifest schema, and CI smoke-test all shipped (`cargo xtask soak`, `.github/workflows/soak.yml`). The 720-hour run has **not** been executed. | Run `cargo xtask soak --hours 720 --efi <release.efi>` on a Firecracker / Cloud-Hypervisor host and commit the resulting manifest under `artifacts/soak/`. |
| **microVM Phase-1 transport** | E6 S6.5 | ⬜ `console-proto` over `smoltcp` + TLS is designed; blocked on a `virtio-net` driver that does not yet exist in the kernel. The protocol carries over unchanged — only the transport is missing. | Implement a `virtio-net` driver for the Firecracker/Cloud-Hypervisor target, then bind the existing `console-proto` framing onto a `smoltcp` TCP socket (the Phase-0 COM1 path already proves the protocol). |
| **Native FFI runtimes** | E8 S8.3 | ⬜ Abstraction + fixtures shipped (`llm-backends/src/native.rs`: `NativeRuntime`, `LlamaCppNativeBackend`, `LiteRtLmBackend`, `FixtureNativeRuntime`). Real bindings sit behind unwired flags `llama-native-live` / `litert-lm-live`. | Wire the `llama.cpp` and LiteRT-LM FFI behind those feature flags against the installed native libraries; flip the fixture default when a runtime is present. |
| **Real fine-tuning** | E8 S8.4.5/.6 | 🟡 The full loop is now wired end-to-end in software: the hosted agent persists its sleep-phase corpus (`ANIMA_CORPUS_DIR`, default `~/.anima/training_corpus`, shared into the trainer container read-only), and `trainer/sleep_phase.py` consumes it — corpus validation + manifest verified via `--dry-run`; QLoRA (Unsloth) + GGUF export + Ollama Modelfile are the `live` path. `crates/finetune`'s `UnslothFineTuner` remains a `live`-gated skeleton. | Run `sleep_phase.py` (no `--dry-run`) on a CUDA host to validate the training/merge/quant path, then point `UnslothFineTuner::live` at the same flow; the manifest's `adapter_artifact` block already mirrors `finetune::AdapterArtifact` for library ingestion. |

## 1a. The software tail (no hardware required)

One item is *not* hardware-gated and is called out separately so it isn't
lost behind the table above:

| Item | Epic | State today | What closes it |
|---|---|---|---|
| **`vita` in the microVM** | E4.5 follow-on | ⬜ Effectively std-only; the kernel links `corpus` + `scheduler` + `memory` + `interoception` + `console-proto` and runs the E4.5 soak **without the lifecycle director**. | Port `vita` off `std` and extend the boot soak to a full in-kernel wake→sleep cycle. This is the highest-leverage *software* item: it makes the bare-metal target an organism rather than a substrate. The gap has now been **measured** (UEFI-target probe build) — see the checklist below. |

#### `vita` no_std gap map (measured 2026-06-11)

A probe build of the kernel with `vita = { default-features = false }` against
`build-std = [core, alloc]` pinned the work to:

1. **Crate attribute missing** — `vita/src/lib.rs` has feature-gated
   `std`/`alloc` imports but no `#![cfg_attr(not(feature = "std"), no_std)]`;
   without it the crate silently requires `std` regardless of features.
2. **Cargo plumbing** — vita's deps are pulled with default (std) features.
   Each needs `default-features = false` + forwarding through vita's `std`
   feature: `scheduler`, `memory` (`libm` for no_std float math),
   `interoception`, `senses`, `serde`/`serde_json`
   (`default-features = false, features = ["alloc"]`), and the three below.
3. **Small dependency ports** — `kv-controller` (1 `use std::` site; no
   `std` feature yet; `vita::kv_gate` imports it unconditionally),
   `defence` (3 sites), `skills` (2 sites; `std` feature already exists and
   vita's import is already gated).
4. **Mutex gap** — `LifecycleManager.task_cancel`/`motivated_gate` are
   `Arc<Mutex<…>>` with `Mutex` imported only under `std`; needs a no_std
   lock (e.g. a `spin`-backed shim preserving the `.lock().unwrap()` call
   shape) or gating.
5. **Stranded IPC types** — `vita::router` (no_std) imports `ToolSpec` /
   `InvokeMemoryScope` / `InvokeRequest` from the wholly std-gated
   `cortex_bridge`; the plain-data types must move to an always-compiled
   module (alloc-only).
6. **Audit sink** — `vita::audit` has 12 `use std::` sites (JSONL file sink,
   HMAC sidecar); no_std needs the in-memory ring + a serial-writer seam so
   the kernel can frame entries onto COM1 via `console-proto`.
7. **Kernel posture decision (recorded)** — stay on
   `build-std = [core, alloc]`. The alternative (UEFI's Tier-2 partial
   `std`) was probed and rejected: the prebuilt sysroot `std` collides with
   build-std (`E0152` duplicate lang items), would fight the kernel's own
   `#[global_allocator]`/`panic_handler`, and risks the ≤ 1 MiB image
   budget.

### Close-out checklist (for whoever has the hardware)

1. **Soak** — provision one Firecracker or Cloud-Hypervisor VM; build the
   release EFI (§2); launch the soak driver; let it run 30 days; commit the
   manifest. CI already enforces the ≤1 MiB image budget; the ≤2 s boot
   budget is *recorded* informationally in CI (QEMU+OVMF is not
   representative) and is asserted by this soak run on microVM hardware.
2. **virtio-net** — this unblocks E6 S6.5 *and* any future in-microVM network
   inference; it is the highest-leverage hardware item.
3. **Native FFI / GPU training** — independent of the microVM; can be done on
   any developer box with the respective native lib / GPU. Each is a
   feature-flag flip plus the real binding, with fixtures as the reference
   behaviour to match.

## 2. Bare-metal onboarding runbook (E9 S9.6)

Boots the production UEFI framekernel (`kernels/microvm`) under a microVM
monitor. Development/iteration should use the hosted target
(`docs/getting-started.md`); this runbook is for the bare-metal production
target.

### Prerequisites

- Nightly Rust with the `x86_64-unknown-uefi` target:
  `rustup toolchain install nightly && rustup target add x86_64-unknown-uefi --toolchain nightly`
- QEMU + OVMF (for local verification) **or** a Firecracker / Cloud-Hypervisor
  host (for production).

### Build the framekernel

```sh
cd kernels/microvm
cargo +nightly build --release          # -> target/x86_64-unknown-uefi/release/anima-microvm.efi
```

CI gates the release EFI at **≤ 1 MiB** and records QEMU/OVMF
boot-to-soak-complete latency informationally (see
`.github/workflows/ci.yml`); the **≤ 2 s** boot budget applies to
Firecracker / Cloud Hypervisor and is asserted by the soak run below.

### Verify the boot locally (QEMU/OVMF)

The `ci.yml` `microvm-boot` job is the reference: it boots the EFI under QEMU
and greps COM1 serial for the `E4.1_*` … `E4.5_SOAK_DONE` markers (and
`E6.4_CONSOLE_DONE` for the operator-console Phase-0 demo). Reuse that job's
QEMU invocation to reproduce a boot on a workstation before promoting an image.

### Run under a microVM monitor (production)

Boot `anima-microvm.efi` as the guest firmware/kernel under Firecracker or
Cloud Hypervisor. Operator telemetry/guidance flows over COM1 using the
`console-proto` serial framing (`ANIMA_TLM` / `ANIMA_IN`); attach with
`anima-console serial …` (see `docs/11-operator-interface.md`). Networked
operator transport (HTTP/SSE in-guest) waits on the `virtio-net` driver tracked
in §1.

### Continuous soak (production stability record)

```sh
cargo xtask soak --hours 720 --efi kernels/microvm/target/x86_64-unknown-uefi/release/anima-microvm.efi
```

The driver records per-iteration boot latency and outcome, writes a resumable
JSON manifest + JSONL log, and is the artefact that closes E4.7's 30-day
criterion. Commit the manifest under `artifacts/soak/`.

> **Note on accuracy.** This runbook references only commands and CI jobs that
> exist in the repository today. The exact Firecracker/Cloud-Hypervisor device
> config is intentionally left to the operator's environment; validate the
> QEMU/OVMF boot (above) first, since that path is CI-exercised.
