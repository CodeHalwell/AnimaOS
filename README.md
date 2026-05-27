# AnimaOS

AnimaOS is a bare-metal, cloud-isolated framekernel OS intended to act as the
somatic architecture (physical body, autonomic nervous system, and reflex arcs)
for an autonomous LLM agent process.

See [`docs/`](./docs/README.md) for the full design suite.

## Workspace Layout

```
anima-os/
├── .github/workflows/ci.yml   # fmt + build + test + clippy
├── Cargo.toml
├── crates/
│   ├── corpus/                # The body (TCB): frame allocator, PCB, syscall enum
│   ├── vita/                  # Autonomous lifecycle director + sleep routines
│   ├── scheduler/             # 3-tier MLFQ, bounded token pipe, LlmBackend
│   ├── memory/                # CLS L1/L2/L3, ARC cache, emotional decay
│   ├── praxis/                # Efferent actuator: routing, circuit breaker, MCP/A2A envelopes
│   ├── self/                  # Self/non-self barrier: typestate capability tokens
│   ├── interoception/         # Stress index, TTFT window
│   └── senses/                # Afferent input: text / PCM packetization
└── kernels/
    ├── hosted/                # Linux process emulation binary (`anima-hosted`)
    └── microvm/               # UEFI bare-metal kernel — Stage 4 production target
```

**Stage 4 (E4.1 – E4.7)** is closed: the microVM kernel boots under
QEMU+OVMF (and, by extension, Firecracker / Cloud Hypervisor), runs Embassy,
brings up `smoltcp` + TLS 1.3, drives one full Stage-3 sleep cycle from the
no_std + alloc port of every higher crate, and is gated in CI by Kani
proofs on the scheduler, Miri on `corpus`, an EFI image-size budget, a
boot-time budget, and a 60 s soak smoke-run (`crates/vita/src/bin/vita_soak.rs`;
the same binary backs the 30-day production soak via `--duration 2592000`).

> The `self` directory contains the package `anima-self` (Rust import: `anima_self`).
> `self` is a reserved Rust keyword and cannot be used directly as a crate name.

## Implemented Core Interfaces

### Autonomic Substrate (`corpus`)
- `FrameAllocator` (bump-style, atomic, audited)
- `AgentPcb`, `AgentPid`, `AgentState`
- `SyscallEnum`

### Self-Preservation Plane (`vita`)
- `somatic_execution_loop` (waking / sleep transitions)
- Sleep maintenance routines: pruning, replay validation, dream exploration,
  policy compilation

### Reflex Loop Control (`scheduler`)
- 3-tier `TaskAgenda` (`MlfqTier::High/Medium/Low`)
- `IterationAwareMlfq::dispatch_task`
- `BoundedTokenPipe` with credit-based backpressure
- `LlmBackend` trait + `CancellationToken`

### Synaptic Memory (`memory`)
- `VirtualContextManager` (L1)
- `ArcCache` (L2 warm cache)
- `ArchivalStore` (L3 vector-similarity stub)
- Emotionally-modulated decay `S(t)` (`MemoryNode::activation_at`)

### Efferent Actuator Core (`praxis`)
- `length_robust_filter` (relative routing)
- `CircuitBreaker` (Closed / Open / HalfOpen, configurable cooldown)
- `ToolDriver` trait + `ToolEnvelope` (MCP / A2A buses)

### Self/Non-Self Barrier (`self` / `anima-self`)
- Typestate `Capability<Unverified>` → `Capability<Verified>`

### Interoceptive Feedback (`interoception`)
- `HomeostaticMonitor::compute_systemic_stress_index`
- Rolling TTFT window via `record_ttft`

### Afferent Input Vector (`senses`)
- `HumanGuidance` policy bounds
- Text / PCM packetization through `SensoryPacket`

The non-TCB crates explicitly enforce `#![forbid(unsafe_code)]`.

## Building & Running

### Hosted target (development)

```sh
cargo build --workspace
cargo test --workspace
cargo run -p hosted --bin anima-hosted
```

### microVM target (Stage 4 production)

```sh
# Build the UEFI image (requires nightly + rust-src).
cd kernels/microvm
cargo +nightly build --release

# Boot under QEMU+OVMF. The CI job microvm-boot does the same and greps
# for E4.2_TASK_DONE, E4.3_TCP_DONE, E4.4_TLS_DONE, E4.5_SLEEP_DONE,
# and ANIMA_PANIC on the COM1 serial log.
mkdir -p esp/EFI/BOOT
cp target/x86_64-unknown-uefi/release/anima-microvm.efi esp/EFI/BOOT/BOOTX64.EFI
qemu-system-x86_64 \
    -drive if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE_4M.fd \
    -drive format=raw,file=fat:rw:esp \
    -serial stdio -display none -m 512M -no-reboot
```

### Soak harness (E4.7)

```sh
# 60-second smoke (matches the CI vita-soak job).
cargo run --release --bin vita-soak --package vita -- --duration 60 --cycle-target-ms 100

# 30-day production soak (operated out of band).
cargo run --release --bin vita-soak --package vita -- --duration 2592000
```

CI runs `cargo fmt --check`, build, test, `cargo clippy -- -D warnings`,
`miri test -p corpus`, the Kani harnesses (`kani-scheduler`), the microVM
boot test (`microvm-boot`), the image-size + boot-time gates
(`microvm-metrics`), and the 60-second soak smoke (`vita-soak`).
