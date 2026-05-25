# 07 — Implementation Plan (Epic Breakdown)

This document restructures the 24-month roadmap in `05-roadmap.md` as a
sequence of delivery **epics**. Each stage of the implementation is a single
epic with a stable identifier, scope statement, dependencies, exit criteria,
and a list of constituent stories. Epics are the unit of planning, branching,
and milestone reporting — stories within an epic may be parallelised, but an
epic is closed only when all of its exit criteria are met.

The epic numbering aligns with the existing milestone numbering in
`05-roadmap.md` (e.g. Epic E1.4 corresponds to milestone M1.4) so that
references in commits, PR titles, and audit logs remain stable across both
documents.

---

## Status legend

| Marker | Meaning |
|--------|---------|
| ✅     | Closed — all exit criteria met, evidence in repo |
| 🟡     | In progress — partial implementation merged |
| ⬜     | Not started |

The status column reflects the current tree at the time this plan was
written (see `README.md` "Implemented Core Interfaces") and must be refreshed
when an epic transitions state.

---

## Stage 1 — Waking Hosted Core

Establish the workspace, the core abstractions, the scheduler, and the
first end-to-end task path on the hosted Linux target.

### Epic E1.1 — Workspace Skeleton and CI Quarantine ✅

**Scope.** Lay down the Cargo workspace, all eight crates, the hosted
kernel binary, and a CI pipeline that enforces the unsafe quarantine.

**Dependencies.** None.

**Stories.**
- S1.1.1 Cargo workspace with the eight crates from `01-architecture.md`.
- S1.1.2 `#![forbid(unsafe_code)]` on every non-`corpus` crate.
- S1.1.3 `kernels/hosted` binary skeleton (`anima-hosted`).
- S1.1.4 GitHub Actions workflow: fmt, build, test, clippy `-D warnings`.
- S1.1.5 Per-crate README files linking back to the design suite.

**Exit criteria.**
1. `cargo build --workspace` and `cargo test --workspace` are clean.
2. CI fails on any unsafe-block leak outside `corpus`.
3. README points to `docs/` and lists implemented interfaces.

### Epic E1.2 — Core Abstractions and Capability Typestate ✅

**Scope.** Define the shared types that the rest of the workspace compiles
against — process control block, syscall surface, and the
`Capability<Unverified>` → `Capability<Verified>` typestate machinery.

**Dependencies.** E1.1.

**Stories.**
- S1.2.1 `AgentPcb`, `AgentPid`, `AgentState` in `corpus`.
- S1.2.2 `SyscallEnum` enumerating the inter-crate call surface.
- S1.2.3 `FrameAllocator` (audited bump allocator) in `corpus`.
- S1.2.4 `Capability` typestate and verification entry point in `anima-self`.
- S1.2.5 Mock implementations sufficient for downstream crates to link.

**Exit criteria.**
1. Workspace compiles with the real types replacing all placeholders.
2. Typestate prevents construction of `Capability<Verified>` outside the
   verification path (compile-fail test).
3. `FrameAllocator` audit log is exercised in a unit test.

### Epic E1.3 — Provider-Agnostic LLM Backend ⬜

**Scope.** A streaming, cancellable, token-counting backend abstraction
with at least two real provider implementations plus a deterministic mock.
The implementations live outside the workspace under `llm-backends/` so the
core crates remain provider-neutral.

**Dependencies.** E1.2 (`CancellationToken`, `Capability`).

**Stories.**
- S1.3.1 `LlmBackend` trait: streaming completions, cancellation, token
  counting, model metadata.
- S1.3.2 Anthropic provider implementation.
- S1.3.3 OpenAI provider implementation.
- S1.3.4 Deterministic mock backend for hermetic testing.
- S1.3.5 Backend selection plumbing in `kernels/hosted`.

**Exit criteria.**
1. Mock backend yields byte-for-byte reproducible streams in tests.
2. Each real backend completes a streamed request against a recorded
   fixture (no live API calls in CI).
3. Cancellation interrupts a long stream within one token of cancellation.

### Epic E1.4 — Three-Tier MLFQ Scheduler 🟡

