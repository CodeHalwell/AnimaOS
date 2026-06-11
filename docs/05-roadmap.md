# 05 — Implementation Roadmap

A 24-month phased plan from current state (design specification only) to a production microVM image booting natively as an isolated unit. The plan is deliberately conservative on calendar time and aggressive on per-phase exit criteria: it is better to take longer than to declare a phase complete prematurely.

## Overview

| Phase | Months | Focus | Exit |
|-------|--------|-------|------|
| Phase 1 | 1–3 | Waking hosted core | Multiple agents executing tasks with fair token allocation |
| Phase 2 | 4–6 | Memory and tool bus | Dynamic tool routing without context pollution |
| Phase 3 | 7–12 | Interoception and sleep | Clean wake/sleep transitions driven by stress |
| Phase 4 | 13–24 | Bare-metal and verification | MicroVM booting natively with all subsystems |

Each phase has hard exit criteria. A phase is not complete until all criteria pass — no rolling-over of incomplete work.

---

## Phase 1: Waking Hosted Core (Months 1–3)

### Focus

Establish the core task execution architecture, state representation, and the hosted kernel target. At the end of this phase, Anima runs as an ordinary Linux process that demonstrates the core architectural patterns even though it does not yet do anything biologically interesting.

### Milestones

**M1.1 — Workspace skeleton (Week 1–2)**

- Cargo workspace structure matching the layout in `01-architecture.md`.
- All eight crates created with placeholder `lib.rs` files, correct `#![forbid(unsafe_code)]` attributes, and basic dependency declarations.
- CI pipeline running Level 1 checks (fmt, clippy, unsafe quarantine).
- README files for each crate linking to the relevant section of the documentation suite.

**M1.2 — Core abstractions (Week 3–4)**

- `AgentPCB` (Agent Process Control Block) defined in `corpus`.
- `SyscallEnum` for inter-crate calls.
- `TaskId`, `Priority`, `Capability` types with their typestate machinery in `self`.
- Mock implementations sufficient for the rest of the workspace to compile against.

**M1.3 — Provider-agnostic LLM backend (Week 5–6)**

- `LlmBackend` trait supporting streaming completions, cancellation, and token counting.
- Implementations against at least two providers (Anthropic and OpenAI APIs are sensible defaults).
- A mock backend for testing with deterministic outputs.
- The backend lives outside the workspace crates as a separate `llm-backends/` directory.

**M1.4 — MLFQ scheduler (Week 7–8)**

- Three priority levels with the boost-and-decay policy from the design.
- Iteration-aware continuous batching at the queue level.
- Per-task token-slice tracking.
- Unit tests covering all level transitions and starvation scenarios.

**M1.5 — Bounded token pipes with credit backpressure (Week 9–10)**

- The inter-crate event bus using `crossbeam` ring buffers.
- Credit-based backpressure: producers stall when consumers fall behind.
- Integration test: multiple producers feeding a single consumer at varying rates, no message loss.

**M1.6 — First end-to-end run (Week 11–12)**

- A single agent task executing through the full hosted-kernel path.
- Senses → vita → scheduler → LlmBackend → response → audit log.
- Demonstrated with at least two concurrent agents.

### Exit Criteria

1. Workspace builds clean (fmt, clippy, unsafe quarantine).
2. Unit test coverage targets met for `corpus`, `vita`, `scheduler`.
3. Two concurrent agents executing tasks with fair token-slice allocation, verified by integration test.
4. End-to-end trace visible in audit log for each completed task.

### Risks

- **LLM backend rate limits during development.** Mitigation: heavy use of the mock backend; real backends only for end-to-end verification.
- **Scheduler complexity creep.** The MLFQ design has many degrees of freedom; resist the temptation to optimise before there is workload data.

---

## Phase 2: Somatic Memory and Tool Bus (Months 4–6)

### Focus

Build the memory hierarchy and the praxis subsystem. At the end of this phase, the agent can manage its own context across L1/L2/L3 and can dynamically route tool calls through circuit breakers and sandboxes.

### Milestones

**M2.1 — L1 block-structured tracking (Week 13–14)**

- L1 implemented as block-structured token tracking mapped to PagedAttention semantics.
- Hooks into the LlmBackend trait so backend implementations can report active L1 use.
- Memory pressure events emitted on the event bus.

**M2.2 — L2 concurrent cache (Week 15–16)**

- `scc::HashMap`-backed L2 layer.
- ARC eviction policy implemented and tested.
- Promotion path from L2 to L1 driven by retrieval query matches.

**M2.3 — Praxis tool driver framework (Week 17–18)**

