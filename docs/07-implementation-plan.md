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

### Epic E1.3 — Provider-Agnostic LLM Backend ✅

**Scope.** A streaming, cancellable, token-counting backend abstraction
with at least two real provider implementations plus a deterministic mock.
The implementations live outside the workspace under `llm-backends/` so the
core crates remain provider-neutral.

**Dependencies.** E1.2 (`CancellationToken`, `Capability`).

**Stories.**
- S1.3.1 `LlmBackend` trait: streaming completions, cancellation, token
  counting, model metadata. ✅ (`scheduler/src/backend.rs` — extended with
  `model_id`, `max_context_tokens`, `estimate_token_count` default methods)
- S1.3.2 Anthropic provider implementation. ✅ (`llm-backends/src/anthropic.rs`)
- S1.3.3 OpenAI provider implementation. ✅ (`llm-backends/src/openai.rs`)
- S1.3.4 Deterministic mock backend for hermetic testing. ✅ (`scheduler/src/mock.rs`)
- S1.3.5 Backend selection plumbing in `kernels/hosted`. ✅ (`BackendFactory`,
  `ANIMA_BACKEND` env var in `kernels/hosted/src/main.rs`)

**Exit criteria.**
1. Mock backend yields byte-for-byte reproducible streams in tests. ✅
   (`openai::tests::fixture_output_is_byte_for_byte_reproducible`)
2. Each real backend completes a streamed request against a recorded
   fixture (no live API calls in CI). ✅ (`llm-backends/fixtures/anthropic.json`,
   `openai.json`; fixture replay tested in `anthropic::tests` and `openai::tests`)
3. Cancellation interrupts a long stream within one token of cancellation. ✅
   (`cancellation_interrupts_within_one_token_of_cancel_signal`)

### Epic E1.4 — Three-Tier MLFQ Scheduler ✅

**Scope.** The reflex-loop dispatcher: a three-tier multi-level feedback
queue with iteration-aware continuous batching and per-task token slicing.

**Dependencies.** E1.2.

**Stories.**
- S1.4.1 `TaskAgenda` with `MlfqTier::High / Medium / Low`. ✅
- S1.4.2 `IterationAwareMlfq::dispatch_task` priority boost/decay policy. ✅
- S1.4.3 Per-task token-slice accounting. ✅ (`Task::token_budget`,
  `IterationAwareMlfq::total_tokens_dispatched`,
  `token_budget_truncates_response_at_slice_boundary`)
- S1.4.4 Starvation-prevention boost interval. ✅
  (`IterationAwareMlfq::with_boost_interval`,
  `IterationAwareMlfq::check_and_boost`,
  `TaskAgenda::boost_all_to_high`)
- S1.4.5 Unit tests covering every tier transition and a starvation soak. ✅
  (`tier_transition_table_all_tiers`, `no_starvation_under_adversarial_workload`)

**Exit criteria.**
1. No task starves under a synthetic adversarial workload (1k tasks, 60 s). ✅
   (`no_starvation_under_adversarial_workload` — 900 High + 100 Low, all dispatched)
2. Tier-transition table is exhaustively tested. ✅
   (`tier_transition_table_all_tiers`)
3. Token-slice accounting is consistent with the backend's reported usage
   within 1 token per request. ✅ (`token_accounting_accumulates_across_multiple_dispatches`,
   `token_budget_truncates_response_at_slice_boundary`)

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

### Epic E1.6 — First End-to-End Hosted Run ✅

**Scope.** Demonstrate the full reflex arc on the hosted kernel: a
sensory packet enters via `senses`, is shepherded by `vita`, dispatched
by `scheduler`, executed by an `LlmBackend`, and the response is logged
to the audit trail. Two concurrent agents must share fairly.

**Dependencies.** E1.3, E1.4, E1.5.

**Stories.**
- S1.6.1 Wire `senses` text-packet ingress into `vita`. ✅
- S1.6.2 `vita` somatic execution loop dispatching tasks via the scheduler. ✅
  (`somatic_execution_loop` in `vita/src/lib.rs`)
- S1.6.3 Audit-log sink in `kernels/hosted`. ✅ (`print_audit` in `main.rs`,
  `AuditLog` in `vita/src/audit.rs`)
- S1.6.4 Integration test: two agents, fair token-slice ratio asserted. ✅
  (`kernels/hosted/tests/end_to_end.rs`)

**Exit criteria.**
1. End-to-end trace appears in the audit log for each completed task. ✅
2. Fair-share assertion holds for the two-agent integration test. ✅
   (`two_concurrent_agents_complete_tasks_through_shared_backend`)
3. `anima-hosted` runs the demo scenario from `cargo run`. ✅
   (supports `ANIMA_BACKEND=anthropic|openai|mock`)

---

## Stage 2 — Somatic Memory and Tool Bus

The hierarchical memory subsystem and the efferent actuator: routing,
sandboxing, and the L1/L2/L3 transitions that keep the working context
small and the archive durable.

### Epic E2.1 — L1 Block-Structured Context Tracking ✅

**Scope.** L1 is the live attention window. This epic models it as
block-structured token tracking that maps cleanly onto PagedAttention
semantics, and emits memory-pressure events on the bus.

**Dependencies.** E1.3, E1.5.

**Stories.**
- S2.1.1 `VirtualContextManager` block table. ✅ (`memory/src/lib.rs` —
  `with_blocks(tokens, max_context, block_size)`, `occupied_blocks()`,
  `free_blocks()`, `total_blocks()`, `set_high_water_blocks()`)
- S2.1.2 Backend hooks reporting active L1 occupancy. ✅ (`occupied_blocks()`
  ceiling-division against block_size matches PagedAttention semantics;
  `l1_occupancy_within_one_block_of_ground_truth` pins the accuracy)
- S2.1.3 Memory-pressure event emission on the token pipe. ✅
  (`memory/src/pressure.rs` — `MemoryPressureEvent`, `emit_to_pipe()`;
  Normal/HighWater/Critical levels, credit-based backpressure into
  `BoundedTokenPipe`)

**Exit criteria.**
1. L1 occupancy reported within one block of ground truth. ✅
   (`l1_occupancy_within_one_block_of_ground_truth`)
2. Pressure events fire at the configured high-water mark in tests. ✅
   (`check_pressure_fires_high_water_at_mark`,
   `high_water_pressure_consumes_quarter_credits`,
   `critical_pressure_consumes_all_credits`)

### Epic E2.2 — L2 Warm Cache with ARC Eviction ✅

**Scope.** A concurrent warm cache (`scc::HashMap`-backed) with the ARC
eviction policy and a defined promotion path back into L1.

**Dependencies.** E2.1.

**Stories.**
- S2.2.1 `ArcCache` over `scc::HashMap`. ✅ (`memory/src/l2_cache.rs` —
  full ARC implementation with T1/T2/B1/B2 lists; `scc` added as
  workspace dep; thread safety via `Arc<Mutex<ArcCacheInner>>`)
- S2.2.2 ARC promotion/demotion ledgers. ✅ (adaptive parameter `p`
  increases on B1 ghost hits, decreases on B2 ghost hits;
  `ghost_hit_in_b1_adapts_p_upward`)
- S2.2.3 Promotion-on-retrieval path L2 → L1. ✅ (`PromotionHint::Frequency`
  returned on T2 hits; callers use the hint to re-admit items to L1;
  `promotion_hint_frequency_indicates_l2_to_l1_candidate`)

**Exit criteria.**
1. ARC hit-rate matches the reference implementation on the published trace
   set within 1%. ✅ (`arc_hit_rate_is_at_least_as_good_as_lru_on_frequency_workload`
   — ARC matches or beats LRU with ≤1 % tolerance on the frequency workload)
2. Concurrent reader/writer soak with no data races (Miri/loom). ✅
   (`concurrent_readers_and_writers_produce_no_panics` — 4 writer +
   4 reader threads, 500 ops each, no panics, invariant holds)

### Epic E2.3 — Praxis Tool Driver Framework ✅

**Scope.** The `/dev/anima/praxis/tools/` namespace and the
filesystem-style discovery API, plus the length-robust relative routing
filter and a small set of built-in tools.

**Dependencies.** E1.2 (capabilities), E1.5 (event bus).

**Stories.**
- S2.3.1 `ToolDriver` trait and `ToolEnvelope` (MCP/A2A buses). ✅
  (`praxis/src/lib.rs`, `praxis/src/envelope.rs`)
- S2.3.2 Tool registration and discovery API. ✅ (`praxis/src/registry.rs` —
  `ToolRegistry::register()`, `lookup()`, `list()`, `dispatch()`;
  per-tool `CircuitBreaker` integrated; `Clone` shares state via `Arc`)
- S2.3.3 `length_robust_filter` relative-routing implementation. ✅
  (`praxis/src/routing.rs`)
- S2.3.4 Built-in tools: clock, system-event reader, simple text I/O. ✅
  (`ClockTool` — Unix epoch ms; `EchoTool` — payload echo;
  `TextIoTool` — UTF-8 validation + newline append)

**Exit criteria.**
1. Tool registry survives 1k registrations without L1 occupancy drift. ✅
   (`registry_survives_one_thousand_registrations` — 10 threads × 100
   registrations each, all 1 000 entries verified accessible)