**Scope.** The reflex-loop dispatcher: a three-tier multi-level feedback
queue with iteration-aware continuous batching and per-task token slicing.

**Dependencies.** E1.2.

**Stories.**
- S1.4.1 `TaskAgenda` with `MlfqTier::High / Medium / Low`.
- S1.4.2 `IterationAwareMlfq::dispatch_task` priority boost/decay policy.
- S1.4.3 Per-task token-slice accounting.
- S1.4.4 Starvation-prevention boost interval.
- S1.4.5 Unit tests covering every tier transition and a starvation soak.

**Exit criteria.**
1. No task starves under a synthetic adversarial workload (1k tasks, 60 s).
2. Tier-transition table is exhaustively tested.
3. Token-slice accounting is consistent with the backend's reported usage
   within 1 token per request.

### Epic E1.5 — Bounded Token Pipe with Credit Backpressure ✅

**Scope.** The inter-crate event bus: bounded SPSC/MPSC pipes with
credit-based backpressure, used for every cross-subsystem signal.

**Dependencies.** E1.4 (pipe is exercised by the scheduler first).

**Stories.**
- S1.5.1 `BoundedTokenPipe` over `crossbeam` ring buffers.
- S1.5.2 Credit accounting on producer/consumer sides.
- S1.5.3 Producer stall semantics on credit exhaustion.
- S1.5.4 Integration test: N producers, 1 consumer, mixed rates, zero loss.

**Exit criteria.**
1. No message loss under a 24-hour producer-stress soak.
2. Producer stall latency bounded by the consumer's drain interval.

### Epic E1.6 — First End-to-End Hosted Run ⬜

**Scope.** Demonstrate the full reflex arc on the hosted kernel: a
sensory packet enters via `senses`, is shepherded by `vita`, dispatched
by `scheduler`, executed by an `LlmBackend`, and the response is logged
to the audit trail. Two concurrent agents must share fairly.

**Dependencies.** E1.3, E1.4, E1.5.

**Stories.**
- S1.6.1 Wire `senses` text-packet ingress into `vita`.
- S1.6.2 `vita` somatic execution loop dispatching tasks via the scheduler.
- S1.6.3 Audit-log sink in `kernels/hosted`.
- S1.6.4 Integration test: two agents, fair token-slice ratio asserted.

**Exit criteria.**
1. End-to-end trace appears in the audit log for each completed task.
2. Fair-share assertion holds for the two-agent integration test.
3. `anima-hosted` runs the demo scenario from `cargo run`.

---

## Stage 2 — Somatic Memory and Tool Bus

The hierarchical memory subsystem and the efferent actuator: routing,
sandboxing, and the L1/L2/L3 transitions that keep the working context
small and the archive durable.

### Epic E2.1 — L1 Block-Structured Context Tracking 🟡

**Scope.** L1 is the live attention window. This epic models it as
block-structured token tracking that maps cleanly onto PagedAttention
semantics, and emits memory-pressure events on the bus.

**Dependencies.** E1.3, E1.5.

**Stories.**
- S2.1.1 `VirtualContextManager` block table.
- S2.1.2 Backend hooks reporting active L1 occupancy.
- S2.1.3 Memory-pressure event emission on the token pipe.

**Exit criteria.**
1. L1 occupancy reported within one block of ground truth.
2. Pressure events fire at the configured high-water mark in tests.

### Epic E2.2 — L2 Warm Cache with ARC Eviction 🟡

**Scope.** A concurrent warm cache (`scc::HashMap`-backed) with the ARC
eviction policy and a defined promotion path back into L1.

**Dependencies.** E2.1.

**Stories.**
- S2.2.1 `ArcCache` over `scc::HashMap`.
- S2.2.2 ARC promotion/demotion ledgers.
- S2.2.3 Promotion-on-retrieval path L2 → L1.

**Exit criteria.**
1. ARC hit-rate matches the reference implementation on the published trace
   set within 1%.
2. Concurrent reader/writer soak with no data races (Miri/loom).

### Epic E2.3 — Praxis Tool Driver Framework 🟡

