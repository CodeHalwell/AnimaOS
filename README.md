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
- `FrameAllocator` (bump-style, atomic, audited; `BumpAllocator` as `#[global_allocator]` in microVM)
- `AgentPcb`, `AgentPid`, `AgentState`
- `SyscallEnum`
- Kani proof harnesses: frame allocator bounds, sequential non-overlap (15 proofs total across `corpus` + `scheduler`)

### Self-Preservation Plane (`vita`)
- `somatic_execution_loop` — waking / sleep transitions driven by agenda state
- Striatal Gate (`ThresholdGate`, `GateConfig`) — cost-class arbitration with per-decision audit entries
- Thalamic Router (`StaticRouter`) — static route table (cheap-local / mid-tier / frontier) with interoceptive modulation
- Sleep maintenance routines: Pruning → Replay → Dreaming → Compilation (four-phase sequencer)
- KV-gate integration: `gate_working_context` + `gate_working_context_with_signals`
- Episodic store (`EpisodeStore`), identity memory (`IdentityMemory`), cortex bridge (`PythonCortexBridge`)
- `AuditLog` with 26+ structured `AuditEntry` variants

### Reflex Loop Control (`scheduler`)
- 3-tier `TaskAgenda` (`MlfqTier::High/Medium/Low`) with starvation-prevention boost
- `IterationAwareMlfq::dispatch_task` with per-task token-slice accounting
- `BoundedTokenPipe` with credit-based backpressure
- `LlmBackend` trait + `CancellationToken`; Anthropic, OpenAI, and deterministic mock providers
- Kani proofs: `credits_never_exceed_capacity`, `push_refund_roundtrip`, `boost_all_to_high` invariants

### Synaptic Memory (`memory`)
- `VirtualContextManager` — L1 block-structured context with PagedAttention semantics
- `ArcCache` — L2 warm cache with ARC eviction (T1/T2/B1/B2 adaptive replacement)
- `L3Archive` — L3 archival store: cosine-similarity search, demotion, provenance, process-restart persistence
- `MemoryNode::activation_at` — emotionally-modulated exponential decay `S(t)`
- `TurboQuant` — PolarQuant rotation + Lloyd-Max codebook (4/2/1.5/1-bit), QJL bias correction, SIMD scoring kernels
- Memory-pressure event emission via `BoundedTokenPipe`
- Sleep-cycle routines: `L1PruningStore`, `prune_l2_cache`, `run_replay_validation`, `run_dream_walk`, `compile_traces_to_pairs`

### Efferent Actuator Core (`praxis`)
- `length_robust_filter` (relative routing, τ_rel = 0.85 default)
- `CircuitBreaker` (Closed / Open / HalfOpen, configurable cooldown)
- `ToolDriver` trait + `ToolEnvelope` (MCP / A2A buses); built-in: `ClockTool`, `EchoTool`, `TextIoTool`
- `WasmSandbox` — Wasmtime runtime, shared `Arc<Engine>`, fuel-metered, capability-gated WASI imports, `SandboxedMathEvaluator`

### Self/Non-Self Barrier (`self` / `anima-self`)
- Typestate `Capability<Unverified>` → `Capability<Verified>`

### Interoceptive Feedback (`interoception`)
- `HomeostaticMonitor::compute_systemic_stress_index` (1 Hz telemetry stream)
- Rolling TTFT window via `record_ttft`
- `InteroceptiveSensorBundle` — six scalar signals: thermal_load, compute_pressure, memory_pressure, power_budget, financial_budget, attention_demand
- `FinancialBudgetSensor` — per-provider API spend ledger with daily/monthly limits
- `PowerSensor` — Linux sysfs battery/AC reader (opt-in)
- `AttentionSensor` — idle-time decay (opt-in)

### Afferent Input Vector (`senses`)
- `HumanGuidance` policy bounds (max length, blocked prefixes, runtime-updateable)
- Text and PCM packetization via `SensoryPacket` + `PrioritizedPacket`

### Learned KV-Cache Controller (`kv-controller`, Stage 5 / E5.4)
- `LinearGate` implementing `BlockGate` trait — 7-element feature vector, logistic gate
- `TraceCapture` / `InvocationTrace` — per-invocation trace with `TraceProvenance` (live/synthetic/public)
- Offline training pipeline: `compile_training_pairs`, `TrainingCorpus`
- Evaluation harness: `NeedleBenchmarkConfig`, needle recall vs LRU baseline
- Fault state machine: `ControllerState::Faulted` → LRU fallback, `AuditEntry::KvControllerFaulted`

### Defence Layer (`defence`, Stage 5 / E5.6)
- `PromptInjectionDetector` — 49 heuristic patterns + `InjectionClassifier` trait for learned models
- `GoalDriftMonitor` — Jaccard term-overlap similarity, configurable threshold
- `RewardHackingDetector` — completion-claim pattern matching, evidence-threshold gate
- `MotorActionGate` — filesystem/network/self-modification review against `anima-self` capabilities
- `DefenceLayer` — orchestrator with sliding-window veto escalation; `AuditEntry::DefenceVeto` + `AuditEntry::AttentionDemandEscalated`

### Cognitive Layer (`cortex`, Stage 5 / E5.1–E5.3)
- Python service with length-prefixed JSON-over-UDS IPC bridge
- LangGraph-style agent loop: plan / act / observe / revise with configurable termination
- `IdentityMemory` v0 — YAML/JSON file under `~/.anima/<agent_id>/`, atomic write, `anima identity show|set` CLI
- Episode summariser: `archive_episode()` with `SourceTier::Episode` provenance in L3

### Kill-Shot Demonstrations (`xtask`, Stage 5 / E5.8)
- `cargo xtask demo --kind graceful` — graceful-degradation-under-thermal-stress, n=8 runs, two-proportion z-test
- `cargo xtask demo --kind retention` — long-horizon coding-session retention, 40-block fixture, 5 budget variants
- Artefacts written to `artifacts/demos/<date>-<kind>/`; all fixture data embedded (no live API calls)

### MicroVM Kernel (`kernels/microvm`, Stage 4 / E4.1–E4.6)
- UEFI boot trampoline (`x86_64-unknown-uefi`), `BumpAllocator` global allocator
- Embassy async executor (raw, no-arch, `__pender` no-op) + spin-poll loop
- `smoltcp` 0.11 TCP/IP loopback — three-way handshake, client/server exchange
- TLS 1.3 in bare-metal: P-256 ECDHE, AES-128-GCM, SHA-256/HKDF, ECDSA CertificateVerify (RustCrypto), smoltcp transport
- No-std port of `scheduler`, `memory`, `praxis`, `anima-self`, `interoception`, `senses` (`vita` requires std — future work)
- Sleep-cycle soak: VCM pressure, MLFQ boost, L1 pruning, stress index, `run_dream_walk_no_std`
- CI: `microvm-boot` greps COM1 serial for `E4.1_*` … `E4.5_SOAK_DONE`; Miri + Kani in nightly CI

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