- The `/dev/anima/praxis/tools/` namespace with file-system semantics.
- Tool registration and discovery API.
- Length-robust relative routing filter.
- Initial set of built-in tools: clock, system event reader, simple text I/O.

**M2.4 — Circuit breakers (Week 19)**

- Per-tool circuit breakers wired into the praxis dispatch path.
- Closed/Open/HalfOpen state transitions with configurable timeouts.
- Breaker state exposed via interoception telemetry.

**M2.5 — Wasmtime sandbox integration (Week 20–21)**

- Wasmtime runtime hosted under `praxis/compute/`.
- Gas metering, memory limits, capability-based imports.
- A sample sandboxed tool: a math evaluator written as a WASI module.

**M2.6 — LanceDB L3 archive (Week 22–24)**

- Embedded LanceDB instance under `/dev/anima/memory/l3`.
- Vector embedding pipeline for memory entries.
- L2 → L3 demotion path.
- L3 → L2 retrieval path driven by similarity scoring.

### Exit Criteria

1. Memory tier transitions verified by integration test in both directions.
2. Tool routing demonstrated against at least 20 registered tools without context pollution (measured by L1 occupancy delta when tools are added to the registry).
3. Wasmtime sandbox demonstrated to bound a deliberately misbehaving tool (infinite loop, memory exhaustion attempt) within gas/memory limits.
4. L3 archive survives a process restart with consistent retrieval.

### Risks

- **LanceDB integration friction.** As an embedded vector DB, LanceDB may have quirks under the `no_std`-adjacent constraints we will eventually need. Mitigation: keep LanceDB behind a small interface so it can be swapped if necessary.
- **Wasmtime compilation cost.** Wasmtime is a substantial dependency. Mitigation: lazy initialisation, single shared runtime instance, careful feature flag selection.

---

## Phase 3: Interoception and the Autonomic Sleep Cycle (Months 7–12)

### Focus

Real-time feedback monitoring and the full sleep cycle. This is the phase that makes Anima distinctively alive rather than merely well-architected. The longer schedule reflects the inherent complexity of getting the homeostatic loop to behave well under varied loads.

### Milestones

**M3.1 — Kernel trace hooks (Month 7)**

- Latency tracking instrumentation across the hot paths.
- Rolling-window TTFT calculation.
- Token-count tracking integrated with memory tier states.

**M3.2 — Stress index calculation (Month 7)**

- `HomeostaticMonitor` implementation matching the spec in `02-subsystems.md`.
- Stress index visible on the telemetry stream at 1 Hz.
- Threshold-driven events on the event bus.

**M3.3 — Sensory bridge (Month 8)**

- `/dev/anima/senses/human` mount point for text input via Unix socket.
- Voice input pipeline: PCM streaming socket → VAD → text via local speech-to-text.
- Sensory event envelope and priority assignment.

**M3.4 — Sleep state transitions (Month 9)**

- Wake → Sleep transition driven by stress + empty agenda.
- Sleep → Wake transition driven by sensory events.
- Phase progression: Pruning → Replay → Dreaming → Compilation.

**M3.5 — Pruning phase implementation (Month 9)**

- Emotional decay model implemented in `memory`.
- L1 and L2 pruning routines.
- Semantic floor enforcement.

**M3.6 — Replay validation (Month 10)**

- Generative replay sampling from the L3 audit stream.
- Accuracy threshold checking.
- Rollback on degradation.

**M3.7 — Dreaming phase (Month 11)**

- Random graph walks across L3.
- Associative edge candidate generation.
- Candidate validation feeding into the next pruning cycle.

**M3.8 — Compilation phase (Month 12)**

- Trace-to-training-pair compilation for all three output formats.
- Persistence under `training_corpus/` in L3.
- Final sleep cycle close-out.

### Exit Criteria

1. System cleanly transitions between Waking and Sleeping based on stress and agenda state.
2. Each of the four sleep phases runs to completion on at least 100 consecutive sleep cycles without error.
3. Generative replay validation rolls back at least one pruning change in soak test (proof that the validation path works).
4. Emergency consolidation triggers and recovers under deliberate stress injection.
5. Audit log shows complete lifecycle history with no gaps.

### Risks

- **Sleep cycle tuning.** Default thresholds for pruning, validation degradation, and stress are guesses; real workloads will require tuning. Mitigation: extensive soak testing with telemetry export, parameter sweeps in CI.
- **Dreaming quality.** Random walks may produce mostly useless edges. Mitigation: the validation step in the next pruning cycle filters bad candidates; we accept that dreaming yield is variable by design.

---

## Phase 4: Bare-Metal Isolation and Production Verification (Months 13–24)

### Focus