**Scope.** The `/dev/anima/praxis/tools/` namespace and the
filesystem-style discovery API, plus the length-robust relative routing
filter and a small set of built-in tools.

**Dependencies.** E1.2 (capabilities), E1.5 (event bus).

**Stories.**
- S2.3.1 `ToolDriver` trait and `ToolEnvelope` (MCP/A2A buses).
- S2.3.2 Tool registration and discovery API.
- S2.3.3 `length_robust_filter` relative-routing implementation.
- S2.3.4 Built-in tools: clock, system-event reader, simple text I/O.

**Exit criteria.**
1. Tool registry survives 1k registrations without L1 occupancy drift.
2. Routing filter selects the correct tool on the documented benchmark set.

### Epic E2.4 — Per-Tool Circuit Breakers ✅

**Scope.** Each tool dispatch path is wrapped by a Closed/Open/HalfOpen
circuit breaker whose state is exposed to interoception.

**Dependencies.** E2.3.

**Stories.**
- S2.4.1 `CircuitBreaker` state machine with configurable cooldown.
- S2.4.2 Wire breakers into the `praxis` dispatch path.
- S2.4.3 Telemetry export of breaker state.

**Exit criteria.**
1. State transitions covered by exhaustive table-driven tests.
2. Telemetry stream reflects state change within the next tick.

### Epic E2.5 — Wasmtime Sandbox for Untrusted Tools ⬜

**Scope.** Host a Wasmtime runtime under `praxis/compute/` with gas
metering, memory limits, and capability-based imports. Ship one sample
WASI tool that exercises the full sandbox surface.

**Dependencies.** E2.3.

**Stories.**
- S2.5.1 Wasmtime runtime initialised once and shared.
- S2.5.2 Gas meter integrated with the scheduler's token slice.
- S2.5.3 Capability-gated WASI imports.
- S2.5.4 Sample sandboxed math evaluator.

**Exit criteria.**
1. Adversarial WASI module (infinite loop, memory exhaustion attempt) is
   bounded inside the configured limits.
2. Wasmtime startup cost amortised across the process lifetime
   (one-time init).

### Epic E2.6 — LanceDB L3 Archive ⬜

**Scope.** An embedded LanceDB instance under `/dev/anima/memory/l3`,
with the embedding pipeline and bidirectional L2↔L3 paths.

**Dependencies.** E2.2.

**Stories.**
- S2.6.1 LanceDB embed and lifecycle management.
- S2.6.2 Embedding pipeline for memory entries.
- S2.6.3 L2 → L3 demotion path with provenance.
- S2.6.4 L3 → L2 retrieval via similarity scoring.

**Exit criteria.**
1. L3 survives a process restart with consistent retrieval.
2. Demotion is idempotent; retrieval is deterministic for fixed seeds.

---

## Stage 3 — Interoception and the Autonomic Sleep Cycle

The interoceptive monitor and the full four-phase sleep cycle. This is
the stage that turns a well-architected runtime into a living one.

### Epic E3.1 — Kernel Trace Hooks and Rolling TTFT ✅

**Scope.** Latency instrumentation across the hot paths and a rolling
TTFT window suitable for driving stress thresholds.

**Dependencies.** E1.6, E2.1.

**Stories.**
- S3.1.1 Trace hooks at `senses`, `scheduler`, `praxis` boundaries.
- S3.1.2 `record_ttft` rolling window.
- S3.1.3 Token-count tracking tied to memory tier state.

**Exit criteria.**
1. TTFT window stays within configured memory budget across a 24-hour soak.
2. Trace overhead under 2% on a representative workload.

### Epic E3.2 — Homeostatic Stress Index ✅

**Scope.** The `HomeostaticMonitor` computes a 1 Hz systemic stress
index from interoceptive signals and emits threshold-driven events.

**Dependencies.** E3.1.

**Stories.**
- S3.2.1 `HomeostaticMonitor::compute_systemic_stress_index`.
- S3.2.2 Threshold configuration and event emission.
- S3.2.3 Telemetry stream at 1 Hz.

**Exit criteria.**
1. Stress index reproducible from a recorded trace.
2. Threshold events fire deterministically in tests.

