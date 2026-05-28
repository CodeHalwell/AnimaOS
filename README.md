# AnimaOS

AnimaOS is a bare-metal, cloud-isolated framekernel OS intended to act as the
somatic architecture (physical body, autonomic nervous system, and reflex arcs)
for an autonomous LLM agent process.

See [`docs/`](./docs/README.md) for the full design suite.

## Workspace Layout

```
anima-os/
├── .github/workflows/ci.yml   # fmt + build + test + clippy + microvm-{build,boot}
├── .github/workflows/bench.yml    # criterion benches + E4.7 regression gate
├── .github/workflows/soak.yml     # E4.7 microVM soak harness (manual)
├── .github/workflows/nightly.yml  # E4.6 Kani + Miri
├── Cargo.toml
├── crates/
│   ├── corpus/                # The body (TCB): frame allocator, PCB, syscall enum
│   ├── vita/                  # Autonomous lifecycle director + sleep routines + router/gate
│   ├── scheduler/             # 3-tier MLFQ, bounded token pipe, LlmBackend
│   ├── memory/                # CLS L1/L2/L3, ARC cache, emotional decay, TurboQuant
│   ├── praxis/                # Efferent actuator: routing, circuit breaker, MCP/A2A envelopes
│   ├── self/                  # Self/non-self barrier: typestate capability tokens
│   ├── interoception/         # Stress index, TTFT window
│   └── senses/                # Afferent input: text / PCM packetization
└── kernels/
    ├── hosted/                # Linux process emulation binary (`anima-hosted`) — development only
    └── microvm/               # x86_64-unknown-uefi framekernel — production target
```

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

### Learned KV-Cache Controller (`kv-controller`, Stage 5 / E5.4)
- Semantic gating on top of TurboQuant: `kv_controller::controller`,
  `kv_controller::trace`, `kv_controller::training`
- `MemoryScope::kv_controller` opt-in from `vita::router`

### Defence Layer (`defence`, Stage 5 / E5.6)
- Prompt-injection screen, drift detection, immune-analogue veto
- Vetoed actions logged at elevated severity; repeated vetoes escalate to the gate

The non-TCB crates explicitly enforce `#![forbid(unsafe_code)]`.

## Building & Running

```sh
# Workspace (hosted/dev target) — fmt + clippy + tests on stable Rust.
cargo build --workspace
cargo test --workspace
cargo run -p hosted --bin anima-hosted   # development convenience binary

# Production target — bare-metal UEFI framekernel.
# Requires nightly Rust + the x86_64-unknown-uefi target.
cd kernels/microvm && cargo +nightly build --release

# microVM soak driver (Epic E4.7).
cargo xtask soak --hours 1 --efi kernels/microvm/target/x86_64-unknown-uefi/release/anima-microvm.efi
```

CI runs the following gates:

| Workflow      | What it gates                                              |
|---------------|------------------------------------------------------------|
| `ci.yml`      | fmt + build + test + clippy, supply-chain audit, microVM UEFI build (E4.7.1 image-size budget) and QEMU boot (E4.7.2 boot-time ≤ 2 s) |
| `bench.yml`   | Criterion benches with the E4.7.3 regression gate against `bench/baselines/<crate>.json` |
| `nightly.yml` | Kani bounded model checking (15 proofs) + Miri (E4.6)      |
| `soak.yml`    | E4.7 soak harness smoke test (manual dispatch)             |

## Targets

| Target                     | Status            | Toolchain | Purpose                                                                 |
|----------------------------|-------------------|-----------|-------------------------------------------------------------------------|
| `kernels/microvm`          | **Production**    | nightly + `x86_64-unknown-uefi` | Bare-metal framekernel under Firecracker / Cloud Hypervisor. |
| `kernels/hosted`           | Development only  | stable    | Linux-process emulation for local experimentation and CI workspace tests. |

The hosted target is retained for development convenience; new subsystem
features must build for both targets before they can land on `main`.

## Deployment

Two parallel surfaces share the same workspace code — see
[`docs/10-deployment-pathways.md`](./docs/10-deployment-pathways.md) for the
full rationale.

### Bare-metal microVM (primary)

The UEFI framekernel at `kernels/microvm` boots under Firecracker or Cloud
Hypervisor.  Per Epic E4.7 production hardening:

- Release image budget: **≤ 1 MiB EFI** (enforced in `ci.yml`).
- Boot-to-soak-complete budget: **≤ 2 s** under QEMU/OVMF
  (enforced in `ci.yml`).
- Continuous 30-day soak driven by `cargo xtask soak --hours 720`; the
  resulting manifest is committed under `artifacts/soak/` as a durable
  record of stability.

### Containerised (development)

`anima-hosted` + Ollama (llama.cpp inference) + Unsloth trainer
(profile-gated, for sleep-phase QLoRA), all wired through
docker-compose with NVIDIA GPU passthrough.  Useful for local development
when iterating on cognition without rebuilding the UEFI kernel each time:

```sh
docker compose up --build                       # inference stack
docker compose --profile training up --build    # also build the trainer
```

Operational details — model defaults, env vars, VRAM budget on a 3090,
known limitations — live in [`docker/README.md`](./docker/README.md).