Port to bare-metal microVM, integrate `smoltcp` and `rustls`, complete the formal verification surface, and prepare for production deployment.

### Milestones

**M4.1 — corpus `no_std` port (Months 13–14)**

- All `corpus` code compiles under `no_std`.
- Custom allocator integrated.
- UEFI boot trampoline.
- Boots to a panic-handler-only state in QEMU.

**M4.2 — Embassy runtime in corpus (Month 15)**

- Async executor running in the kernel.
- First task scheduled and completed at the kernel level.

**M4.3 — `smoltcp` integration (Months 16–17)**

- TCP/IP stack initialised at boot.
- Network interface driver for virtio-net (Firecracker target).
- First TCP connection established from inside the microVM.

**M4.4 — `rustls` integration (Month 18)**

- TLS termination over `smoltcp`.
- Outbound TLS to an LLM provider API demonstrated.

**M4.5 — Higher crates ported (Months 19–20)**

- `vita`, `scheduler`, `memory`, `praxis`, `self`, `interoception`, `senses` running in the microVM.
- Hosted target retained for development; microVM target promoted to production.

**M4.6 — Formal verification rollout (Months 21–22)**

- Kani proofs written for scheduler invariants, rate limiters, ring buffer.
- Miri running clean on `corpus` test suite.
- All proofs and miri runs integrated into nightly CI.

**M4.7 — Production hardening (Months 23–24)**

- MicroVM image size optimised. ✅ — release EFI ≤ 1 MiB (CI gated in
  `ci.yml` step `Enforce EFI image-size budget (E4.7.1)`).
- Boot time optimised to under 2 seconds. 🟡 — `ci.yml` `microvm-boot`
  records QEMU wall-clock boot latency for information only; QEMU+OVMF
  on a shared runner (full firmware POST) is not a representative
  measurement and is deliberately not gated.  The 2 s budget applies to
  Firecracker / Cloud Hypervisor and is asserted as part of the
  hardware soak run (`docs/22` §1).
- Soak testing: 30-day continuous run without restart. 🟡 — harness
  in `xtask soak`, manifest schema + CI smoke test in
  `.github/workflows/soak.yml`; the 720-hour run itself is operator-
  driven and committed under `artifacts/soak/`.
- Performance regression benchmark suite established. ✅ — checked-in
  baselines at `bench/baselines/<crate>.json`, `xtask bench-baseline`
  comparison tool, and `bench.yml` gates every PR against them.

### Exit Criteria

1. Anima boots as a microVM under Firecracker and Cloud Hypervisor within 2 seconds.
2. Full subsystem behaviour matches the hosted target.
3. All Kani proofs pass; miri clean on `corpus`.
4. 30-day soak test completes without unscheduled restart, with stable memory and audit log integrity.
5. Documentation updated to reflect the production target as primary; hosted target documented as development-only.

### Risks

- **Bare-metal driver work.** `smoltcp` is solid but integration with virtio devices may require novel work. Mitigation: budget includes 2 months for network stack alone.
- **Formal verification scope creep.** It is easy to want to prove more than is necessary. Mitigation: the verification doc lists what we prove; expansions require explicit scope approval.
- **Performance regressions at the bare-metal boundary.** The hosted target's performance is not the production target's performance. Mitigation: per-PR benchmark in Phase 4, regression alerts.

---

## Parallelisation

The phases are sequenced but not strictly serial. Some work can be parallelised:

- The documentation suite can be revised continuously in parallel with all phases.
- The verification infrastructure (Phase 4 M4.6) can begin in Phase 2 for the crates that are stable.
- The hosted target's polish can continue throughout Phase 4 if there are developers available.

What cannot be parallelised:

- Phase 3 depends on Phase 2's memory hierarchy being complete and stable.
- Phase 4's bare-metal port depends on Phase 3's homeostatic loop being well-characterised in the hosted target.

## Team Shape

This roadmap assumes a small team — three to five engineers — with at least one having deep Rust systems experience and at least one with prior LLM agent infrastructure experience. With a larger team, the parallelisation opportunities expand but the integration risk grows; the 24-month total stays roughly constant.

A single engineer working alone could deliver this roadmap but would likely need 36 months. The bottleneck is not the code volume; it is the integration testing, the verification setup, and the soak time required to validate the homeostatic loop.

## What Counts as "Done"

The end of Phase 4 is not the end of work. It is the point at which Anima is a viable substrate for production agent deployment, with verified subsystems and a tractable audit surface. Beyond that point, work shifts to:

- Hardening against adversarial inputs.
- Optimising specific workload patterns.
- Building higher-level agent frameworks on top of the substrate.

These are out of scope for this roadmap and would be specified separately.
