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
    └── microvm/               # Firecracker / Cloud Hypervisor unikernel (TBD)
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

The non-TCB crates explicitly enforce `#![forbid(unsafe_code)]`.

## Building & Running

```sh
cargo build --workspace
cargo test --workspace
cargo run -p hosted --bin anima-hosted
```

CI runs `cargo fmt --check`, build, test, and `cargo clippy -- -D warnings`.

## Docker (GPU-ready local deployment)

A spike for running `anima-hosted` in a container with NVIDIA GPU
passthrough lives under [`docker/`](./docker/README.md). Quick start:

```sh
docker compose up --build
```

See [`docker/README.md`](./docker/README.md) for prerequisites
(nvidia-container-toolkit), live-backend env vars, and known limitations.
