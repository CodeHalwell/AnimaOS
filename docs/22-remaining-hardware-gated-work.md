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
| **Real fine-tuning** | E8 S8.4.5/.6 | ⬜ Pipeline, eval harness, and adapter library shipped (`crates/finetune`, `cargo xtask finetune`). `UnslothFineTuner` is a `live`-gated skeleton returning `BackendUnavailable`. | Implement the Unsloth/HRA GPU training + merge/quant path behind the `live` gate on a CUDA host; the JSONL loader, manifest, and adapter-mount plumbing are already in place. |

## 1a. The software tail (no hardware required)

One item is *not* hardware-gated and is called out separately so it isn't
lost behind the table above:

| Item | Epic | State today | What closes it |
|---|---|---|---|
| **`vita` in the microVM** | E4.5 follow-on | ⬜ `vita` is `no_std`-attributed but effectively std-only; the kernel links `corpus` + `scheduler` + `memory` + `interoception` + `console-proto` and runs the E4.5 soak **without the lifecycle director**. `praxis`, `anima-self`, and `senses` build for `no_std` but are not yet linked. | Port `vita`'s somatic execution loop off `std` (timers, channels, audit sink behind traits), link it plus the remaining `no_std` crates into `kernels/microvm`, and extend the boot soak to drive a full wake→sleep cycle in-kernel. This is the highest-leverage *software* item: it is what makes the bare-metal target an organism rather than a substrate. |

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