### Epic E3.3 — Sensory Bridge (Text and Voice) 🟡

**Scope.** The afferent input surface: a text socket and a PCM voice
pipeline that produces `SensoryPacket`s with priorities.

**Dependencies.** E1.6.

**Stories.**
- S3.3.1 `/dev/anima/senses/human` text-input socket.
- S3.3.2 PCM streaming socket → VAD → local STT.
- S3.3.3 `SensoryPacket` envelope and priority assignment.
- S3.3.4 `HumanGuidance` policy bounds.

**Exit criteria.**
1. Text and voice both reach `vita` as priority-tagged packets.
2. Policy bounds reject out-of-policy inputs without panicking.

### Epic E3.4 — Wake/Sleep State Transitions ⬜

**Scope.** Drive transitions between waking and sleeping based on
stress and agenda state, and sequence the four sleep phases.

**Dependencies.** E3.2, E3.3.

**Stories.**
- S3.4.1 Wake → Sleep on (stress high ∧ agenda empty).
- S3.4.2 Sleep → Wake on sensory event.
- S3.4.3 Phase sequencer: Pruning → Replay → Dreaming → Compilation.

**Exit criteria.**
1. Transitions audited end-to-end in the log.
2. 100 consecutive sleep cycles complete without error in the soak test.

### Epic E3.5 — Pruning Phase with Emotional Decay ⬜

**Scope.** The pruning phase implements `S(t)` activation decay against
the semantic floor in both L1 and L2.

**Dependencies.** E2.2, E3.4.

**Stories.**
- S3.5.1 `MemoryNode::activation_at` decay model.
- S3.5.2 L1 and L2 pruning routines.
- S3.5.3 Semantic floor enforcement.

**Exit criteria.**
1. Pruning bounded by the configured floor under stress injection.
2. No retained entry has activation below the floor after a pass.

### Epic E3.6 — Replay Validation with Rollback ⬜

**Scope.** Generative replay against the L3 audit stream, with rollback
when degradation crosses the configured threshold.

**Dependencies.** E2.6, E3.5.

**Stories.**
- S3.6.1 Replay sampling from L3.
- S3.6.2 Accuracy threshold checker.
- S3.6.3 Rollback path for prior pruning changes.

**Exit criteria.**
1. Soak test demonstrates at least one rollback (proof the path works).
2. Validation accuracy logged for every cycle.

### Epic E3.7 — Dreaming Phase ⬜

**Scope.** Random graph walks across L3 produce associative-edge
candidates that feed the next pruning cycle for validation.

**Dependencies.** E2.6, E3.6.

**Stories.**
- S3.7.1 Random-walk sampler with seeded determinism.
- S3.7.2 Candidate edge generation.
- S3.7.3 Hand-off to the next pruning cycle.

**Exit criteria.**
1. Candidate yield is logged and monotonic-reproducible per seed.
2. Bad candidates are filtered out by the subsequent pruning pass.

### Epic E3.8 — Compilation Phase: Trace → Training Pairs ⬜

**Scope.** Compile the cycle's traces into all three output training
formats and persist them under `training_corpus/` in L3.

**Dependencies.** E3.6.

**Stories.**
- S3.8.1 Trace-to-pair compiler for each format.
- S3.8.2 Persistence under `training_corpus/`.
- S3.8.3 Final close-out of the sleep cycle.

**Exit criteria.**
1. Output corpora validate against the documented schemas.
2. Emergency consolidation can trigger and recover under stress injection.

---

## Stage 4 — Bare-Metal Isolation and Production Verification

Port to the microVM target, integrate `smoltcp` and `rustls`, complete
the formal verification surface, and harden for production.

### Epic E4.1 — `corpus` `no_std` Port ⬜

**Scope.** Compile `corpus` under `no_std` with a custom allocator and a
UEFI boot trampoline that reaches a panic-handler-only state in QEMU.

**Dependencies.** End of Stage 3.

**Stories.**
- S4.1.1 `no_std`-clean `corpus`.
- S4.1.2 Custom allocator integration.
- S4.1.3 UEFI boot trampoline.