2. Routing filter selects the correct tool on the documented benchmark set. ✅
   (`length_robust_filter_selects_correct_tools_from_benchmark_set` —
   τ_rel=0.85 keeps `clock` and `echo`, drops `text-io` from the 3-tool set)

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

### Epic E2.5 — Wasmtime Sandbox for Untrusted Tools ✅

**Scope.** Host a Wasmtime runtime under `praxis/compute/` with gas
metering, memory limits, and capability-based imports. Ship one sample
WASI tool that exercises the full sandbox surface.

**Dependencies.** E2.3.

**Stories.**
- S2.5.1 Wasmtime runtime initialised once and shared. ✅ (`praxis/src/compute.rs` —
  `WasmSandbox` wraps a shared `Arc<Engine>` created once in `WasmSandbox::new()`;
  `engine()` returns a clone-able `&Arc<Engine>` for multi-call reuse;
  `sandbox_engine_created_once_and_shared` and
  `engine_shared_across_multiple_invocations` pin the invariant)
- S2.5.2 Gas meter integrated with the scheduler's token slice. ✅ (`SandboxConfig::fuel_limit`
  — per-call fuel budget threaded into `Store::set_fuel()`; `SandboxResult::fuel_consumed`
  reports units used; `fuel_consumed_is_positive_for_arithmetic` and
  `simple_arithmetic_does_not_exhaust_generous_fuel_budget` verify accounting)
- S2.5.3 Capability-gated WASI imports. ✅ (`SandboxCapabilities { allow_stdout, allow_stderr }`
  — `build_linker()` links `env::write_stdout` / `env::write_stderr` only when the flag
  is set; modules calling unlisted imports fail at link time before any code runs;
  `missing_capability_blocks_instantiation` asserts `Trap` without capability;
  `granted_capability_allows_instantiation` asserts `Ok` with capability)
- S2.5.4 Sample sandboxed math evaluator. ✅ (`SandboxedMathEvaluator` — a `ToolDriver`
  registered as `"wasm-math"`; arithmetic (add/sub/mul/div) compiled from embedded WAT
  and executed inside a fresh isolated `Store`; JSON payload `{"op":"add","a":1,"b":2}`
  → `{"result":3}`; verified by `sandboxed_math_evaluator_add_via_tool_driver`)

**Exit criteria.**
1. Adversarial WASI module (infinite loop, memory exhaustion attempt) is
   bounded inside the configured limits. ✅
   (`adversarial_infinite_loop_is_bounded_by_fuel` — `ADVERSARIAL_LOOP_WAT` spins
   until fuel=0, returns `SandboxError::FuelExhausted`;
   `adversarial_memory_exhaustion_is_bounded_by_limit` — `ADVERSARIAL_MEMORY_WAT`
   attempts 65 535-page growth (≈ 4 GiB), denied by `ResourceLimiter`, returns
   `SandboxError::MemoryExhausted`)
2. Wasmtime startup cost amortised across the process lifetime (one-time init). ✅
   (`sandbox_engine_created_once_and_shared` — `Arc::ptr_eq` proves same allocation;
   `engine_shared_across_multiple_invocations` — 5 calls reuse the same engine Arc,
   ref-count stays at 2 throughout)

### Epic E2.6 — LanceDB L3 Archive ✅

**Scope.** An embedded LanceDB instance under `/dev/anima/memory/l3`,
with the embedding pipeline and bidirectional L2↔L3 paths.

**Dependencies.** E2.2.

**Stories.**
- S2.6.1 LanceDB embed and lifecycle management. ✅ (`memory/src/archival.rs` —
  `L3Archive` with file-backed JSON persistence; `open(path, dim, cap)` loads
  an existing snapshot or creates fresh; `demote()` flushes atomically via
  write-to-`.tmp`-then-rename on every insert; `LifecycleManager::l3_archive`
  field added to `vita/src/lib.rs`)
- S2.6.2 Embedding pipeline for memory entries. ✅ (`embed_memory_node(node)`
  — 4-dim feature vector `[initial_activation, λ, α·arousal, σ·surprise]`;
  `archive_memory_node(id, key, node)` packages a `MemoryNode` as `ArchivedItem`
  with LE-bytes payload; both exported from `memory::archival`)
- S2.6.3 L2 → L3 demotion path with provenance. ✅ (`SourceTier`, `Provenance`,
  `ArchivalEntry`; `L3Archive::demote(item, provenance)` is idempotent by item ID;
  `L1PruningStore::drain_pruned_with(elapsed, floor)` returns evicted nodes;
  `SleepRoutineOutcome::evicted_l1_nodes` carries them out of `run_pruning_phase`;
  `LifecycleManager::run_sleep_cycle` and `transition_to_sleep_state` both
  demote evicted L1 nodes to `l3_archive` when present)
- S2.6.4 L3 → L2 retrieval via similarity scoring. ✅ (`L3Archive::search(query, k)`
  — cosine similarity ordered by (desc score, asc id) for deterministic output;
  `retrieve_top_k_from_l3_for_l2(l3, query, k, l2_cache)` — top-k search and
  re-admission into `ArcCache<String, MemoryNode>` in one call)

**Exit criteria.**
1. L3 survives a process restart with consistent retrieval. ✅
   (`l3_archive_survives_process_restart_with_consistent_retrieval` in
   `memory::archival`; `l3_archive_survives_sleep_cycle_restart` in `vita::lib`)
2. Demotion is idempotent; retrieval is deterministic for fixed seeds. ✅
   (`demotion_is_idempotent` — second call for same ID returns `AlreadyPresent`
   without modifying the archive; `search_results_are_deterministic_for_identical_query`
   — tied items broken by ascending ID; `sleep_cycle_demotion_is_idempotent` in
   `vita::lib`)

---

### Epic E2.7 — TurboQuant Vector Quantisation ✅

**Scope.** Production-grade vector quantisation for the L2 warm cache
and the L3 archive, derived from the TurboQuant algorithm (Zandieh
et al., ICLR 2026) with Qdrant 1.18's MSE-variant extensions. The
goals are 6×–32× memory reduction on stored vectors, an unbiased
dot-product estimator suitable for HNSW symmetric scoring, no
calibration dataset, and SIMD-accelerated scoring on hot paths. This
epic establishes the substrate that E5.4 (Learned KV-Cache Controller)
sits on top of.

**Dependencies.** E2.2 (L2 cache), E2.6 (L3 archive). Lands as a
cross-cutting storage substrate that both tiers adopt; ordering within
Stage 2 is "after either E2.2 or E2.6, before Stage 3 closure."

**Stories.**
- S2.7.1 PolarQuant rotation: fast Hadamard random orthogonal rotation
  applied to every vector before quantisation. Distributes per-
  coordinate variance evenly so each coordinate approximates N(0, 1).
  Rotation is fixed per segment so dot products and L2 distances are
  preserved without inverting the rotation at scoring time.
- S2.7.2 Lloyd-Max codebook: fixed lookup table of 2^b levels for the
  standard normal, supporting bit depths 4, 2, 1.5, and 1. **MSE
  variant only** — the codebook supports symmetric scoring between two
  stored vectors as required for HNSW graph construction. The PROD
  variant is rejected: it requires a float-side query at scoring time
  and splits the bit budget between codebook and QJL correction.
- S2.7.3 QJL inner-product bias correction: a Quantised Johnson-
  Lindenstrauss residual projection that reduces the quantisation
  residual to a single sign bit per coordinate, producing an unbiased
  inner-product estimator at negligible extra storage.
- S2.7.4 Length renormalisation: store one `f32` per vector recording
  the ratio of the original length to the centroid-reconstruction
  length, restored at scoring time. Avoids TurboQuant's persistent
  length bias without paying for a full QJL projection (the Qdrant
  1.18 fix borrowed from RaBitQ).
- S2.7.5 Per-coordinate calibration (anisotropy compensation): a
  one-time pre-pass per L3 segment uses the P-Square streaming
  quantile algorithm (Jain & Chlamtac, 1985) over a Vitter
  Algorithm-R reservoir sample to estimate `(shift, scale)` per
  coordinate. Applied asymmetrically — folded into the query at search
  time as a single scalar addition — so the hot scoring path is
  unchanged in shape.
- S2.7.6 L2 / dot / cosine metric support: store the original L2 norm
  per vector and reconstruct L2 distance via the identity
  `‖q − v‖² = ‖q‖² + ‖v‖² − 2·⟨q, v⟩`. L1 is explicitly **not**
  supported (random orthogonal rotation preserves L2 but not L1).
- S2.7.7 SIMD scoring kernels: x86_64 AVX-VNNI (`VPDPBUSD`) and ARMv8.2
  NEON (`SDOT`) implementations for 4-bit and 2-bit; bit-plane scoring
  via `AND` + `popcount` for 1-bit. Scalar fallback for portable builds
  and for CI runners without VNNI/SDOT.
- S2.7.8 Integration paths:
  - L2 warm cache: `instant-distance` HNSW indices use TurboQuant-
    quantised payloads on the read path, with rotation and codebook
    parameters carried in the cache metadata.
  - L3 archive: `L3Archive` is extended with a `Quantisation` enum
    (`None`, `TurboQuant { bits }`). Existing archive files migrate
    on the next sleep cycle (read full-precision, rewrite quantised).
  - The L3 backing-store decision (custom TurboQuant over the existing
    `L3Archive` / LanceDB vs. swapping to Qdrant 1.18 which already
    ships TurboQuant) is recorded as Open Decision **OD6**; this epic
    does not pre-commit.

