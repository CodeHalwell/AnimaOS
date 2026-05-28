# AnimaOS

AnimaOS is a bare-metal, cloud-isolated framekernel OS intended to act as the
somatic architecture (physical body, autonomic nervous system, and reflex arcs)
for an autonomous LLM agent process.

See [`docs/`](./docs/README.md) for the full design suite.

## Workspace Layout

```
anima-os/
├── .github/workflows/         # ci.yml, nightly.yml, bench.yml, pages.yml
├── Cargo.toml
├── crates/
│   ├── corpus/                # The body (TCB): frame allocator, PCB, syscall enum
│   ├── vita/                  # Autonomous lifecycle director + sleep routines + router/gate
│   ├── scheduler/             # 3-tier MLFQ, bounded token pipe, LlmBackend
│   ├── memory/                # CLS L1/L2/L3, ARC cache, emotional decay, TurboQuant
│   ├── praxis/                # Efferent actuator: routing, circuit breaker, MCP/A2A envelopes
│   ├── self/                  # Self/non-self barrier: typestate capability tokens
│   ├── interoception/         # Stress index, TTFT window, sensory bridge
│   ├── senses/                # Afferent input: text / PCM packetization
│   ├── kv-controller/         # Learned KV-cache gate over TurboQuant (Stage 5, E5.4)
│   └── defence/               # Defence layer: prompt-injection / drift detection (Stage 5, E5.6)
├── kernels/
│   ├── hosted/                # Linux process emulation binary (`anima-hosted`)
│   └── microvm/               # UEFI no_std kernel, Firecracker / Cloud Hypervisor (E4.1–E4.6)
├── cortex/                    # Python cognitive layer MVP (Stage 5, E5.1)
├── llm-backends/              # Out-of-workspace providers (Anthropic, OpenAI, Ollama, mock)
├── trainer/                   # Sleep-phase Unsloth QLoRA harness
├── xtask/                     # Kill-shot demo runner (separate workspace)
└── docker/                    # docker-compose stack: anima-hosted + Ollama + Unsloth
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
cargo build --workspace
cargo test --workspace
cargo run -p hosted --bin anima-hosted
```

CI runs `cargo fmt --check`, build, test, and `cargo clippy -- -D warnings`.

## Deployment

Two parallel surfaces share the same workspace code — see
[`docs/10-deployment-pathways.md`](./docs/10-deployment-pathways.md) for the
full rationale.

### Containerised (now)

`anima-hosted` + Ollama (llama.cpp inference) + Unsloth trainer
(profile-gated, for sleep-phase QLoRA), all wired through
docker-compose with NVIDIA GPU passthrough:

```sh
docker compose up --build                       # inference stack
docker compose --profile training up --build    # also build the trainer
```

Operational details — model defaults, env vars, VRAM budget on a 3090,
known limitations — live in [`docker/README.md`](./docker/README.md).

### Bare-metal native (target)

Three flavours (host-native sidecar → in-process Rust inference →
microVM framekernel) documented in
[`docs/10-deployment-pathways.md`](./docs/10-deployment-pathways.md).
Each step preserves the `LlmBackend` trait surface so cognitive code
never needs rewriting.