**Exit criteria.**
1. QEMU boots the trampoline image and reaches the panic handler under a
   deliberate panic.

### Epic E4.2 — Embassy Runtime Inside `corpus` ⬜

**Scope.** Embed Embassy's async executor in the kernel and run the
first kernel-level task to completion.

**Dependencies.** E4.1.

**Exit criteria.**
1. A scheduled async task completes and signals via the audit channel.

### Epic E4.3 — `smoltcp` TCP/IP Stack ⬜

**Scope.** Bring up `smoltcp` at boot against virtio-net for the
Firecracker target.

**Dependencies.** E4.2.

**Exit criteria.**
1. First outbound TCP connection from inside the microVM succeeds.

### Epic E4.4 — `rustls` Over `smoltcp` ⬜

**Scope.** TLS termination over the `smoltcp` stack and a demonstrated
outbound TLS call to an LLM provider.

**Dependencies.** E4.3.

**Exit criteria.**
1. End-to-end TLS handshake completes against a real provider in the
   nightly integration job.

### Epic E4.5 — Higher Crates Ported to MicroVM ⬜

**Scope.** Port `vita`, `scheduler`, `memory`, `praxis`, `anima-self`,
`interoception`, and `senses` to the microVM target. Promote the
microVM target to production status; retain hosted for development.

**Dependencies.** E4.4.

**Exit criteria.**
1. The Stage 3 sleep-cycle soak passes inside the microVM target.

### Epic E4.6 — Formal Verification Rollout ⬜

**Scope.** Kani proofs for scheduler invariants, rate limiters, and the
ring buffer; Miri clean on the `corpus` test suite; both integrated into
nightly CI.

**Dependencies.** E4.5 (verification surface stabilises after the port).

**Exit criteria.**
1. All declared Kani proofs pass in nightly CI.
2. Miri runs clean on the `corpus` suite.

### Epic E4.7 — Production Hardening and 30-Day Soak ⬜

**Scope.** Boot-time and image-size optimisation, regression benchmark
suite, and a continuous 30-day soak run.

**Dependencies.** E4.6.

**Exit criteria.**
1. MicroVM boots within 2 s under Firecracker and Cloud Hypervisor.
2. 30-day soak completes without unscheduled restart and with stable
   memory and audit-log integrity.
3. Documentation updates make the microVM target primary and mark the
   hosted target development-only.

---

## Cross-Cutting Epics

These epics run continuously across stages rather than belonging to a
single stage. They are tracked separately so that their progress does
not block stage closure.

### Epic EX.1 — Documentation in Lockstep 🟡

Keep the `docs/` suite synchronised with the code. Every PR that
changes a public interface updates the relevant section here.

### Epic EX.2 — Audit Log and Telemetry Pipeline 🟡

A single durable audit log and a telemetry export that is consumed by
both development tooling and the homeostatic monitor. Owners change as
stages progress; the epic remains open.

### Epic EX.3 — Performance Regression Benchmark Suite ⬜

A per-PR microbenchmark suite (Criterion) plus a nightly macro-benchmark
job. Begins in Stage 2 once the memory hierarchy is stable; tightens in
Stage 4.

### Epic EX.4 — Security Posture and Threat Model ⬜

Maintain a living threat model, run `cargo audit` and `cargo deny` in
CI, and produce a security review at the end of each stage.

---

## Parallelisation Notes

- Stage-1 epics E1.3, E1.4, and E1.5 can proceed in parallel once E1.2
  is closed.
- Stage-2 memory epics (E2.1, E2.2, E2.6) and praxis epics (E2.3, E2.4,
  E2.5) form two parallel tracks that converge at the end of the stage.
- Stage-3 sleep epics are strictly sequential after E3.4.
- Stage-4 epics are strictly sequential up to E4.5; E4.6 may begin on
  stable crates during Stage 2.
- All cross-cutting epics run in parallel with every stage.

## What Counts as Stage Closure

A stage closes only when every constituent epic is ✅ and the stage-level
exit criteria documented in `05-roadmap.md` are demonstrably met by a
green CI run plus a referenced audit-log trace. Rolling incomplete epics
across stage boundaries is explicitly disallowed.