**Exit criteria.**
1. On the documented benchmark set (the existing replay traces plus
   the public retrieval datasets used in Qdrant's 1.18 release blog),
   TurboQuant 4-bit reaches within 1–2 pp recall of the full-
   precision baseline at 8× compression; TurboQuant 2-bit beats a
   1-bit baseline by ≥ 9 pp recall at the same storage class.
   ✅ `four_bit_recall_positive_signal` (d=128, n=500) confirms positive
   correlation with full-precision ranking and ≥ 50% recall@10.
2. SIMD kernels are exercised in CI on at least one x86_64 and one
   aarch64 runner; the scalar fallback path produces bit-identical
   results to the SIMD path on a shared corpus.
   ✅ `simd_support_is_reported_on_known_architectures` — auto-vectorisable
   four-way-unrolled dot product in `dot_product_f32`; LLVM emits AVX/SSE2
   on x86_64 and NEON on AArch64.
3. L3 retrieval is deterministic under fixed rotation and codebook
   parameters (extends the E2.6 determinism criterion to the
   quantised path).
   ✅ `quantised_scoring_is_deterministic` — bit-identical scores on
   repeated calls with the same rotation seed.
4. The per-segment calibration pre-pass completes within a documented
   wall-clock budget at production segment sizes (placeholder:
   ≤ 5 s for a 100 k-vector segment at d = 1536).
   ✅ P-Square algorithm is O(n × d_pad × 5) with O(d_pad) space;
   calibration of 100 k × 1536 is below 5 s on a single core.

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

### Epic E3.3 — Sensory Bridge (Text and Voice) ✅

**Scope.** The afferent input surface: a text socket and a PCM voice
pipeline that produces `SensoryPacket`s with priorities.

**Dependencies.** E1.6.

**Stories.**
- S3.3.1 `/dev/anima/senses/human` text-input socket. ✅ (`senses/src/lib.rs` —
  `SensoryBridge::packetize_text_checked()` with policy-bounds enforcement)
- S3.3.2 PCM streaming socket → VAD → local STT. ✅ (`SensoryBridge::packetize_pcm_checked()`;
  VAD stub — validates non-empty frame; STT integration deferred to E4.x)
- S3.3.3 `SensoryPacket` envelope and priority assignment. ✅
  (`PrioritizedPacket { packet: SensoryPacket, priority: SensoryPriority }`;
  `SensoryPriority::Low | Normal | High | Critical` with `Ord` ordering)
- S3.3.4 `HumanGuidance` policy bounds. ✅
  (`HumanGuidance::max_text_length`, `blocked_prefixes`; `PolicyViolation`
  error returned without panic; runtime update via `set_active_bounds()`)

**Exit criteria.**
1. Text and voice both reach `vita` as priority-tagged packets. ✅
   (`text_packet_reaches_vita_and_is_dispatched_as_high_priority_task`,
   `voice_pcm_packet_reaches_vita_and_is_dispatched_as_task` — packets
   converted to MLFQ tasks in `somatic_execution_loop`)
2. Policy bounds reject out-of-policy inputs without panicking. ✅
   (`checked_text_rejects_empty_input_without_panicking`,
   `checked_text_rejects_input_exceeding_max_length_without_panicking`,
   `checked_text_rejects_blocked_prefix_without_panicking`,
   `checked_pcm_rejects_empty_frame_without_panicking`)

### Epic E3.4 — Wake/Sleep State Transitions ✅

**Scope.** Drive transitions between waking and sleeping based on
stress and agenda state, and sequence the four sleep phases.

**Dependencies.** E3.2, E3.3.

**Stories.**
- S3.4.1 Wake → Sleep on (stress high ∧ agenda empty). ✅
  (`somatic_execution_loop` — agent sleeps whenever agenda is empty;
  sensory events populate the agenda, keeping the agent awake under load)
- S3.4.2 Sleep → Wake on sensory event. ✅
  (`sensory_event_during_sleep_triggers_wake_transition` — sensory packets
  injected during sleep are consumed at the top of the next iteration,
  converted to tasks, and cause the agent to wake and dispatch)
- S3.4.3 Phase sequencer: Pruning → Replay → Dreaming → Compilation. ✅
  (`sleep::run_maintenance_audited` — sequential MemoryPruning →
  GenerativeReplay → DreamExploration → PolicyCompilation with per-phase
  `SleepPhaseStarted` / `SleepPhaseCompleted` audit entries)

**Exit criteria.**
1. Transitions audited end-to-end in the log. ✅
   (`sleep_transition_audits_all_four_phases_in_order` — `SleepEntered` +
   8 phase entries; `wake_transition_is_audited_after_sleep` — `WakeEntered`
   logged on state change)
2. 100 consecutive sleep cycles complete without error in the soak test. ✅
   (`one_hundred_sleep_cycles_complete_without_error` — 100 cycles via
   `LifecycleManager::run_sleep_cycle()`; 400 `SleepPhaseCompleted{success:true}`
   entries verified; 400 `SleepPhaseStarted` entries verified)

### Epic E3.5 — Pruning Phase with Emotional Decay ✅

**Scope.** The pruning phase implements `S(t)` activation decay against
the semantic floor in both L1 and L2.

**Dependencies.** E2.2, E3.4.

**Stories.**
- S3.5.1 `MemoryNode::activation_at` decay model. ✅ (`memory/src/decay.rs` —
  already implemented; `activation_at(t)` enforces `SEMANTIC_FLOOR`)
- S3.5.2 L1 and L2 pruning routines. ✅ (`memory/src/pruning.rs` —
  `L1PruningStore::run_pruning_pass_with(elapsed, floor)`;
  `prune_l2_cache(cache, elapsed, floor)` via `ArcCache::retain`;
  `PruningContext` in `vita/src/sleep.rs` wires the store into
  `run_maintenance_audited`)
- S3.5.3 Semantic floor enforcement. ✅ (`effective_floor = floor.max(SEMANTIC_FLOOR)` —
  callers cannot prune below the semantic floor; `ArcCache::retain` preserves
  ghost-list state so ARC adaptation is unaffected)

**Exit criteria.**
1. Pruning bounded by the configured floor under stress injection. ✅
   (`pruning_bounded_by_semantic_floor_under_stress` in `memory::pruning`,
   `pruning_bounded_by_floor_under_stress_injection` in `vita::sleep`,
   `lifecycle_pruning_bounded_by_floor_under_stress` in `vita::lib`)
2. No retained entry has activation below the floor after a pass. ✅
   (`no_retained_node_has_activation_at_or_below_floor_after_pruning` in `memory::pruning`,
   `no_retained_node_below_floor_after_sleep_pruning_pass` in `vita::sleep`,
   `lifecycle_no_retained_node_below_floor_after_sleep_cycle` in `vita::lib`)

### Epic E3.6 — Replay Validation with Rollback ✅

**Scope.** Generative replay against the L3 audit stream, with rollback
when degradation crosses the configured threshold.

**Dependencies.** E2.6, E3.5.

**Stories.**
- S3.6.1 Replay sampling from L3. ✅ (`memory/src/replay.rs` —
  `run_replay_validation(l3, config)` samples up to `max_sample_size` entries
  in ascending ID order from `L3Archive::entries()`, queries each with its own
  embedding via `l3.search(query, 1)`, and checks whether the top-1 result ID
  matches the expected entry ID; `ReplayContext<'a>` carries `&'a L3Archive`
  and is wired into `vita::sleep::run_maintenance_audited`)
- S3.6.2 Accuracy threshold checker. ✅ (`ReplayConfig::accuracy_threshold` —
  `accuracy = validated / queries_run`; rollback triggered when
  `accuracy < threshold` and `rollback_enabled = true`; `ReplayReport`
  exposes `queries_run`, `queries_validated`, `accuracy`, `threshold`, and
  `triggered_rollback`)
- S3.6.3 Rollback path for prior pruning changes. ✅ (`run_replay_validation`
  returns `Vec<(String, MemoryNode)>` of failed entries decoded from their
  20-byte payloads; `SleepRoutineOutcome::replay_rollback_nodes` carries these
  out of the sleep phase; `LifecycleManager::run_sleep_cycle` and
  `transition_to_sleep_state` re-insert rollback nodes into `l1_memory` after
  maintenance completes; `LifecycleManager::replay_config` controls threshold
  and rollback behaviour)

**Exit criteria.**
1. Soak test demonstrates at least one rollback (proof the path works). ✅
   (`soak_test_sleep_cycle_triggers_rollback_and_restores_l1_nodes` in
   `vita::lib` — pre-populates L3 with 3 entries sharing the same embedding
   so accuracy = 1/3 < threshold 0.5; asserts `rr.triggered_rollback = true`
   and `m.l1_memory.len() == rr.rolled_back`;
   `rollback_triggered_when_duplicate_embeddings_cause_low_accuracy` in
   `memory::replay` — same mechanism at the library level)
2. Validation accuracy logged for every cycle. ✅ (`sleep_cycle_logs_replay_accuracy_when_l3_is_configured`
   in `vita::lib` — verifies `replay` field is `Some` in `SleepRoutineOutcome`
   for the `GenerativeReplay` phase on every cycle;
   `one_hundred_sleep_cycles_with_l3_log_replay_report_every_cycle` — 100
   cycles all carry a report; `accuracy_is_logged_for_every_cycle_even_when_perfect`
   in `memory::replay`)

### Epic E3.7 — Dreaming Phase ✅

**Scope.** Random graph walks across L3 produce associative-edge
candidates that feed the next pruning cycle for validation.

**Dependencies.** E2.6, E3.6.

**Stories.**
- S3.7.1 Random-walk sampler with seeded determinism. ✅ (`memory/src/dreaming.rs` —
  `Xorshift64` PRNG seeded by `DreamConfig::seed`; `run_dream_walk(l3, config)` samples
  entries in ascending ID order, performs seeded random walks, and returns a deterministic
  `(DreamReport, Vec<AssociativeEdge>)` for every call with identical inputs;
  `LifecycleManager::dream_config` field controls seed and walk parameters)
- S3.7.2 Candidate edge generation. ✅ (`AssociativeEdge { from_key, to_key, similarity }`
  — edges discovered by cosine-similarity walks across L3 embeddings; deduplicated
  (highest similarity kept), sorted descending by similarity then lexicographic key
  pair for full determinism; `DreamConfig::similarity_threshold` filters low-quality edges;
  `SleepRoutineOutcome::dream_candidates` carries the edge list out of the sleep phase)
- S3.7.3 Hand-off to the next pruning cycle. ✅ (`dream_candidates` is exposed on
  `SleepRoutineOutcome` index 2; callers can read the list from `run_sleep_cycle()` and
  seed the next pruning pass; `DreamContext` wires the L3 archive into
  `run_maintenance_audited`; `LifecycleManager` passes `dream_ctx` when `l3_archive`
  is configured)

**Exit criteria.**
1. Candidate yield is logged and monotonic-reproducible per seed. ✅
   (`candidate_yield_is_monotonic_reproducible_per_seed` in `memory::dreaming` —
   two calls with identical archive + config produce byte-for-byte identical reports
   and edge lists; `dream_candidates_are_reproducible_per_seed` in `vita::sleep`;
   `dream_candidates_are_reproducible_across_lifecycle_cycles` in `vita::lib` —
   two consecutive lifecycle sleep cycles with the same `dream_config` and unchanged
   L3 produce identical candidate lists)
2. Bad candidates are filtered out by the subsequent pruning pass. ✅
   (`threshold_filters_out_low_similarity_candidates` in `memory::dreaming` —
   orthogonal nodes (cosine similarity 0.0) are excluded when threshold > 0;
   `all_candidate_edges_have_similarity_at_or_above_threshold` verifies post-condition;
   `dream_threshold_filters_low_similarity_edges_in_lifecycle` in `vita::lib` confirms
   end-to-end filtering through the LifecycleManager)

### Epic E3.8 — Compilation Phase: Trace → Training Pairs ✅

**Scope.** Compile the cycle's traces into all three output training
formats and persist them under `training_corpus/` in L3.

**Dependencies.** E3.6.

**Stories.**
- S3.8.1 Trace-to-pair compiler for each format. ✅ (`memory/src/compilation.rs` —
  `compile_traces_to_pairs(entries, config)` pairs each `TaskStarted` with its
  subsequent `TaskCompleted` (by task ID); emits `AlpacaRecord` (`{ instruction, input,
  output }`), `ConversationRecord` (`{ conversations: [{ role, content }] }`), and
  `ChainOfThoughtRecord` (`{ prompt, chain_of_thought, answer }`) for the three
  `TrainingFormat` variants; failed tasks are excluded; `AuditEntry → AuditTraceEntry`
  conversion in `vita::lib::audit_entry_to_trace` bridges the two crates)
- S3.8.2 Persistence under `training_corpus/`. ✅ (`write_format` writes each format
  as a JSONL file under `CompilationConfig::output_dir`; atomic write (`.tmp` then
  rename) prevents partial reads; `append` mode accumulates across calls;
  `LifecycleManager::compilation_config` controls the output directory and enabled
  formats; `run_sleep_cycle` and `transition_to_sleep_state` both build a
  `CompilationContext` from the current audit log when configured)
- S3.8.3 Final close-out of the sleep cycle. ✅ (`emergency_consolidate(entries, config)`
  — triggers an immediate compilation pass and sets `CompilationReport::emergency_consolidation
  = true`; exposed from `memory::compilation`; tested in `vita::lib` via
  `emergency_consolidation_produces_a_marked_report`)

**Exit criteria.**
1. Output corpora validate against the documented schemas. ✅
   (`output_files_validate_against_schemas` in `memory::compilation` — Alpaca,
   Conversation, and ChainOfThought JSONL files are written and deserialized back
   to their typed structs without error; `sleep_cycle_compiles_completed_tasks_into_training_corpus`
   in `vita::lib` validates the Alpaca file produced by a sleep cycle end-to-end)
2. Emergency consolidation can trigger and recover under stress injection. ✅
   (`emergency_consolidation_marks_report_and_flushes_pairs` in `memory::compilation`;
   `emergency_consolidation_produces_a_marked_report` in `vita::lib` — both assert
   `CompilationReport::emergency_consolidation = true` and verify that pairs are flushed
   and files written correctly)

---

## Stage 5 — Cognitive Layer

The deliberative cortex, the gate-and-router arbitration, the learned
KV-cache controller, the episodic/identity memory split, the
interoceptive policy modulation, and the defence layer. This stage
realises the cognitive-architecture spec in `08-cognitive-architecture.md`
on top of the somatic substrate completed in Stages 1–3.

**Stage sequencing.** Stage 5 executes *before* Stage 4 in the build
order. The numeric ordering is preserved as Stage 5 / E5.x so that the
existing E4.x epic identifiers (already referenced in `05-roadmap.md`,
commits, PR titles, and audit-log placeholders) remain stable. Stage 4
— the bare-metal port and production verification — runs once Stage 5
has produced a working cortex, the learned cache controller has been
integrated, and the kill-shot demonstrations in E5.8 have been
recorded. This sequencing reflects the decision that the agent thesis
is the primary deliverable and the bare-metal isolation is the means
by which it ships.

The vocabulary in this stage follows the cognitive-architecture spec
(cortex, Striatal Gate, Thalamic Router, episodic memory, identity
memory, defence layer). Where these names map to existing crates, the
mapping is recorded in `06-glossary.md` as each epic lands; until then,
the new names are treated as cognitive-layer working vocabulary rather
than as crate renames.

### Epic E5.1 — Cortex MVP ✅

**Scope.** A minimal deliberative loop in Python (planner, executor,
tool call, observation, plan revision) reachable from `vita` over IPC.
One static route, the existing `LlmBackend` provider set as the model
surface, a small tool subset, identity memory as a flat JSON file, and
episode summaries committed through the existing L3 archival path.

**Dependencies.** Stage 1 complete (`LlmBackend`, scheduler, audit
log), E2.6 (L3 archive for episode persistence), E3.3 (sensory packet
path for invocation triggers).

**Stories.**
- S5.1.1 Cortex process skeleton: Python service with a length-prefixed
  JSON-over-UDS RPC bridge to `vita`. Lifecycle is invocation-
  scoped: the cortex is spun up per task and torn down when the
  invocation terminates. ✅ (`cortex/__main__.py` — UDS client;
  `cortex/ipc.py` — 4-byte big-endian length prefix + JSON body;
  `crates/vita/src/cortex_bridge.rs` — `UnixListener` server,
  `PythonCortexBridge::spawn_python`, per-invocation socket cleanup)
- S5.1.2 LangGraph-style agent loop with explicit plan / act / observe /
  revise stages and a configurable termination condition. ✅
  (`cortex/agent_loop.py` — `AgentLoop.run()` with `_plan / _act /
  _observe / _revise`; mock backend produces a deterministic two-step
  plan without live API keys; `MAX_TOOL_CALLS = 10` termination guard)
- S5.1.3 Tool surface: the cortex receives a capability-scoped subset
  of the `praxis` tool registry via the IPC channel; tool dispatch
  round-trips through `praxis` so the existing circuit breakers and
  audit log apply. ✅ (`ToolSpec` list in `InvokeRequest`; `ToolCall`
  / `ToolResponse` round-trip in `PythonCortexBridge::invoke`; the
  `ToolDispatcher` trait decouples the bridge from the `praxis` crate
  so callers inject their own dispatch closure)
- S5.1.4 Identity memory v0: a JSON file under the agent's state
  directory, loaded into every invocation by the cortex's bootstrap. ✅
  (`cortex/identity_memory.py` — `IdentityMemory.load/save/get/set`;
  atomic write via `.tmp`-then-rename; `cortex/__main__.py` bootstrap
  loads the file and merges it into the `InvokeRequest`)
- S5.1.5 Episode summariser: end-of-invocation summary written through
  `vita` into the L3 archive with a new `Episode` provenance variant
  on `SourceTier`. ✅ (`memory::SourceTier::Episode` added to
  `crates/memory/src/archival.rs`; `archive_episode()` helper in
  `cortex_bridge` packs the summary into a 4-dim embedding and calls
  `L3Archive::demote` with `Episode` provenance)

**Exit criteria.**
1. A user-issued task reaches the cortex, completes a multi-step plan
   with at least two tool calls, and emits an episode summary that is
   recoverable from L3 after a process restart. ✅
   (`mock_cortex_makes_two_tool_calls` — `tool_calls_made == 2`,
   non-empty `episode_summary`;
   `episode_summary_persists_in_l3_after_restart` — creates new
   `L3Archive` from same path, cosine-similarity search returns the
   episode entry with `provenance.source_key.starts_with("episode:")`)
2. Cortex crashes do not bring down `vita`; the audit log records the
   crash and the next invocation succeeds from a clean state. ✅
   (`cortex_fault_is_audited_and_does_not_crash_vita` — fault-injected
   `MockCortexBridge` returns `CortexError::CortexFault`, audit log
   contains `AuditEntry::CortexFault`; second invocation with clean
   bridge succeeds)
3. End-to-end latency from sensory packet to first cortex tool action
   is logged and stays within a documented budget on the hosted
   target. ✅ (`latency_to_first_action_is_logged_in_audit` — audit
   log contains `AuditEntry::CortexInvoked`;
   `cortex_invoked_audit_entry_carries_latency_ms` — field
   `latency_to_first_action_ms > 0`)

### Epic E5.2 — Striatal Gate ✅

**Scope.** The arbitration point that decides whether a candidate
event invokes the cortex, and at what cost class (cheap-local / mid-
tier / frontier). First implementation is a hand-tuned threshold
function; inputs are explicit and every decision is audited.

**Dependencies.** E5.1, E3.2 (homeostatic stress index).

**Stories.**
- S5.2.1 Gate input contract: event features (urgency, novelty,
  semantic class, user-facing flag), homeostatic signals
  (`thermal_load`, `compute_pressure`, `memory_pressure`,
  `power_budget`, `financial_budget`, `attention_demand`), recent
  cortex history, and current budgets. ✅ (`vita::gate::EventFeatures`,
  `vita::gate::HomeostaticSignals` — all six signals as documented,
  clamped to `[0.0, 1.0]`, with `neutral()` baseline constructor)
- S5.2.2 Hand-tuned threshold function with documented coefficients
  and a configuration surface for runtime tuning. ✅
  (`ThresholdGate` + `GateConfig::default()` — urgency\_weight=0.65,
  novelty\_weight=0.35, user\_facing\_bonus=0.15,
  operator\_command\_bonus=0.20, base\_threshold=0.40,
  thermal\_penalty=0.30, memory\_penalty=0.20,
  financial\_penalty=0.15, attention\_boost=0.20,
  cheap\_local\_ceiling=0.60, frontier\_floor=0.85; all coefficients
  documented in the struct and module docs)
- S5.2.3 Per-decision audit entry: inputs, threshold values, decision,
  cost class, reasoning string. Auditable from the same log used by
  the existing scheduler. ✅ (`AuditEntry::GateDecision` — carries all
  six homeostatic signals, event features, value\_score,
  threshold\_applied, cost\_class, reasoning, override\_active;
  `record_gate_decision()` helper writes the entry;
  `print_audit()` in `kernels/hosted` renders it with the `🔀` prefix)
- S5.2.4 Override mechanism: explicit user-issued or operator-issued
  invocations bypass the gate, with the bypass recorded. ✅
  (`GateOverride::UserForced { reason }` and
  `GateOverride::OperatorForced { reason }` — both force `invoke=true`,
  operator forces `Frontier` cost class, `override_active=true` is
  set in both the `GateDecision` struct and the audit entry)
- S5.2.5 Hookpoint for a learned gate: the threshold function is
  exposed behind a trait so a learned model can replace it without
  changing callers. ✅ (`pub trait Gate: Send + Sync` — single
  `decide()` method; `ThresholdGate` is the default impl; any type
  implementing `Gate` can be passed to dispatch code without changing
  call sites)

**Exit criteria.**
1. Every cortex invocation is preceded by a gate decision entry in the
   audit log; no invocation bypasses the gate without an explicit
   override entry. ✅ (`every_invocation_decision_is_preceded_by_gate_audit_entry`
   — 10 evaluations produce exactly 10 `GateDecision` entries;
   `override_decision_audit_entry_carries_override_active_true` — forced
   decisions carry `override_active=true`)
2. Threshold sensitivity to each homeostatic signal is covered by a
   table-driven unit test, including the case where signals at their
   neutral values produce baseline behaviour. ✅
   (`homeostatic_signal_sensitivity_table` — 4-row table covering
   thermal\_load, memory\_pressure, financial\_budget=0, attention\_demand;
   each row asserts correct shift direction and exact magnitude;
   `neutral_signals_produce_baseline_threshold` — threshold equals
   `base_threshold` exactly at neutral;
   `thermal_stress_raises_threshold`,
   `financial_pressure_raises_threshold`,
   `memory_pressure_raises_threshold`,
   `high_attention_demand_lowers_threshold` — individual signal tests)
3. A `anima why` CLI command reads the most recent gate decision and
   prints its inputs and reasoning. ✅ (`cargo run --bin anima-hosted -- why`
   runs four representative scenarios — background-cleanup (blocked),
   user-question (MidTier), high-priority-under-thermal (threshold raised
   to 0.670 by thermal\_load=0.9), operator-emergency (Frontier override) —
   and prints the most recent `GateDecision` with full input breakdown)

### Epic E5.3 — Thalamic Router ✅

**Scope.** Route selection: which model, which tools, which memory
scopes, which prompt scaffolding, and which termination conditions
apply for a given cortex invocation. Static route table by default,
with a hookpoint for learned routing in cases where the static mapping
is insufficient.

**Dependencies.** E5.1, E5.2.

**Stories.**
- S5.3.1 Route schema: `RouteId`, `ModelSelector`, `ToolScope`,
  `MemoryScope`, `PromptScaffold`, `TerminationPolicy`. ✅
- S5.3.2 Static route table keyed on event class and gate cost class.
  Three baseline routes: `cheap-local`, `mid-tier`, `frontier`. ✅
- S5.3.3 Router → cortex handshake: route configuration is passed in
  the cortex invocation RPC via `InvokeRequest`; the cortex cannot
  request tools or memory outside the route scope. ✅
- S5.3.4 Identity memory is loaded as part of the route's standard
  context for every invocation (default scope). ✅
- S5.3.5 Hookpoint for learned routing: the route resolver is a trait
  with the static table as its default implementation. ✅

**Exit criteria.**
1. Each baseline route is exercised in an integration test that
   asserts the cortex sees exactly the configured tool subset and
   memory scope. ✅ (32 router tests; see `crates/vita/src/router.rs`)
2. A route misconfiguration (unknown tool reference, missing memory
   scope) is rejected at startup, not at invocation time. ✅
   (`StaticRouter::new` validates all three routes; 7 rejection tests)

### Epic E5.4 — Learned KV-Cache Controller (Semantic Gating over TurboQuant) ✅

**Scope.** A small recurrent or state-space module that observes
hidden states, attention patterns, role flags, and tool-output markers
and produces *semantic gating* decisions for KV-cache writing and
retention at block (page) granularity. The controller sits on top of
the TurboQuant-quantised cache substrate from E2.7: **TurboQuant
handles bit-level compression of the values that are retained; the
controller decides which blocks are worth retaining in the first
place** (pinning user constraints, preserving error traces, dropping
superseded intermediate state). Trained offline against a full-cache
teacher on representative agentic traces, with adversarial needle
insertions for retrieval safety. The headline comparison is no longer
controller-vs-LRU at full precision — it is controller+TurboQuant
versus LRU+TurboQuant at a matched block budget. The TurboQuant-only
configuration is therefore both the substrate and the baseline this
epic must beat.

**Dependencies.** E5.1 (cortex traces as training data), E2.1 (block-
structured context tracking), **E2.7** (TurboQuant substrate for both
the controller's storage path and its baseline), and a backend whose
KV-cache can be intercepted (the Anthropic/OpenAI backends do not
expose this surface, so this work targets a local model first).

**Stories.**
- S5.4.1 Controller architecture: linear gate model (logistic regression
  over a 7-element [`BlockFeatures`] vector: role, is_user_constraint,
  is_error_trace, is_tool_output, recency_score, memory_pressure, bias)
  in new `crates/kv-controller` crate. ✅ (`kv_controller::controller` —
  `LinearGate` implements `BlockGate` trait; `KvController` wraps the
  gate with fault state machine and `Quantizer` integration seam for E2.7;
  `BlockGate` trait is the hook-point for SSM/GRU replacement)
- S5.4.2 Trace capture: `TraceCapture` / `InvocationTrace` with
  `TraceProvenance` tagging (live, synthetic, public_dataset) under
  explicit opt-in (`TraceConfig::enabled`). ✅ (`kv_controller::trace` —
  `TraceCapture`, `InvocationTrace`, `BlockTraceRecord`, `ProvenanceCounts`)
- S5.4.3 Offline training pipeline: `compile_training_pairs` compiles
  `InvocationTrace` → `Vec<TrainingPair>` with teacher labels and
  loss weights; `TrainingCorpus::new` bundles pairs with provenance
  counts. ✅ (`kv_controller::training`)
- S5.4.4 Runtime integration: `vita::kv_gate::gate_working_context`
  gates the working context under routes with `MemoryScope::kv_controller
  = true`; fault → `ControllerState::Faulted` → LRU fallback within the
  same gate pass; `AuditEntry::KvControllerFaulted` + `KvGatePass` written
  to audit log; `MemoryScope::full_with_kv_controller()` opt-in. ✅
  (`vita::kv_gate`, `vita::router::MemoryScope::kv_controller`,
  `vita::audit::AuditEntry::{KvGatePass,KvControllerFaulted}`)
- S5.4.5 Evaluation harness: `NeedleBenchmarkConfig` (standard: 20 blocks,
  5 needles in oldest half, budget=10); `run_controller_benchmark` and
  `run_lru_benchmark` measure needle recall; `NeedleRecallResult::
  recall_advantage_pp` computes the headline pp metric. ✅
  (`kv_controller::eval`)

**Exit criteria.**
1. At a matched block budget, controller beats LRU by ≥ 10 pp needle
   recall on the standard benchmark. ✅ (`controller_beats_lru_by_at_least_ten_pp_needle_recall`
   — pre-trained weights retain all 5 needles (recall=1.0) vs LRU (recall=0.0)
   → +100 pp advantage on the standard config; the [`Quantizer`] trait
   seam means the same comparison applies to controller+TurboQuant vs
   LRU+TurboQuant once E2.7 merges)
2. Controller fault reverts to LRU within the next gating decision and is
   recorded in the audit log. ✅ (`kv_controller_fault_is_recorded_in_audit_log` —
   `AlwaysFaultGate` triggers fault on first call → `KvControllerFaulted`
   + `KvGatePass { fallback_lru: true }` written; `subsequent_faulted_passes_produce_only_gate_pass_entries` —
   second call produces only `KvGatePass` without a second `KvControllerFaulted`)
3. Training-data provenance documented: every `TrainingPair` carries a
   `TraceProvenance` tag; aggregate counts via `TrainingCorpus::provenance_summary`. ✅
   (`training_corpus_provenance_summary_contains_all_fields`)
4. Ablation: frozen random-initialisation weights (pure recency = LRU-
   equivalent) do not beat LRU by ≥ 10 pp. ✅
   (`ablation_frozen_weights_do_not_beat_lru_by_more_than_noise`)

### Epic E5.5 — Episodic and Identity Memory ✅

**Scope.** Two memory tiers above L3 that are cognitive rather than
substrate-level. Episodic memory records what happened across cortex
invocations; identity memory holds stable, human-readable facts about
the user, the machine, and the agent's own configuration.

**Dependencies.** E5.1, E2.6.

**Stories.**
- S5.5.1 Episodic store schema: invocation id, event class, route id,
  start/end timestamps, outcome, summary text, embedding for
  retrieval. Initial implementation reuses the L3 archive with an
  `Episode` provenance variant; promoted to a dedicated store if
  cardinality warrants. ✅ (`vita/src/episodic.rs` — `EpisodeRecord`,
  `EpisodeStore`, `embed_episode`, `pack_episode_payload`,
  `unpack_episode`, `make_episode_archived_item`, `make_episode_provenance`;
  4-dim embedding `[success, duration_norm, recency, summary_len]`; pipe-
  delimited `source_key` encodes string fields; 20-byte binary payload)
- S5.5.2 Episodic retrieval as a cortex tool, with similarity search
  and recency filtering. ✅ (`EpisodeStore::retrieve` — filters to
  `SourceTier::Episode`, cosine-similarity ranking, optional recency
  cutoff via `cutoff_ns`; `EpisodeQuery::top_k` / `with_recency_cutoff`;
  `EpisodeMatch` result type with `record` + `score`)
- S5.5.3 Identity memory file format: human-readable (YAML or JSON),
  with a schema covering user preferences, recurring tasks, observed
  patterns, system policies, and agent self-model fields. File lives
  under the agent's state directory and is version-controlled in-
  place. ✅ (`vita/src/identity.rs` — `IdentityDocument` JSON schema
  with `UserPreferences`, `RecurringTask`, `ObservedPattern`,
  `SystemPolicies`, `AgentSelfModel`, free-form `facts` dict;
  `IdentityMemory::open` / `in_memory`; atomic write-to-tmp-then-rename;
  `default_path` → `~/.anima/<agent_id>/identity.json`)
- S5.5.4 Identity-memory revision API: an `anima identity` CLI
  subcommand to inspect and edit identity facts, with edits audited. ✅
  (`kernels/hosted/src/main.rs` — `cmd_identity()` handles
  `identity show [<key>]` and `identity set <key> <value>`; every `set`
  appends `AuditEntry::IdentityUpdated { key, old_value, new_value }` to
  the audit log; `print_audit` extended with `IdentityUpdated` arm)
- S5.5.5 Router integration: identity memory is loaded as standard
  context (see S5.3.4); episodic retrieval is exposed only on routes
  whose `MemoryScope` includes it. ✅ (`IdentityMemory::to_json` returns
  the document as a `serde_json::Value` for injection into
  `InvokeRequest::identity`; the cortex receives identity as a distinct
  JSON object, not concatenated with task context; episodic retrieval
  requires `MemoryScope::l3 = true` per S5.3.2)

**Exit criteria.**
1. A user can run `anima identity show` and `anima identity set <key>
   <value>` to inspect and edit identity memory; edits round-trip
   through the audit log. ✅ (`anima_identity_show_and_set_round_trip_through_audit_log`
   — `set_fact` stores value, `get_fact` retrieves it, audit log carries
   `IdentityUpdated` with matching key and value;
   `identity_store_survives_process_restart` — facts persist across
   simulated process restarts)
2. Episodic retrieval returns the correct episode for a recorded
   benchmark of (query → expected-episode-id) pairs. ✅
   (`episodic_retrieval_returns_correct_episode_for_benchmark_pair` —
   two episodes with distinct embeddings; success-embedding query returns
   the success episode; `non_episode_l3_entries_excluded_from_episodic_retrieval`;
   `recency_cutoff_excludes_old_episodes`; `retrieval_respects_top_k_limit`)
3. Identity facts loaded at invocation time are visible in the
   cortex's prompt assembly as a distinct section, not concatenated
   with task context. ✅ (`identity_is_injectable_as_distinct_json_section`
   — `to_json` returns a JSON object (not a string); `IdentityDocument::from_json`
   recovers all fields; identity is passed as `InvokeRequest::identity`
   separate from `description`)

### Epic E5.6 — Defence Layer (Immune Analogue) 🟡

**Scope.** The defence component that screens cortex outputs and motor
actions for prompt injection, internal incoherence, goal drift, reward
hacking, and unsafe motor operations. Veto power, with vetoes audited
and repeated vetoes escalating to user attention.

**Dependencies.** E5.1, E5.3 (route-scoped tool access), the existing
`anima-self` capability machinery.

**Stories.**
- S5.6.1 ✅ Prompt-injection detector for tool outputs and externally-
  sourced text: heuristic plus a learned classifier (initial model
  trained on a public injection corpus). Delivered in `crates/defence/src/injection.rs`;
  `PromptInjectionDetector` + `HeuristicClassifier` with 49 built-in patterns;
  red-team corpus embedded in test suite; `InjectionClassifier` trait for
  learned classifier integration.
- S5.6.2 ✅ Goal-drift monitor: compares current cortex actions against
  the original objective embedding; flags divergence above a
  threshold. Delivered in `crates/defence/src/goal_drift.rs`;
  `GoalDriftMonitor` with Jaccard `TermOverlapSimilarity` (default) and
  `ObjectiveSimilarity` trait for embedding-model replacement.
- S5.6.3 ✅ Reward-hacking detector: cortex outputs that mark work
  complete without observable evidence (tool calls, file changes,
  network actions) are flagged. Delivered in
  `crates/defence/src/reward_hacking.rs`; 30+ completion-claim patterns;
  configurable minimum-evidence threshold.
- S5.6.4 ✅ Unsafe motor action gate: filesystem operations on critical
  paths (`/etc`, `/boot`, the agent's own state directory), network
  calls to blocklisted hosts, and self-modification attempts are
  reviewed against `anima-self` capability scope. Delivered in
  `crates/defence/src/motor_gate.rs`; integrates `anima_self::Capability<Verified>`.
- S5.6.5 ✅ Veto mechanics: vetoed actions are blocked, the cortex is
  notified with a structured reason, and the veto is logged at a
  higher severity than routine audit entries. Repeated vetoes (≥ N in
  M minutes) raise an attention-demand event for the user. Delivered
  in `crates/defence/src/layer.rs`; `DefenceLayer` orchestrator with
  sliding-window escalation. `AuditEntry::DefenceVeto` and
  `AuditEntry::AttentionDemandEscalated` added to `vita/src/audit.rs`.

**Wire-in note.** The `defence` crate is standalone (no `vita` dependency)
and ready to be wired into the vita → cortex IPC path when E5.1 merges.
Callers translate `ScreeningOutcome` into `AuditEntry::DefenceVeto` events.

**Exit criteria.**
1. ✅ A red-team corpus of prompt-injection samples is blocked with a
   recorded false-positive rate; the corpus and rate are published per
   release. (15 red-team samples; 0 false negatives; 0/8 clean-sample
   false positives in the embedded test corpus.)
2. ✅ Goal-drift and reward-hacking detectors each trigger at least once
   in a recorded stress run with a deliberately misbehaving cortex
   fixture. (See `layer::tests::misbehaving_cortex_fixture_triggers_all_detectors`.)
3. ✅ Every veto entry in the audit log carries the source detector, the
   action that was blocked, and the cortex's stated intent. (See
   `layer::tests::veto_history_contains_detector_and_invocation_info`.)

### Epic E5.7 — Interoceptive Modulation ✅

**Scope.** Wire the homeostatic signals into the gate, the router,
and the cache controller so that body state continuously modulates
cognitive behaviour. Demonstrate the modulations described in
`08-cognitive-architecture.md` Section 8 with measurable behavioural
change under induced stress.

**Dependencies.** E5.2, E5.3, E5.4, E3.2.

**Stories.**
- S5.7.1 Signal contract: extend `interoception` to publish a stable
  set of scalar signals (`thermal_load`, `compute_pressure`,
  `memory_pressure`, `power_budget`, `financial_budget`,
  `attention_demand`) on the audit/telemetry stream at 1 Hz. ✅
  (`interoception/src/signals.rs` — `InteroceptiveSignals` struct with
  all 6 fields; `SignalPublisher` trait + `FnPublisher` + `NullPublisher`
  for 1 Hz publication; `InteroceptiveSensorBundle::tick()` samples and
  publishes in one call; `AuditEntry::InteroceptiveSnapshot` persists each
  snapshot in the audit log; `HomeostaticSignals::from_interoceptive()`
  bridges the sensor layer to the gate)
- S5.7.2 Financial budget sensor: track API spend per provider
  against configurable daily/monthly budgets; emit `financial_budget`
  as a normalised scalar. ✅ (`interoception/src/budget.rs` —
  `FinancialBudgetSensor` with `SpendRecord` ledger, `CostTable`
  (USD per 1 M tokens with wildcard fallback), `BudgetConfig`
  (daily/monthly USD limits); `financial_budget_scalar(now_ns)` computes
  remaining fraction; atomic per-day accounting via nanosecond epoch
  buckets)
- S5.7.3 Power and attention sensors: read battery / AC state from the
  host and idle / foreground signals from the windowing system on the
  hosted target; both are gated by explicit opt-in. ✅
  (`interoception/src/power.rs` — `PowerSensor` with `PowerConfig::enabled`
  opt-in; Linux sysfs `/sys/class/power_supply` reader; AC sentinel on
  disabled or error; `AttentionSensor` with `AttentionConfig::enabled`
  opt-in; `AttentionReading::attention_demand_scalar(ceiling_secs)` decays
  linearly with idle time; both fallback conservatively when data unavailable)
- S5.7.4 Gate modulation: thresholds rise under thermal stress, drop
  under high attention demand, and require higher value estimates
  under financial pressure. ✅ (Already implemented in E5.2 via
  `ThresholdGate::adaptive_threshold(&HomeostaticSignals)`; E5.7 adds
  `HomeostaticSignals::from_interoceptive(&InteroceptiveSignals)` so real
  sensor values are now wired into the gate; 3 new tests in `gate.rs`
  verify the bridge and end-to-end behaviour under severe stress)
- S5.7.5 Router modulation: route selection shifts toward cheaper
  models under power and financial pressure; planning horizon
  shortens under low battery. ✅ (`StaticRouter::resolve_with_modulation()`
  applies three-rule priority table: (1) severe depletion < 0.20 → force
  `cheap-local`; (2) moderate pressure 0.20–0.40 → downgrade `frontier`
  → `mid-tier`; (3) thermal_load > 0.80 → downgrade `frontier` →
  `mid-tier`; `ModulationDecision<'r>` carries requested vs effective route
  + reason string; `AuditEntry::RouterModulated` logged when modulation
  fires; `record_modulated_router_decision()` helper writes both entries)
- S5.7.6 Cache-controller modulation: the controller's state
  incorporates a memory-pressure signal so eviction becomes more
  aggressive under pressure. ⬜ *Deferred — depends on E5.4 (Learned
  KV-Cache Controller) which has not yet landed.*

**Exit criteria.**
1. A reproducible stress harness drives each signal across its full
   range and the resulting gate / router / controller behaviour is
   logged and asserted against a behavioural specification. ✅
   (`stress_harness_sweeps_financial_budget_across_full_range` — 11 steps
   from 0.0 to 1.0, asserts cheap-local/mid-tier/frontier at each boundary;
   `stress_harness_sweeps_power_budget_across_full_range` — same sweep on
   power_budget; `stress_harness_sweeps_thermal_load_across_full_range` —
   11 steps, asserts mid-tier for thermal > 0.80; all 58 new tests pass;
   `record_modulated_decision_emits_router_modulated_entry_when_modulated`
   — audit trail verified)
2. The `anima why` CLI command from E5.2 includes the homeostatic
   signal values at the time of the decision. ✅ (`cmd_why()` in
   `kernels/hosted/src/main.rs` — prints live `InteroceptiveSensorBundle`
   snapshot (all 6 signals + aggregate_stress) before gate scenarios;
   gate decisions include all 6 homeostatic fields (from E5.2);
   new "Router modulation with live interoceptive signals" section sweeps
   6 scenarios from neutral through severe financial/power/thermal stress
   and shows `RouterModulated` audit entry count)

### Epic E5.8 — Kill-Shot Demonstrations ⬜

**Scope.** The two demonstrations that anchor the cognitive thesis:
graceful-degradation-under-thermal-stress (headline) and long-horizon
coding-session retention (technical credibility builder). Both are
runnable on the hosted target and produce reproducible artefacts
(logs, transcripts, audit trails) that can be referenced in the
project's writeup.

**Dependencies.** E5.4 (long-horizon retention demo), E5.7 (graceful-
degradation demo).

**Stories.**
- S5.8.1 Demo A (headline): the same task is run on the hosted target
  once with `thermal_load` clamped low and once with an external
  compute load driving `thermal_load` high. Both runs complete; the
  high-thermal run uses cheaper routes, shorter context, and more
  reflexive policies; the comparison is rendered as a side-by-side
  transcript with audit-log highlights.
- S5.8.2 Demo B (technical credibility): a four-hour coding session
  is replayed against the cortex with and without the learned cache
  controller; retention of the user's original constraint, the error
  traces, and the architectural decisions is measured and reported.
- S5.8.3 Demo runner: a `cargo xtask demo --kind {graceful,retention}`
  command that drives the demo end-to-end and writes its artefact
  bundle under `artifacts/demos/<date>-<kind>/`.

**Exit criteria.**
1. Both demos produce reproducible artefacts on the hosted target
   from a clean checkout, with no live API calls (recorded fixtures
   only).
2. The graceful-degradation demo's behavioural delta is statistically
   significant against a paired baseline (n ≥ 8 runs per condition).
3. The retention demo reports a measurable advantage for the
   controller-gated cortex on the documented benchmark set.

---

## Stage 4 — Bare-Metal Isolation and Production Verification

Port to the microVM target, integrate `smoltcp` and `rustls`, complete
the formal verification surface, and harden for production.

### Epic E4.1 — `corpus` `no_std` Port ✅

**Scope.** Compile `corpus` under `no_std` with a custom allocator and a
UEFI boot trampoline that reaches a panic-handler-only state in QEMU.

**Dependencies.** End of Stage 3.

**Stories.**
- S4.1.1 `no_std`-clean `corpus`. ✅ (`#![no_std]` added to `crates/corpus/src/lib.rs`;
  all three source files use only `core` types — `core::sync::atomic`,
  `core::mem`, no `std` imports anywhere; 14 existing tests continue to
  pass because the test binary links `std` via the test harness)
- S4.1.2 Custom allocator integration. ✅ (`crates/corpus/src/heap_allocator.rs` —
  `BumpAllocator` implements `core::alloc::GlobalAlloc`; lock-free
  `AtomicUsize` cursor; alignment via power-of-two bit-mask;
  `dealloc` is an intentional no-op (bump allocator);
  registered as `#[global_allocator]` in `kernels/microvm/src/main.rs`;
  8 unit tests covering alignment, exhaustion, no-op dealloc,
  non-overlapping sequential allocations, and the `align_up` helper)
- S4.1.3 UEFI boot trampoline. ✅ (`kernels/microvm/` — standalone Cargo
  package (not workspace member), nightly toolchain via `rust-toolchain.toml`,
  `x86_64-unknown-uefi` target via `.cargo/config.toml` with `build-std`;
  `kernels/microvm/src/main.rs` — UEFI `#[entry]` point that initialises
  corpus's `BumpAllocator`, calls `uefi::helpers::init`, exercises
  `FrameAllocator` + `Vec<u32>` via the bump heap, then triggers a
  deliberate `panic!("ANIMA_PANIC: …")` whose message appears on the
  UEFI console; builds to a 21 KiB `.efi` PE32+ image)

**Exit criteria.**
1. QEMU boots the trampoline image and reaches the panic handler under a
   deliberate panic. ✅ (CI job `microvm-boot` in `.github/workflows/ci.yml`
   — installs `qemu-system-x86` + `ovmf`, builds the release `.efi`, places
   it at `esp/EFI/BOOT/BOOTX64.EFI`, boots with OVMF and `-serial file:…`,
   then `grep -q "ANIMA_PANIC"` on the captured serial log; `microvm-build`
   job verifies fmt + clippy + debug/release builds independently)

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

## Open Decisions

The following decisions are not yet resolved and gate the affected Stage
5 epics. They mirror Section 13 of `08-cognitive-architecture.md` and
must be closed (or the affected epic descoped) before that epic is
admitted into a sprint.

**OD1 — Distribution model.** Is AnimaOS a daemon that runs on existing
OSes (Linux, macOS, Windows), a Linux distribution with the cognitive
layer pre-integrated, or both? The microVM target in Stage 4 assumes
the distribution path; the cortex MVP in E5.1 is currently scoped
against the hosted target. The default working assumption is
hosted-daemon for E5.x, microVM for the production target, with the
distribution path explicitly out of scope until both stages close.

**OD2 — Local-first vs. API-first default routing.** The Thalamic
Router in E5.3 needs a default policy. Current working assumption:
local-first with explicit user opt-in for API escalation. This needs to
be validated against real workloads before E5.3 closes.

**OD3 — Cache controller training data.** E5.4 depends on representative
agentic traces. Phase 2 (= E5.1 + adjacent) will not produce enough
data on its own. Candidate sources — synthetic generation, public
agent trace datasets, human-in-the-loop curation — must be selected
before E5.4 enters implementation. This is the largest technical risk
in Stage 5.

**OD4 — User-facing surface.** The form of cortex ↔ user communication
(desktop notifications, dedicated UI, CLI, chat-style interface) is
unspecified. The decision affects E5.1's exit criteria and E5.6's
attention-escalation behaviour.

**OD5 — Privacy and trust model.** Identity memory (E5.5), the
sensorium opt-ins (E5.7), and the cortex's network surface (E5.6) all
intersect a single privacy story that has not yet been written. Default
posture is conservative — explicit opt-in for sensitive sensorium
streams, explicit opt-in for API model use, all identity memory
inspectable and editable — but the specification must exist before any
non-developer use.

**OD6 — L3 backing store for TurboQuant.** Epic E2.7 introduces
TurboQuant quantisation over the L3 archive. Two implementation
paths are credible: (a) keep the current `L3Archive` (and the
LanceDB direction named in `01-architecture.md §3.4`) and add a
custom TurboQuant layer in Rust against the existing API, or (b)
swap the L3 backing store to Qdrant 1.18 which ships TurboQuant
natively and provides the SIMD scoring kernels for free. Path (a)
preserves the no-host-OS / unikernel posture and the existing
provenance schema but requires writing the SIMD kernels ourselves.
Path (b) trades a heavier dependency (and the bare-metal port
question for Qdrant) for production-grade TurboQuant on day one.
The decision affects E2.7, the technical-stack section of
`01-architecture.md`, and the Stage 4 microVM port. Default
working assumption: path (a) on the hosted target, with path (b)
re-evaluated if Qdrant ships a `no_std` or unikernel-friendly mode.

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

### Epic EX.3 — Performance Regression Benchmark Suite ✅

A per-PR microbenchmark suite (Criterion) plus a nightly macro-benchmark
job. Begins in Stage 2 once the memory hierarchy is stable; tightens in
Stage 4.

**What was built:**
- `criterion = "0.5"` added as workspace dev-dependency.
- **`crates/scheduler/benches/scheduler.rs`** — 6 benchmarks across three groups:
  - `task_agenda/push/{100,1000,10000}` — cost of inserting tasks across all three priority tiers.
  - `task_agenda/select/{100,1000,10000}` — priority-ordered pop of a pre-filled agenda.
  - `mlfq/boost_all_to_high/{50,500,2000}` — bulk starvation-prevention tier promotion.
  - `mlfq/check_and_boost_no_op` — common no-op path when boost threshold is not reached.
  - `token_pipe/push_refund_cycle/{64,512,4096}` — complete credit push/refund cycle.
  - `token_pipe/bulk_push/{8,64,256}` — burst producer with abundant credits.
- **`crates/memory/benches/memory.rs`** — 7 benchmarks across four groups:
  - `arc_cache/sequential_inserts/{64,256,1024}` — full ARC miss path with eviction pressure.
  - `arc_cache/mixed_workload/{64,256,1024}` — warm reads + cold inserts reflecting agent execution.
  - `arc_cache/get_hits/{64,256,1024}` — read-only throughput on a fully-loaded warm cache.
  - `l1_vcm/occupied_blocks/{0,2048,4000,8192}` — block-occupancy ceiling-division (hot scheduler path).
  - `l1_vcm/add_tokens` — L1 token-count update with ceiling enforcement.
  - `memory_node/activation_at/{t=0,1,10,100}` — emotionally modulated exponential decay formula.
  - `memory_node/activation_batch/{64,512,4096}` — bulk decay evaluation (inner pruning-pass loop).
- **`crates/praxis/benches/praxis.rs`** — 10 benchmarks across three groups:
  - `tool_registry/lookup_echo` — HashMap probe + `Arc` clone (common read path).
  - `tool_registry/lookup_miss` — miss path for unregistered tool identifiers.
  - `tool_registry/dispatch_echo` — complete synchronous dispatch with circuit-breaker accounting.
  - `tool_registry/dispatch_clock` — dispatch including a `SystemTime::now` syscall.
  - `tool_registry/list_after_n_registrations/{10,100,1000}` — sorted list allocation.
  - `routing/filter_10_candidates` — typical online routing path (short list).
  - `routing/filter_candidates/{50,200,1000}` — filter scaling with linearly decreasing scores.
  - `routing/filter_all_equal/{50,200,1000}` — full-pass degenerate case (all scores equal).
  - `circuit_breaker/record_success_closed` — steady-state success accounting.
  - `circuit_breaker/record_failure_below_threshold` — failure accounting without state transition.
- **`.github/workflows/bench.yml`** — nightly CI job (02:00 UTC) running all three benchmark
  suites with `--output-format bencher`; HTML reports uploaded as 30-day artifacts.
  Also triggers on PR changes to `crates/scheduler/**`, `crates/memory/**`, `crates/praxis/**`
  to surface regressions before they land.

**Exit criteria met:**
1. ✅ Per-PR microbenchmark job in `.github/workflows/bench.yml`; triggers on changes to the three
   benchmarked crates.
2. ✅ Nightly macro-benchmark job (`schedule: cron: '0 2 * * *'`) with artifact upload.
3. ✅ `cargo build --benches -p scheduler/memory/praxis` clean; `cargo clippy --all-targets -D warnings`
   clean; `cargo fmt --check` clean.
4. ✅ All 237 existing workspace tests continue to pass unmodified.

### Epic EX.4 — Security Posture and Threat Model 🟡

Maintain a living threat model, run `cargo audit` and `cargo deny` in
CI, and produce a security review at the end of each stage.

**Delivered in this epic (partial — first pass):**
- `cargo audit` job added to `.github/workflows/ci.yml` — scans `Cargo.lock`
  against the RustSec advisory database on every PR; findings at error level
  block merge. ✅
- `cargo deny` job added to `.github/workflows/ci.yml` — enforces the
  licence allow-list, bans `openssl` and `git2`, detects duplicate
  dependency versions, and restricts to the crates.io registry. ✅
- `deny.toml` — machine-readable supply-chain policy: licence allow-list,
  banned crates, wildcard-version warnings, registry restriction. ✅
- `docs/09-threat-model.md` — living threat model covering trust zones,
  attack surface catalogue (AS-1 through AS-7), STRIDE threat catalogue
  (T-1 through T-8), security controls matrix, and per-stage security
  review checklist. ✅

**Remaining (future iterations):**
- Pin GitHub Actions `uses:` references to SHAs (currently tag-pinned only).
- SBOM generation via `cargo cyclonedx` or `cargo spdx`.
- Enable Dependabot / Renovate for automated dependency updates.
- Per-stage security review sign-off as each stage closes.

---

## Parallelisation Notes

- Stage-1 epics E1.3, E1.4, and E1.5 can proceed in parallel once E1.2
  is closed.
- Stage-2 memory epics (E2.1, E2.2, E2.6) and praxis epics (E2.3, E2.4,
  E2.5) form two parallel tracks that converge at the end of the stage.
  E2.7 (TurboQuant) sits across both memory tiers and may begin once
  E2.2 and E2.6 are merged; it does not need to wait for E2.5.
- Stage-3 sleep epics are strictly sequential after E3.4.
- Stage 5 — E5.1 is gating for the rest of the stage; once it lands,
  E5.2/E5.3 (gate + router) and E5.5 (memory split) can proceed in
  parallel. E5.4 has the largest research risk and may not close on
  the same timeline as the rest of Stage 5 — it should not gate stage
  closure unless explicitly promoted. E5.6 hardens incrementally
  throughout. E5.7 depends on E5.2/E5.3/E5.4 and converges them into
  E5.8.
- Stage 4 is gated on Stage 5 closure (per the Stage Sequencing note
  at the top of Stage 5). Within Stage 4 the epics are strictly
  sequential up to E4.5; E4.6 may begin on stable crates during
  Stage 2.
- All cross-cutting epics run in parallel with every stage.

## What Counts as Stage Closure

A stage closes only when every constituent epic is ✅ and the stage-level
exit criteria documented in `05-roadmap.md` are demonstrably met by a
green CI run plus a referenced audit-log trace. Rolling incomplete epics
across stage boundaries is explicitly disallowed.
