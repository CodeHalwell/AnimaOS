# AnimaOS

AnimaOS is a bare-metal, cloud-isolated framekernel OS intended to act as the
somatic architecture (physical body, autonomic nervous system, and reflex arcs)
for an autonomous LLM agent process.

## Workspace Layout

```
anima-os/
├── .github/workflows/ci.yml   # fmt + build + test + clippy
├── Cargo.toml
├── crates/
│   ├── kernel-core/           # TCB: frame allocator, PCB, syscall enum
│   ├── lifecycle/             # Autonomous lifecycle director + sleep routines
│   ├── scheduler/             # 3-tier MLFQ, bounded token pipe, LlmBackend
│   ├── memory/                # CLS L1/L2/L3, ARC cache, emotional decay
│   ├── toolbus/               # Routing filter, circuit breaker, MCP/A2A envelopes
│   ├── security/              # Typestate capability tokens
│   ├── observe/               # Interoceptive engine (TTFT, stress index)
│   └── sensory-bridge/        # Text / PCM packetization
└── kernels/
    ├── hosted/                # Linux process emulation binary (`anima-hosted`)
    └── microvm/               # Firecracker / Cloud Hypervisor unikernel (TBD)
```

## Implemented Core Interfaces

### Autonomic Substrate (`kernel-core`)
- `FrameAllocator` (bump-style, atomic, audited)
- `AgentPcb`, `AgentPid`, `AgentState`
- `SyscallEnum`

### Self-Preservation Plane (`lifecycle`)
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

### Efferent Actuator Core (`toolbus`)
- `length_robust_filter` (relative routing)
- `CircuitBreaker` (Closed / Open / HalfOpen, configurable cooldown)
- `ToolDriver` trait + `ToolEnvelope` (MCP / A2A buses)

### Self/Non-Self Barrier (`security`)
- Typestate `Capability<Unverified>` → `Capability<Verified>`

### Interoceptive Feedback (`observe`)
- `HomeostaticMonitor::compute_systemic_stress_index`
- Rolling TTFT window via `record_ttft`

### Afferent Input Vector (`sensory-bridge`)
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
