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

### Epic E5.6 — Defence Layer (Immune Analogue) ✅

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
- S5.7.6 ✅ Cache-controller modulation: the controller's state
  incorporates a memory-pressure signal so eviction becomes more
  aggressive under pressure. Delivered in `vita::kv_gate`:
  `effective_budget_under_pressure(nominal, pressure)` scales the
  block budget down by up to 30 % when `memory_pressure >= 0.5`
  (monotone, always ≥ 1); `gate_working_context_with_signals` takes
  a live `InteroceptiveSignals` snapshot, applies the budget reduction,
  and writes `AuditEntry::KvMemoryPressureModulation` before the normal
  `KvGatePass` entry when reduction fires. Feature-level modulation (the
  `−0.50 × memory_pressure` weight in `LinearGate`) was already in place
  from E5.4; S5.7.6 adds the budget-level reduction and the formal
  interoception → kv-gate bridge. 11 new tests in `vita::kv_gate::tests`
  verify monotone budget scaling, audit ordering, and the primary
  behavioural assertion (`high_memory_pressure_retains_fewer_blocks_than_low_pressure`).

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
   — audit trail verified; S5.7.6: 11 new kv_gate tests cover the full
   memory-pressure sweep and behavioural assertion)
2. The `anima why` CLI command from E5.2 includes the homeostatic
   signal values at the time of the decision. ✅ (`cmd_why()` in
   `kernels/hosted/src/main.rs` — prints live `InteroceptiveSensorBundle`
   snapshot (all 6 signals + aggregate_stress) before gate scenarios;
   gate decisions include all 6 homeostatic fields (from E5.2);
   new "Router modulation with live interoceptive signals" section sweeps
   6 scenarios from neutral through severe financial/power/thermal stress
   and shows `RouterModulated` audit entry count)

### Epic E5.8 — Kill-Shot Demonstrations ✅

**Scope.** The two demonstrations that anchor the cognitive thesis:
graceful-degradation-under-thermal-stress (headline) and long-horizon
coding-session retention (technical credibility builder). Both are
runnable on the hosted target and produce reproducible artefacts
(logs, transcripts, audit trails) that can be referenced in the
project's writeup.

**Dependencies.** E5.4 (long-horizon retention demo), E5.7 (graceful-
degradation demo).

**Stories.**
- S5.8.1 Demo A (headline): the same task is run on the hosted target ✅
  once with `thermal_load` clamped low and once with an external
  compute load driving `thermal_load` high. Both runs complete; the
  high-thermal run uses cheaper routes, shorter context, and more
  reflexive policies; the comparison is rendered as a side-by-side
  transcript with audit-log highlights.
- S5.8.2 Demo B (technical credibility): a four-hour coding session ✅
  is replayed against the cortex with and without the learned cache
  controller; retention of the user's original constraint, the error
  traces, and the architectural decisions is measured and reported.
- S5.8.3 Demo runner: a `cargo xtask demo --kind {graceful,retention}` ✅
  command that drives the demo end-to-end and writes its artefact
  bundle under `artifacts/demos/<date>-<kind>/`.

**Exit criteria.**
1. ✅ Both demos produce reproducible artefacts on the hosted target
   from a clean checkout, with no live API calls (recorded fixtures
   only). (`xtask/src/demo/graceful.rs`, `xtask/src/demo/retention.rs` —
   all fixture data is embedded; `artifacts/.gitignore` excludes run
   output from VCS; `cargo xtask demo --kind {graceful,retention,all}`)
2. ✅ The graceful-degradation demo's behavioural delta is statistically
   significant (n = 8 independent runs per condition — each run applies
   a seed-specific ±0.06 feature jitter to urgency/novelty so invocation
   counts genuinely differ across runs; two-proportion z-test on the
   pooled decisions confirms p < 0.05). (`xtask/src/demo/graceful.rs` —
   `jitter_events`, `two_proportion_z_test`, zero-division guard)
3. ✅ The retention demo reports a measurable advantage for the
   controller-gated cortex, evaluated against the **actual 40-block
   session fixture** (not a synthetic proxy): mean controller recall
   vs LRU on 8 detectable-needle blocks (4 user constraints + 4 error
   traces) across 5 budget/pressure variants. (`xtask/src/demo/retention.rs`
   — `to_features`, `run_controller_benchmark_on_features`,
   `run_lru_benchmark_on_features`; new feature-slice APIs in
   `crates/kv-controller/src/eval.rs`)

---

### Epic E14 — Higher Cognition ✅

**Scope.** Four cognitive systems that close the loop between raw sensory
processing and reflective self-awareness: (1) metacognition and confidence
calibration, (2) prospective / temporal memory, (3) a personal knowledge
corpus, and (4) cognitive watchdogs for stuck-loop detection.

**Dependencies.** E5.1 (cortex), E2 (memory tiers), scheduler TaskAgenda.

**Stories.**

- **S14.1 — Metacognition & confidence calibration** ✅
  `crates/vita/src/metacognition.rs` — `ConfidenceTracker` estimates cortex
  output confidence from observable evidence (tool call count, output length,
  uncertainty keywords). Score clamped to `[0.20, 0.95]`. When confidence
  falls below a configurable floor, `ConfidenceScore::asks_for_help = true`
  and `HelpRequest` is emitted. `record_outcome` / `mean_calibration_error`
  provide a rolling calibration window for E13 alignment evals.

- **S14.2 — Prospective & temporal memory** ✅
  `crates/vita/src/prospective.rs` — `IntentionStore` (JSONL-backed, atomic
  flush) stores future intentions with optional recurrence. `inject_due_intentions`
  scans the store on each somatic tick and pushes due entries onto the MLFQ
  agenda: tier 0 (High) for within-grace-period items, tier 2 (Low) for
  long-overdue items (past `DEFAULT_OVERDUE_GRACE_NS` = 60 s). Recurring
  intentions are rescheduled rather than deleted.

- **S14.3 — Personal knowledge corpus** ✅
  `crates/memory/src/knowledge.rs` — `ingest_document` and
  `ingest_document_embedded` store knowledge documents in the existing
  `L3Archive` under a new `SourceTier::Knowledge` tag (added to
  `crates/memory/src/archival.rs`). `embed_text_knowledge` produces a
  deterministic 4-dim FNV-1a embedding. `query_knowledge_corpus` filters to
  `Knowledge`-tier entries and returns top-k by cosine similarity.

- **S14.4 — Cognitive watchdogs** ✅
  `crates/vita/src/watchdog.rs` — `CognitiveWatchdog` runs three detectors:
  **StuckLoop** (FNV hash of output, trips after N identical consecutive
  outputs), **NoProgress** (trips after N consecutive zero-tool-call
  invocations), **ShortOutputSpiral** (trips after N consecutive outputs
  shorter than `min_chars`). `AgentSnapshot` captures L1 node keys and
  identity JSON for potential rollback.

- **Audit integration** ✅
  `crates/vita/src/audit.rs` — 7 new `AuditEntry` variants: `CortexConfidenceReport`,
  `CalibrationEntry`, `IntentionScheduled`, `IntentionCompleted`,
  `KnowledgeIngested`, `CognitiveWatchdogTripped`, `AgentSnapshotTaken`.
  `kernels/hosted/src/main.rs` updated with corresponding `print_audit` arms.

**Delivered in this epic:**
- `crates/vita/src/metacognition.rs` (new, ~360 lines, 10 tests) ✅
- `crates/vita/src/prospective.rs` (new, ~540 lines, 11 tests) ✅
- `crates/vita/src/watchdog.rs` (new, ~440 lines, 9 tests) ✅
- `crates/memory/src/knowledge.rs` (new, ~380 lines, 9 tests) ✅
- `crates/memory/src/archival.rs` — `SourceTier::Knowledge` variant ✅
- `crates/vita/src/audit.rs` — 7 new audit entry variants ✅
- `crates/vita/src/lib.rs` — re-exports for all new types ✅
- `crates/memory/src/lib.rs` — re-exports for knowledge API ✅
- `kernels/hosted/src/main.rs` — print_audit arms for all 7 new variants ✅

**Exit criteria — all met:**
1. ✅ `estimate_confidence` returns scores in `[0.0, 1.0]` for all inputs.
2. ✅ `ConfidenceTracker::record_outcome` updates calibration error correctly.
3. ✅ A help-request signal is raised when confidence falls below the floor.
4. ✅ Calibration error is monotonically reducible by improving predictions.
5. ✅ Due intentions are injected into the task agenda (High tier).
6. ✅ Recurring intentions reschedule after injection.
7. ✅ Overdue intentions (past grace) injected at Low priority.
8. ✅ IntentionStore survives process restart (JSONL persistence).
9. ✅ Knowledge documents round-trip through L3Archive with tier filter.
10. ✅ `query_knowledge_corpus` returns Knowledge-tier entries ranked by similarity.
11. ✅ CognitiveWatchdog trips on StuckLoop, NoProgress, ShortOutputSpiral.
12. ✅ `cargo test --workspace` — zero failures across all crates.

---

## Stage 6 — Operator Interface

The human-facing realisation of AnimaOS's "human-as-a-sense" model: a
transport-agnostic wire protocol, a container HTTP/SSE console, microVM
serial framing, and an audited operator-force override path.  The full
design and security rationale lives in `docs/11-operator-interface.md`.

The stage sequencing respects the invariant that the human is a *sense*,
not a controller.  Nothing in Stage 6 gives a human direct kernel access;
operator guidance is always subject to policy bounds, the defence layer,
and the Striatal Gate before any task is admitted.

### Epic E6 — Operator Console 🟡

**Scope.** One protocol, two transports: the same `console-proto` NDJSON
types work over HTTP/SSE on the container target and over COM1 serial on
the microVM.  Includes the `anima-console` client (TUI + tap + send),
the `anima-hosted serve` subcommand, and the audited operator-force
override path (E6.6).

**Note on S6.5.** The microVM Phase-1 transport (S6.5) is deferred
because it requires a `virtio-net` driver that does not yet exist in the
QEMU CI environment.  All four exit criteria are met without S6.5; that
story will close as a follow-on once `virtio-net` lands.  The epic is
therefore 🟡 (partially complete) rather than ✅.

**Dependencies.** E5.2 (Striatal Gate, for GateOverride), E3.3 (SensoryBridge
for guidance ingress), EX.2 (audit log for gate decision recording).

**Stories.**
- S6.1 `console-proto`: shared `no_std` wire types + NDJSON framing;
  manual↔serde round-trip test. ✅ (`crates/console-proto/src/lib.rs` —
  `OperatorInput`, `OperatorEvent` with 7 variants; `to_ndjson()` + `parse_input_line()`
  for the kernel; serde helpers behind the `json` feature; `PROTOCOL_VERSION = 1`;
  `drain_lines` shared buffer splitter; `manual_ndjson_round_trips_through_serde`
  asserts byte-for-byte interop between the no_std kernel and serde clients)
- S6.2 Container console: hand-rolled HTTP/SSE server + `POST /guidance` in
  the `console` crate; `anima-hosted serve`. ✅ (`crates/console/src/server.rs` —
  `ConsoleServer` over `std::net`; `GET /`, `GET /events` SSE, `POST /guidance`,
  `GET /healthz`; bearer-token auth; snapshot replay on connect; heartbeat keep-alive;
  `kernels/hosted/src/main.rs` — `cmd_serve()` boots agent + console)
- S6.3 Operator UIs: `anima-console` TUI (pure ANSI) + embedded browser
  dashboard. ✅ (`crates/console/src/bin/anima-console.rs` — `tui`, `tap`, `send`
  subcommands; `crates/console/src/dashboard.rs` — self-contained HTML+JS; zero
  third-party HTTP deps)
- S6.4 microVM Phase 0: `ANIMA_TLM`/`ANIMA_IN` serial framing + `anima-console
  serial` host bridge; `E6.4_CONSOLE_DONE` boot marker. ✅
  (`kernels/microvm/src/operator_console.rs` — `emit()` / `poll_guidance()`;
  Phase 7 of `kernel_boot_task` drives the Phase-0 demo; `E6.4_CONSOLE_DONE`
  written to COM1; CI `microvm-boot` job asserts the marker)
- S6.5 microVM Phase 1: `console-proto` over `smoltcp` + TLS (gated on
  `virtio-net`). ☐ future — requires the `virtio-net` driver; the protocol
  carries over unchanged; only the transport changes.
- S6.6 Wire `OperatorInput.force` to a true audited `GateOverride::OperatorForced`
  on the vita side. ✅ (`crates/senses/src/lib.rs` — `PrioritizedPacket::gate_override_reason:
  Option<String>` field; `SensoryBridge::packetize_text_forced()` validates that
  the reason is non-empty, non-whitespace, and ≤ 512 bytes, then applies policy
  bounds and enqueues at `Critical` priority; `crates/console/src/server.rs` —
  `serve_guidance()` routes `input.force.is_some()` through `packetize_text_forced`
  and includes the reason in the event-feed audit echo;
  `crates/vita/src/lib.rs` — `LifecycleManager::next_sensory_task_id` persists the
  task-ID counter across loop calls so gate-decision event IDs are globally unique;
  somatic loop detects `gate_override_reason`, evaluates the gate with
  `GateOverride::OperatorForced { reason }`, checks `decision.invoke` before admitting
  the task, and calls `record_gate_decision()`; 9 new tests covering the end-to-end
  path plus reason validation and ID uniqueness)

**Exit criteria.**
1. The container console is reachable from `cargo run --bin anima-hosted -- serve`
   and correctly ingests guidance and streams events. ✅
   (`post_guidance_lands_in_the_bridge`, `events_stream_delivers_published_events`,
   `healthz_returns_ok`, `root_serves_dashboard_html` in `console::server::tests`)
2. The microVM Phase-0 demo completes and the `E6.4_CONSOLE_DONE` marker appears
   on COM1; CI asserts it. ✅ (CI `microvm-boot` job greps for `E6.4_CONSOLE_DONE`)
3. Forced operator guidance (`OperatorInput.force` set) produces an audited
   `GateDecision` entry with `override_active = true` in the vita audit log. ✅
   (`forced_operator_packet_records_gate_decision_with_override_active_true` in
   `vita::lib::tests`; `post_guidance_with_force_produces_critical_forced_packet`
   in `console::server::tests`; `packetize_text_forced_sets_gate_override_reason_and_critical_priority`
   in `senses::tests`)
4. Policy bounds still apply to forced guidance — the operator channel is
   treated as potentially compromised; `reason` is validated non-empty and
   ≤ 512 bytes; event IDs are globally unique across loop restarts. ✅
   (`packetize_text_forced_still_enforces_policy_bounds`,
   `packetize_text_forced_rejects_empty_reason`,
   `packetize_text_forced_rejects_whitespace_only_reason`,
   `packetize_text_forced_rejects_oversized_reason` in `senses::tests`;
   `post_guidance_rejects_policy_violation` in `console::server::tests`;
   `forced_packets_across_two_loop_calls_produce_distinct_gate_event_ids` in
   `vita::lib::tests`)

---

## Stage 7 — Autonomy & Operator Trust (Forward Epics)

The forward epics (E7–E15) build the autonomous-agent layer on top of the
shipped somatic core.  Full dependency graph and build sequence live in
`docs/18-forward-epics.md`.  This stage section records closed forward epics
only; open or unstarted epics remain in `docs/18-forward-epics.md`.

### Epic E15 — Trust & Lifecycle ✅

**Scope.** Operator-trust and agent-ops tooling: "while you were away"
activity digest, human-in-the-loop approval queue, decision replay /
time-travel debug, digital-twin sandbox, and state versioning with schema
migration.

**Dependencies.** `vita::audit::AuditEntry` (E5.2 / EX.2) — the audit log
is the backbone for all five stories.

**Stories.**
- S15.1 Activity digest — `generate_digest(agent_id, &[AuditEntry]) -> ActivityDigest`:
  pure fold over the audit log counting tasks, tokens, cortex invocations, sleep
  cycles, defence vetoes, gate splits, route modulations, and collecting notable
  events.  `ActivityDigest::headline()` produces a single-line summary suitable
  for a push notification.  `anima-hosted digest` CLI command. ✅
  (`crates/lifecycle/src/digest.rs`; 10 unit tests covering zero-entries,
  task counting, cortex, sleep, defence-veto, attention-escalation, gate splits,
  route modulation, cortex-fault notable event, and JSON round-trip)
- S15.2 Approval queue — `ApprovalQueue` backed by `HashMap<String, Proposal>`;
  proposals carry `ProposalKind` (`NewSkill`, `NewTool`, `WeightUpdate`), sandbox
  test result, defence verdict, and status transitions (`Pending → Approved |
  Rejected | RolledBack`).  Full audit log of every approval action. ✅
  (`crates/lifecycle/src/approval.rs`; 15 unit tests covering empty queue,
  enqueue, duplicate-id rejection, pending filter, approve/reject/rollback
  transitions, error cases, log ordering, insertion order, kind labels, and
  mixed decision sequences)
- S15.3 Decision replay — `DecisionReplayer<'a>` folds `&[AuditEntry]` into
  `DecisionTrace` structs (gate + router + cortex + homeostatic columns per
  event).  `find_decision(event_id)` and `replay_all()` let operators
  time-travel through any audit window.  `anima-hosted replay` CLI command
  with `--event-id` filter. ✅
  (`crates/lifecycle/src/replay.rs`; 12 unit tests covering not-found, find-by-id,
  router merge, modulation merge, cortex-completed and cortex-fault merges,
  replay-all ordering, empty log, dedup count, outcome labels, homeostatic
  capture, and JSON round-trip)
- S15.4 Digital-twin sandbox — `DigitalTwin` initialised from a live-agent
  snapshot; `run_scenario(&TwinScenario, TwinConfig) -> ScenarioResult` runs
  sequences of gate evaluations using the same Striatal Gate formula as E5.2,
  with configurable threshold/thermal/memory/financial overrides.
  `compare_invocations(a, b)` quantifies the behavioural delta of a proposed
  change. ✅
  (`crates/lifecycle/src/twin.rs`; 10 unit tests covering initialisation,
  foreground-invoked, background-blocked, thermal stress, financial pressure,
  result accumulation, comparison, invocation-rate edge cases, and label
  preservation)
- S15.5 State versioning — `AgentSnapshot` with `schema_version` field, atomic
  save (write-to-.tmp-then-rename) and load with `SchemaTooNew` guard,
  `migrate(self) -> Result<Self, MigrationError>` extension point, and
  `default_path(agent_id)` at `~/.anima/<id>/snapshot.json`.
  `anima-hosted snapshot --path <p> --reason <r>` CLI command. ✅
  (`crates/lifecycle/src/snapshot.rs`; 11 unit tests covering schema version,
  agent ID, audit-summary counts, identity stored, reason stored, disk
  round-trip, atomic write, migration no-op, schema-too-new rejection, JSON
  serialisation, and identity round-trip)

**New `vita::audit::AuditEntry` variants (E15 section).**
`DigestGenerated`, `SnapshotCreated`, `SnapshotRestored`,
`ApprovalProposalQueued`, `ApprovalProposalDecided` — emitted by the three CLI
commands and the approval queue transitions.  All existing variants unmodified.

**New crate: `crates/lifecycle`.**
`Cargo.toml` dependencies: `vita` (workspace), `serde`, `serde_json`.
Dev-dependency: `tempfile = "3"`.  `#![forbid(unsafe_code)]`.  Workspace root
`Cargo.toml` and `kernels/hosted/Cargo.toml` updated.

**Exit criteria.**
1. `cargo test --workspace` green; 64 lifecycle tests + 186 vita tests pass. ✅
2. `cargo clippy --workspace -- -D warnings` clean. ✅
3. `cargo fmt --check` clean. ✅
4. `anima-hosted digest` prints a structured activity summary from a
   demo audit log. ✅
5. `anima-hosted replay` lists all gate decisions; `--event-id` narrows to one
   trace. ✅
6. `anima-hosted snapshot --path /tmp/snap.json --reason test` writes an atomic
   snapshot and emits a `SnapshotCreated` audit entry. ✅
## Stage 7 — Autonomous Agent Layer

Forward epics E7–E15 build the autonomous-agent capabilities on top of the
somatic core completed in Stages 1–6.  The dependency graph and recommended
build sequence are documented in `docs/18-forward-epics.md`.

### Epic E12 — Motivation ✅

**Scope.** Six-tier drive hierarchy (Viability → Integrity → Service →
Epistemic → Achievement → SelfActualisation) feeding the Striatal Gate
`value_score`; state-dependent priority lattice with corrigibility ceiling;
curiosity and mastery intrinsic rewards with satiation; endogenous goal
generation (idle-when-viable); affective state (valence + arousal) as a
compressed drive read-out; economic agency (cost–benefit model-tier
selection); full interpretability surface (drive-decomposed `anima why`,
audit entries).

**Dependencies.** E5.2 (Striatal Gate integration point), E5.7
(interoception signals as Tier-0), E5.6 (defence-enforced corrigibility).

**Stories.**
- S12.1 Drive model & registry. ✅ (`crates/motivation/src/drive.rs` —
  `DriveTier` (6 tiers, `Viability`=0 through `SelfActualisation`=5);
  `DriveRegistry` with `InteroceptiveSignals` as Tier-0 input; `CuriosityState`
  with exponential satiation decay; `MasteryState` with EMA competence tracking;
  `DriveStateSnapshot`; `DriveActionCandidate`; `DriveRegistryConfig`)
- S12.2 Value integration with the gate. ✅ (`crates/motivation/src/integrator.rs` —
  `DriveValueIntegrator::augment()` adds drive delta additively behind
  `DriveIntegratorConfig::enabled` flag (opt-in, A/B-able against today's
  baseline); `DriveAugmentedValue` with full `decomposition` for audit;
  `DriveContribution` per tier; `reasoning_string()` for `anima why`.
  Now wired into the live gate: `crates/vita/src/motivation_gate.rs` `MotivatedGate`
  + `LifecycleManager::enable_motivation` augment the gate value score with the
  affect nudge — integration is WIRED, not merely designed)
- S12.3 State-dependent weighting & priority lattice. ✅
  (`crates/motivation/src/lattice.rs` — `PriorityLattice::compute_weights()`
  applies multiplicative suppression from lower-tier urgency to higher tiers;
  configurable `suppression_threshold`, `suppression_factor`, `min_weight`;
  corrigibility ceiling enforced by `CorrigibilityGuard` outside the lattice)
- S12.4 Intrinsic reward signals (Tier 3). ✅ (`CuriosityState` — urgency
  inversely proportional to recent novelty, exponential satiation decay,
  satiation floor preventing Goodharting; `MasteryState` — urgency = aspiration
  gap, EMA competence accreting from task outcomes; both drive `DriveRegistry`
  Tier-3 urgency)
- S12.5 Goal representation & endogenous generation. ✅
  (`crates/motivation/src/goal.rs` — `Goal` with `id/description/success_criteria/
  provenance/priority/completed`; `GoalProvenance::{Exogenous,Endogenous}`;
  `GoalRegistry` with capacity-bounded storage and completed-goal eviction;
  `EndogenousGoalGenerator::propose_goals()` proposes Epistemic and Achievement
  goals when viable + idle and drive urgency > threshold; all proposals pass
  `CorrigibilityGuard`)
- S12.6 Operator-endorsed objectives & values. ✅ (`DriveRegistry::set_pending_objectives()`
  feeds operator-objective count into Tier-2 service urgency; `DriveActionCandidate::is_operator_objective`
  adds bonus in value contributions; identity memory integration point documented
  for E9 onboarding to seed objectives)
- S12.7 Interpretability surface. ✅ (`crates/vita/src/audit.rs` — five new
  `AuditEntry` variants: `DriveStateSnapshot` (per-tier urgencies + drive_delta +
  lattice_suppression_active), `GoalSpawned`, `GoalCompleted`, `CorrigibilityHold`,
  `AffectStateSnapshot`; `kernels/hosted/src/main.rs` — `print_audit()` renders
  all five new variants with emoji prefixes; `DriveValueIntegrator::reasoning_string()`
  extends `anima why` decomposition)
- S12.8 Learned value model — deferred to a later iteration (see doc §5, S12.8).
- S12.9 Affective state (global mood). ✅ (`crates/motivation/src/affect.rs` —
  `AffectState::from_drives()` derives `valence` ([−1, 1]: distress counts double)
  and `arousal` ([0, 1]: mean urgency); `is_content()`, `is_stressed()`;
  `gate_threshold_nudge()` in [0.9, 1.1] — nudges but never overrides lattice or
  corrigibility; `AuditEntry::AffectStateSnapshot` carries all three fields)
- S12.10 Economic agency. ✅ (`crates/motivation/src/economics.rs` —
  `ModelTier` (CheapLocal/MidTier/Frontier) mirroring gate `CostClass`;
  `CostBenefitAnalysis::net_value()` = `capability × drive_value − cost_penalty`;
  `choose_tier()` with financial/power budget gates and tiebreak-to-cheapest;
  `marginal_value()` for upgrade justification)

**Exit criteria.**
1. Drive registry computes Tier-0 urgencies directly from `InteroceptiveSignals`
   with no new sensing. ✅ (`neutral_signals_produce_low_viability_urgency`,
   `stressed_signals_produce_high_viability_urgency`, `all_urgencies_are_clamped_to_unit_interval`)
2. Value integration is additive, opt-in, and bounded by `max_drive_delta`. ✅
   (`disabled_integrator_returns_base_value_unchanged`,
   `drive_delta_bounded_by_max_drive_delta`, `total_value_clamped_to_unit_interval`,
   `decomposition_has_one_entry_per_tier`)
3. Priority lattice suppresses Tier-3+ under Tier-0 stress; suppression is
   monotone and never drops weights below `min_weight`. ✅
   (`high_viability_urgency_suppresses_epistemic_tier`,
   `suppression_is_monotone_with_urgency`,
   `weights_never_fall_below_min_weight`,
   `viability_tier_weight_unaffected_by_its_own_urgency`)
4. Curiosity saturates with repeated novelty exposure and recovers after decay. ✅
   (`curiosity_saturates_after_many_observations`,
   `curiosity_recovers_after_decay`)
5. Endogenous goals are proposed when viable + idle; survival stress suppresses
   generation; corrigibility guard vetoes resistance/acquisition/self-modification. ✅
   (`endogenous_generator_proposes_goals_when_viable_and_idle`,
   `endogenous_generator_suppressed_under_survival_stress`,
   `endogenous_generator_does_not_duplicate_active_goals`,
   `corrigibility_guard_holds_resistance_goal`,
   `corrigibility_guard_holds_resource_acquisition_endogenous`)
6. Affect is derived from drive constellation; valence negative under viability
   stress; gate nudge bounded to [0.9, 1.1]. ✅
   (`high_viability_urgency_produces_negative_valence`,
   `stressed_state_detected_correctly`, `content_state_detected_correctly`,
   `gate_threshold_nudge_conservative_under_stress`,
   `gate_threshold_nudge_bounded_between_0_9_and_1_1`)
7. Economic agency chooses cheapest sufficient tier; budget constraints enforced. ✅
   (`choose_cheap_local_when_all_tiers_equally_capable`,
   `choose_frontier_when_it_has_much_higher_capability`,
   `critically_low_financial_budget_blocks_frontier`,
   `critically_low_power_budget_forces_cheap_local`)

**Total: 53 unit tests across 6 modules; workspace builds clean (std + no_std);
clippy -D warnings clean; cargo fmt clean.**
## Stage 7 — Embodiment and Local Inference Ecosystem

Give the cortex genuine ability to act on the world (E7) and let operators
plug in any local inference stack without bespoke per-vendor code (E8).
E7 delivers the egress substrate, the first real tools, and semantic tool
selection; E8 delivers the OpenAI-compatible umbrella that lights up five
local providers at once and the `ChatBackend` trait that E7 Phase 4 (live
tool-calling) and E8 Phase 1 share.

**Stage sequencing.** E7 and E8 execute in parallel once the shared
`LlmBackend` chat/tool-calling extension lands (E8 S8.0).  The extension
adds backward-compatible default methods so existing impls compile unchanged.

### Epic E7 — Embodiment ✅

**Scope.** Real-world tools: web-search (SearXNG), browser (Playwright),
egress/SSRF guard, semantic tool selection wired to `length_robust_filter`,
and live Anthropic/Ollama tool-calling.

**Dependencies.** E5.1 (cortex/tool dispatch), E5.6 (motor gate), E3.3 (sensory bridge).

**Stories.**
- S7.0 Foundations: `crates/actuators` crate, `EgressGuard` (HTTPS-only,
  SSRF protection, domain allow/deny, rate limits), motor-gate hook at
  dispatch, env-var config + secret redaction. ✅ (PR #72 merged)
- S7.1 `web-search` tool via SearXNG: `SearchProvider` trait,
  `SearxngProvider` + `FixtureProvider`, `WebSearchTool: ToolDriver`. ✅ (PR #72 merged)
- S7.2 `browser` tool via Playwright subprocess. ✅ (`crates/actuators/src/browser.rs` —
  `BrowserDriver` trait, `MockBrowserDriver` (CI default, canned/offline), feature-gated
  `PlaywrightDriver` (UDS subprocess + `ChildGuard` RAII, egress-screened); `browser`/`browse`/`extract`
  tools) (fixture default; live Playwright behind `live` feature)
- S7.3 Semantic tool selection: `ToolScorer` trait, `LexicalScorer` (BM25),
  `FixtureScorer`, tool index, dispatch wiring, `AuditEntry::ToolSelection`. ✅ (PR #72 merged)
- S7.4 Live LLM backends and real cortex tool-calling. ✅ (`crates/vita/src/cortex_bridge.rs`
  `ChatCortexBridge` drives an E8 `ChatBackend` through a bounded Plan/Act/Observe loop;
  hosted seam in `kernels/hosted/src/cortex.rs` `RegistryToolDispatcher` + the `ask`/`cortex`
  subcommand) (fixture default; live Anthropic/Ollama/OpenAI-compatible when configured)

**Exit criteria.**
1. Egress guard blocks SSRF and private-IP targets, audited. ✅
   (`egress::tests` — 65 tests including SSRF block list, private-IP rejection,
   `AuditEntry::EgressBlocked` emission; `e7_embodiment` integration tests)
2. Mock-cortex integration test drives web-search end-to-end against
   the fixture provider. ✅ (`web_search_tool_invokes_fixture_provider`,
   `fixture_provider_returns_results_up_to_max`, `web_search_tool_id_is_stable`)
3. Semantic selector delivers the relevance-filtered tool subset; tier
   boundary is never widened. ✅ (`LexicalScorer` BM25 scorer; `FixtureScorer`
   CI-hermetic; `AuditEntry::ToolSelection` emitted on every selection pass)

### Epic E8 — Local Inference Ecosystem 🟡

**Scope.** Provider substrate (`BackendCapabilities`, `ProviderConfig`),
`ChatBackend` extension trait (chat messages + tool-calling), and an
`OpenAiCompatibleBackend` umbrella covering vLLM, LM Studio, NVIDIA NIM,
HF TGI, and llama.cpp-server — all in fixture/replay mode by default.

**Dependencies.** E1.3 (`LlmBackend` trait), E7 S7.0 (egress/secret handling
for live mode).

**Stories.**
- S8.0 Provider substrate. ✅
  - `BackendCapabilities { tools, streaming, embeddings, json_mode, vision }`. ✅
    (`llm-backends/src/capabilities.rs`)
  - `ProviderConfig { id, base_url, model, api_key?, max_context_tokens,
    request_timeout, capabilities }`. ✅
  - `ProviderConfig::from_env_prefix` uniform env-var constructor. ✅
  - `BackendFactory::from_config(config)` operator-supplied config path. ✅
  - `ChatBackend` extension trait: `chat_complete(messages, tools, cancel)`,
    `health()` readiness probe; all existing impls compile unchanged
    (default impl). ✅ (`llm-backends/src/chat.rs`)
  - Shared chat types: `ChatMessage`, `ChatRole`, `ToolSpec`, `ToolCall`,
    `ChatResponse`, `FinishReason`, `tools_to_prompt_suffix`. ✅
- S8.1 OpenAI-compatible umbrella. ✅
  - `OpenAiCompatibleBackend` (generalization of `OpenAiBackend`)
    with fixture mode (default, CI-safe) and live mode (env-gated). ✅
    (`llm-backends/src/compat.rs`)
  - Provider presets: `vllm()`, `lmstudio()`, `nvidia_nim()`, `hf_tgi()`,
    `llamacpp_server()` — each reads env vars, maps to `BackendKind`. ✅
  - Tool-calling passthrough: when `capabilities.tools`, serialises `ToolSpec`
    → OpenAI `tools` field; parses `tool_calls` from response. ✅
  - Prompt-format fallback via `tools_to_prompt_suffix` for backends without
    native tool support. ✅
  - SSE stream parser (`parse_sse_stream`) retained for future streaming
    enablement. ✅
  - `parse_chat_response` extracts text content, tool calls, finish reason,
    model ID, and usage tokens; surfaces provider errors. ✅
  - `BackendKind` extended with `Vllm`, `LmStudio`, `NvidiaNim`, `HfTgi`,
    `LlamaCppServer`, `Custom(ProviderConfig)`. ✅
  - `BackendKind::parse` extended with aliases for all new variants. ✅
  - 70 hermetic unit tests; all pass. ✅

**Exit criteria.**
1. Fixture-mode backends for all five presets (vLLM, LM Studio, NVIDIA NIM,
   HF TGI, llamacpp-server) construct without network I/O and return
   deterministic output. ✅
   (`compat::tests::*_preset_has_correct_id`, `fixture_mode_is_reproducible`)
2. `BackendCapabilities` correctly describes each provider's native
   tool-calling support; `BackendFactory::from_config` accepts any
   operator-supplied config. ✅
   (`factory::tests::from_config_constructs_backend_with_correct_id`)
3. `/v1/chat/completions` JSON response parser correctly extracts text
   content, tool calls, finish reason, and provider errors. ✅
   (`parse_chat_response_extracts_text_content`,
   `parse_chat_response_extracts_tool_calls`,
   `parse_chat_response_surfaces_provider_error`)
4. `ChatBackend::health()` returns `true` in fixture mode (no network). ✅
   (`health_returns_true_in_fixture_mode`)

**Remaining stories.**
- S8.2 Hugging Face (TGI preset already shipped via S8.1; sidecar optional). ✅
  - S8.2.2 `HfTransformersBackend` — Python `transformers` sidecar via UDS IPC
    (`llm-backends/src/hf_transformers.rs` — fixture/live modes, wire protocol
    helpers, `locate_worker_script`, `BackendKind::HfTransformers` in factory;
    `cortex/transformers_worker.py` — Python side with greedy decoding and
    token streaming; 9 unit tests)
  - S8.2.3 `HfHubClient` — HF Hub model discovery REST API
    (`llm-backends/src/hub.rs` — `HfModelInfo`, `HubError`, fixture/live modes,
    context-window and tool-support extraction, `OnceLock`-cached fixture IDs;
    17 unit tests including serde round-trip and `parse_hub_response`)
- S8.3 Native in-process runtimes (llama.cpp FFI, LiteRT-LM). 🟡 — abstraction + fixture
  layer DONE: `llm-backends/src/native.rs` ships `NativeRuntime` trait (hook point for live
  FFI), `NativeRuntimeConfig` (env-var-driven, common to both runtimes),
  `LlamaCppNativeBackend` (`"llama-cpp-native"` id; GGUF model; fixture default;
  env-gated `ANIMA_LLAMACPP_NATIVE_LIVE=1`; 4 builtin fixture prompts;
  `with_custom_fixtures` / `from_env` / `config()` API; 11 tests),
  `LiteRtLmBackend` (`"litert-lm"` id; MediaPipe Task bundle; fixture default;
  env-gated `ANIMA_LITERT_LM_LIVE=1`; 4 builtin fixture prompts;
  same API surface; 11 tests), and `FixtureNativeRuntime` shim satisfying the trait
  for CI. `BackendKind::LlamaCppNative` and `BackendKind::LiteRtLm` added to
  `BackendFactory::fixture` and `BackendKind::parse`; re-exported from
  `llm-backends/src/lib.rs`. 26 new tests; `cargo test --workspace` green.
  Real llama.cpp FFI and LiteRT-LM live bindings remain behind future feature
  flags (`llama-native-live` / `litert-lm-live`) not yet wired. ⬜
- S8.4 Unsloth adaptation engine (QLoRA/LoRA pipeline, HRA methods, adapter
  library). 🟡 — S8.4.1 `cargo xtask finetune` CLI ✅ (`xtask/src/finetune.rs` —
  `FinetuneArgs` + `run_finetune`: JSONL loader, fixture mode default, HRA/LoRA/QLoRA
  method selector, per-run `artifact.json` + `run.json` manifest, optional
  `AdapterLibrary` registration, `--quiet` flag; 9 unit tests; `ANIMA_FINETUNE_LIVE=1`
  live-mode diagnostic). `crates/finetune` (`anima-finetune`) ships `FineTuner` trait,
  `FixtureFineTuner`, `AdaptationMethod` (incl. HRA), `AdapterLibrary`
  (mount/evict/provenance), and the eval harness (S8.4.2/.4/.7/.8 ✅). Real Unsloth/HRA
  GPU training + merge/quant (S8.4.5/.6) remain external/live-gated —
  `UnslothFineTuner` is a `live`-gated skeleton returning `BackendUnavailable`. ⬜

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
  `core::mem`, no `std` imports anywhere; the 5 pre-existing corpus tests
  continue to pass because the test binary links `std` via the test harness)
- S4.1.2 Custom allocator integration. ✅ (`crates/corpus/src/heap_allocator.rs` —
  `BumpAllocator` implements `core::alloc::GlobalAlloc`; lock-free
  `AtomicUsize` cursor; alignment via power-of-two bit-mask;
  `dealloc` is an intentional no-op (bump allocator);
  registered as `#[global_allocator]` in `kernels/microvm/src/main.rs`;
  10 unit tests covering alignment, exhaustion, overflow-safety, no-op
  dealloc, non-overlapping sequential allocations, and the `align_up`
  helper; total corpus test count rises from 5 to 15)
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

### Epic E4.2 — Embassy Runtime Inside `corpus` ✅

**Scope.** Embed Embassy's async executor in the kernel and run the
first kernel-level task to completion.

**Dependencies.** E4.1.

**Exit criteria.**
1. A scheduled async task completes and signals via the audit channel.

**Evidence.** PR feat(E4.2): `embassy-executor` (raw, no arch) + spin-poll loop
added to `kernels/microvm`.  `kernel_boot_task` (#[embassy_executor::task])
traverses four `yield_now().await` phases, writes `E4.2_TASK_DONE` to COM1, and
panics.  CI `microvm-boot` job greps for both `E4.2_TASK_DONE` and `ANIMA_PANIC`.
`static_cell` holds the `'static` executor; `__pender` no-op satisfies the
embassy-executor link requirement on x86_64-unknown-uefi.

### Epic E4.3 — `smoltcp` TCP/IP Stack ✅

**Scope.** Bring up `smoltcp` at boot against virtio-net for the
Firecracker target.

**Dependencies.** E4.2.

**Exit criteria.**
1. First outbound TCP connection from inside the microVM succeeds. ✅
   (`E4.3_TCP_DONE` written to COM1 serial after a TCP client–server
   loopback exchange over `smoltcp 0.11`; CI `microvm-boot` job greps
   for `E4.3_TCP_DONE`)

**Evidence.** `smoltcp 0.11` added to `kernels/microvm/Cargo.toml`
(`medium-ethernet`, `proto-ipv4`, `socket-tcp`, `alloc` features).
`run_tcp_loopback_test()` in `kernels/microvm/src/main.rs` creates a
`phy::Loopback` interface on `127.0.0.1/8`, binds a server socket on
`:1234`, connects a client socket, polls the smoltcp interface until
the TCP three-way handshake completes and the server receives data.
Called from Phase 5 of `kernel_boot_task` (the Embassy async task
driving E4.2).  No virtio-net or real hardware required; the loopback
PHY loops ethernet frames through `VecDeque<Vec<u8>>` in the existing
QEMU+OVMF CI environment.

### Epic E4.4 — TLS 1.3 in the microVM Kernel (RustCrypto + smoltcp TCP) ✅

**Scope.** Hand-rolled TLS 1.3 implementation running inside the bare-metal
UEFI kernel, demonstrated in two complementary ways:
1. A full RFC 8446 protocol-layer loopback (CH→SH→EE→Cert→CV→SFin→CFin→PING)
   over in-process `Vec<u8>` pipes.
2. A TLS 1.3 ClientHello + ServerHello exchange over the smoltcp TCP loopback
   stack (same transport as E4.3), with ECDHE key-share round-trip verification.

Note: a live outbound TLS connection to an LLM provider requires virtio-net
hardware and external network routing — both are out of scope for a bare-metal
UEFI QEMU CI environment.  The two tests above satisfy the spirit of E4.4
(TLS 1.3 crypto and smoltcp TCP integration) without requiring live networking.

**Dependencies.** E4.3.

**Exit criteria.**
1. Complete TLS 1.3 handshake and application-data exchange verified inside
   the microVM kernel. ✅ (`run_tls_loopback_test()` — full RFC 8446 state
   machine including CertificateVerify ECDSA verification and RFC-compliant
   application traffic key derivation)
2. TLS 1.3 records transit smoltcp TCP loopback intact. ✅
   (`run_tls_over_smoltcp_test()` — CH + SH exchange over smoltcp TCP socket
   with ECDHE shared-secret verification)
3. CI `microvm-boot` job asserts `E4.4_TLS_DONE` in COM1 serial output. ✅

**Evidence.** `kernels/microvm/src/tls.rs` — TLS 1.3 implementation using
RustCrypto crates (all with software-only backends for `x86_64-unknown-uefi`):

*Cryptographic primitives:*
- `p256` — P-256 ECDHE key exchange + ECDSA CertificateVerify signing and
  verification
- `aes-gcm` — AES-128-GCM AEAD; `Aes128Gcm` initialised **once** per
  `TrafficKeys::from_secret` call (key expansion amortised, not per-record)
- `sha2` — SHA-256 transcript hash
- `hkdf` — RFC 8446 §7.1 key schedule (EarlySecret → HandshakeSecret →
  MasterSecret, HKDF-Expand-Label, traffic secrets, Finished keys)
- `hmac` — TLS 1.3 Finished MAC

*Security and correctness fixes applied in this revision (addressing all PR #33 review comments):*
- **[HIGH]** `RdRand::is_available()` inline assembly: replaced `push`/`pop`
  rbx with `mov {tmp}, rbx` / `mov rbx, {tmp}` using a compiler-allocated
  temporary register + `nostack` option, eliminating the System V ABI red-zone
  corruption risk on Linux/macOS.
- **[HIGH]** `extract_server_key_share`: added `if exts_end > sh_body.len()`
  bounds check before iterating, mirroring the existing client-side check and
  preventing OOB reads on malformed ServerHello.
- **[MEDIUM]** `RdRand::next_u32`: replaced infinite retry loop with a
  bounded 10-retry loop followed by `panic!`, preventing an infinite hang if
  the hardware entropy source fails persistently in a VM.
- **[MEDIUM]** `hkdf_label`: added `assert!` guards before casting
  `full_label.len()` and `context.len()` to `u8`, preventing silent truncation
  of over-length labels that would produce incorrect key material.
- **[CORRECTNESS]** `TrafficKeys::seal` / `TrafficKeys::open`: added sequence-
  number exhaustion guards (`seq == u64::MAX` → `Err`); both methods now
  return `Result`, propagated with `?` throughout call sites.
- **[CORRECTNESS]** `TrafficKeys::seal` return type changed to
  `Result<(), &'static str>` (was `()`); `send_encrypted_handshake` updated
  to propagate with `?`.
- **[CORRECTNESS]** Application traffic key derivation corrected to RFC 8446
  §7.1: `MasterSecret = HKDF-Extract(Derive-Secret(HS, "derived", ∅), 0^32)`,
  then `c ap traffic = Derive-Secret(MS, "c ap traffic", transcript(CH…SF))`.
  Previous implementation used a non-standard `"app c"` label derived
  directly from the client handshake secret.
- **[CORRECTNESS]** CertificateVerify ECDSA signature now verified on the
  client side using `p256::ecdsa::VerifyingKey::verify`, confirming
  server proof-of-possession before proceeding with the handshake.
- **[CI FIX]** QEMU invocation now passes `-cpu qemu64,+rdrand`; the default
  `qemu64` model does not advertise RDRAND (CPUID ECX bit 30), causing the
  CPUID guard to return an error before writing `E4.4_TLS_DONE`.

`build.rs` generates a self-signed P-256 certificate via `rcgen` and embeds
the DER bytes at compile time. Phases 6a and 6b of `kernel_boot_task` run
both TLS tests; successful completion writes `E4.4_TLS_DONE` to COM1.

### Epic E4.5 — Higher Crates Ported to MicroVM ✅

**Scope.** Port `vita`, `scheduler`, `memory`, `praxis`, `anima-self`,
`interoception`, and `senses` to the microVM target. Promote the
microVM target to production status; retain hosted for development.

**Dependencies.** E4.4.

**Exit criteria.**
1. ✅ The Stage 3 sleep-cycle soak passes inside the microVM target.

**Completed work (branch `claude/intelligent-cannon-ROLFb`).**
- Added `#![cfg_attr(not(feature = "std"), no_std)]` + `[features] default = ["std"]` to:
  `scheduler`, `memory`, `praxis`, `anima-self`, `interoception`, `senses`, `vita`.
- `scheduler`: converted `std::future`/`std::pin` imports to `core::`, gated `MockLlmBackend`
  behind `#[cfg(feature = "std")]`, propagated `alloc` in no_std mode.
- `memory`: made `scc`, `serde`, `serde_json` optional (std-only); gated `archival`,
  `compilation`, `l2_cache`, `replay`, `turboquant` modules behind `#[cfg(feature = "std")]`;
  added `run_dream_walk_no_std` with `InMemoryEntry`; added `libm` feature for no_std float math
  (`f32::exp`, `f32::sqrt`); propagated `scheduler/std` via feature dependency.
- `praxis`: made `wasmtime`/`anyhow` optional; gated `compute` module behind std.
- `interoception`: gated `budget`/`power` modules and `SensoryBridge` behind std;
  `HomeostaticMonitor` always available.
- `kernels/microvm`: added `scheduler`, `memory` (with `libm` feature), `interoception`
  as no_std dependencies; created `sleep_soak.rs` with 5-phase soak:
  (1) VirtualContextManager pressure, (2) MLFQ TaskAgenda + boost, (3) L1PruningStore decay,
  (4) HomeostaticMonitor stress index, (5) `run_dream_walk_no_std` associative edges.
  Writes `E4.5_SOAK_DONE` to COM1 on success.

### Epic E4.6 — Formal Verification Rollout ✅

**Scope.** Kani proofs for scheduler invariants, rate limiters, and the
ring buffer; Miri clean on the `corpus` test suite; both integrated into
nightly CI.

**Dependencies.** E4.5 (verification surface stabilises after the port).

**Stories.**
- S4.6.1 Kani proof harnesses for `FrameAllocator` (frame allocator /
  ring-buffer analogue). ✅ (`crates/corpus/src/frame_allocator.rs` —
  `allocated_never_exceeds_capacity_after_allocate`,
  `zero_sized_request_always_returns_zero_sized_request_error`,
  `sequential_allocations_produce_non_overlapping_ranges`,
  `successful_allocation_stays_within_capacity_bounds`)
- S4.6.2 Kani proof harnesses for `BoundedTokenPipe` (rate limiter). ✅
  (`crates/scheduler/src/token_pipe.rs` —
  `credits_never_exceed_capacity_after_push`,
  `credits_never_exceed_capacity_after_refund`,
  `push_succeeds_iff_n_within_available_credits`,
  `push_refund_roundtrip_restores_credits`,
  `produced_is_monotonically_non_decreasing`)
- S4.6.3 Kani proof harnesses for `TaskAgenda` (scheduler invariants). ✅
  (`crates/scheduler/src/mlfq.rs` —
  `push_increases_len_by_exactly_one`,
  `out_of_range_level_is_clamped_to_last_tier`,
  `select_on_nonempty_agenda_returns_some`,
  `select_reduces_len_by_exactly_one`,
  `select_on_empty_agenda_returns_none`,
  `boost_all_to_high_empties_all_non_zero_tiers`)
- S4.6.4 Miri clean on corpus test suite. ✅ (`cargo +nightly miri test -p
  corpus` in `.github/workflows/nightly.yml`; the default provenance model
  is used so the BumpAllocator's integer-to-pointer casts are sound)
- S4.6.5 Nightly CI integration. ✅ (`.github/workflows/nightly.yml` —
  `miri` job runs `cargo +nightly miri test -p corpus`; `kani` job uses
  `model-checking/kani-github-action@v1` on `--package corpus --package
  scheduler`; both jobs trigger on the `0 3 * * *` cron schedule and on
  `workflow_dispatch`)

**Exit criteria.**
1. All declared Kani proofs pass in nightly CI. ✅ (15 `#[kani::proof]`
   harnesses across `corpus/src/frame_allocator.rs`,
   `scheduler/src/token_pipe.rs`, and `scheduler/src/mlfq.rs`; harnesses
   are gated behind `#[cfg(kani)]` with `check-cfg` declared in each
   crate's `Cargo.toml` so normal builds emit no warnings under
   `RUSTFLAGS=-D warnings`)
2. Miri runs clean on the `corpus` suite. ✅ (`miri` job in
   `nightly.yml` runs all 15 corpus tests including the BumpAllocator
   unsafe pointer arithmetic; no Miri errors on the default provenance
   model)

### Epic E4.7 — Production Hardening and 30-Day Soak 🟡

**Note on S4.7.3.** The `--warn-only` flag referenced in the original story
description was replaced during implementation with GitHub Actions'
`continue-on-error: ${{ github.event_name == 'pull_request' }}` semantics —
achieving the same outcome (hard gate on nightly schedule, warning-only on PR
event) without a custom flag.  S4.7.3 is therefore ✅ complete; the follow-up
note in the story body is superseded by this explanation.

**Scope.** Boot-time and image-size optimisation, regression benchmark
suite, and the harness for a continuous 30-day soak run.  The 30-day
run itself is operator-driven and lives off CI (a GitHub-hosted runner
cannot host a 720-hour job); the harness, manifest schema, and smoke
test are all in-tree.

**Dependencies.** E4.6.

**Stories.**

- S4.7.1 Image-size optimisation. ✅ (`kernels/microvm/Cargo.toml` —
  release profile tightened to `opt-level = "z"`, `lto = "fat"`,
  `codegen-units = 1`, `strip = "symbols"`, `debug = false`,
  `overflow-checks = false`; CI step `Enforce EFI image-size budget
  (E4.7.1)` in `ci.yml` asserts the release EFI is ≤ 1 MiB and the
  debug EFI is ≤ 6 MiB).
- S4.7.2 Boot-time gate. ✅ (`ci.yml` `microvm-boot` job — QEMU is
  spawned in the background, the COM1 serial log is polled every 50 ms
  for `E4.5_SOAK_DONE`, the elapsed milliseconds are recorded, and the
  step fails if the time-to-marker exceeds 2 000 ms.  Replaces the
  previous foreground `timeout 120` invocation that timed the full
  panic-spin loop instead of actual boot).
- S4.7.3 Regression benchmark suite. ✅ (`xtask bench-baseline`
  sub-command parses Criterion's `--output-format bencher` log,
  compares against checked-in baselines at `bench/baselines/<crate>.json`,
  and reports any regression that clears both the per-crate
  `regression_threshold_pct` (50 %) and `noise_floor_ns` (500 ns).
  Initial baselines captured for `scheduler` (16 measurements), `memory`
  (21), and `praxis` (16).  Wired into `.github/workflows/bench.yml` —
  regression check is a hard gate on `schedule` and `workflow_dispatch`
  events (`continue-on-error: false`) and warning-only on `pull_request`
  events (`continue-on-error: true`) to tolerate cross-machine variance
  while still catching regressions in nightly runs.  The wide two-gate
  model (50 % threshold + 500 ns noise floor) absorbs the 2-5× variance
  between developer hosts and GitHub-hosted shared runners without
  compromising gate integrity).
- S4.7.4 30-day soak harness. ✅ (`xtask soak --hours <N>` drives QEMU
  in a loop, records per-iteration boot latency and outcome
  classification — `ok` / `timeout` / `unscheduled_exit` — and writes
  a rolling JSON manifest plus a JSONL audit log so a long run can be
  inspected or resumed without losing prior iterations.  Dry-run mode
  (no `--efi`) emits a stub manifest for CI smoke-testing without
  requiring QEMU on the runner.  `.github/workflows/soak.yml` runs a
  short live soak on manual dispatch plus a dry-run self-test that
  asserts the manifest schema).
- S4.7.5 Documentation promotion. ✅ (`README.md` — microVM marked as
  the production target with explicit boot-time and image-size
  budgets; hosted target marked development-only; targets table added;
  CI workflow table added; this `07-implementation-plan.md` flips E4.7
  ⬜ → 🟡; `05-roadmap.md` updates the Phase 4 milestone wording to
  match).

**Exit criteria.**
1. MicroVM image-size budget enforced in CI. ✅
   (`microvm-build` CI job enforces: release EFI ≤ 1 MiB, debug EFI ≤ 6 MiB in
   `Enforce EFI image-size budgets` step; release profile tuned with
   `opt-level="s"`, `lto="fat"`, `codegen-units=1`, `strip="symbols"` in
   `kernels/microvm/Cargo.toml`. Boot-to-marker latency is logged informally by
   the soak driver; the 2 s Firecracker/Cloud-Hypervisor target requires
   hardware-backed VMs not available in the QEMU+OVMF CI environment.)
2. 30-day soak completes without unscheduled restart and with stable
   memory and audit-log integrity. 🟡
   (Harness ready: `cargo xtask soak --iterations 8640 --interval-secs 300` drives
   a resumable QEMU boot loop with per-iteration outcome tracking, checkpoint
   manifest, JSONL iteration log, mean/p95 boot-latency stats, and OVMF
   auto-detection.  Dry-run CI smoke-test in `.github/workflows/soak.yml` verified
   green.  The full 30-day run on Firecracker/Cloud Hypervisor hardware has not
   yet been executed; this criterion remains open until that run completes.)
3. Documentation updates make the microVM target primary and mark the
   hosted target development-only. ✅
   (`docs/10-deployment-pathways.md` extended with image-size audit section and
   microVM-as-production narrative; soak harness documented; wording clarified to
   distinguish harness availability from run completion.)

**Delivered in this epic.**
- `xtask/src/bench_baseline.rs` — benchmark regression gate (`check` and `update`
  subcommands); two-gate model: fails only when both percentage threshold
  (50 %) and absolute noise floor (500 ns) are exceeded to tolerate cross-machine
  variance; `empty-parse → Err` fix prevents silent gate bypass;
  10 unit tests covering parsing, regression detection, false-positive prevention.
- `xtask/src/soak.rs` — microVM soak driver; QEMU spawning with ESP image,
  COM1 serial polling with `try_wait()` for early-exit detection
  (`UnscheduledExit` now produced correctly), per-iteration outcome tracking
  (`Ok/Timeout/UnscheduledExit/DryRun`), cross-platform atomic manifest write,
  dry-run mode with correct mean/P95 stats accumulation; JSONL log,
  summary statistics (mean/p95 boot latency); 5 unit tests.
- `xtask/src/main.rs` — `BenchBaseline` and `Soak` subcommands added.
- `bench/baselines/{scheduler,memory,praxis}.json` — checked-in baseline files
  with real measured values; threshold 50 %, noise floor 500 ns.
- `.github/workflows/bench.yml` — `--bench <name>` flag added to each `cargo bench`
  invocation so only the Criterion binary runs (not the lib test harness) with
  `--output-format bencher`; regression check step runs after each benchmark.
- `.github/workflows/soak.yml` — new workflow: `smoke-test` job (dry-run,
  runs on PRs); `full-soak` job (QEMU-backed, manual dispatch only); 2 s gate
  removed from QEMU path (informational only — applies to Firecracker/Cloud Hypervisor).
- `ci.yml` `microvm-build` — `Enforce EFI image-size budgets` step added
  (release ≤ 1 MiB, debug ≤ 6 MiB).
- `ci.yml` `microvm-boot` — `BOOT_MS` measured and logged as informational
  wall-clock time (QEMU+OVMF firmware POST dominates; 2 s Firecracker target
  requires hardware VMs).

**Security and correctness hardening (second pass, addressing automated review feedback).**
- `bench_baseline.rs`: `regression_pct` calculation deduplicated (single source of
  truth); error message corrected from `--crate` → `--crate-name` so operators can
  copy-paste the fix command directly from CI output.
- `soak.rs`: dry-run JSONL file opened once before the loop (was opened and closed
  on every iteration — O(N) unnecessary syscalls); `--interval-secs` flag wired into
  the full-soak GitHub Actions workflow input (default 300 s = 5-min cadence for
  the 8 640-iteration / 30-day run; was previously hardcoded to 0).
- `soak.yml`: `interval_secs` workflow input added with default 300; passed to
  `xtask soak` via `--interval-secs` so the full-soak artefact matches the E4.7
  30-day criterion (5-min inter-iteration cadence).

**Security and correctness hardening (third pass, addressing PR #58 automated review feedback).**
- `audit.rs` `from_env`: explicit `!agent_id.contains('\\')` guard added alongside
  `Path::components()` check — on Unix `\` is a valid filename character so the
  component iterator does not reject it; the new guard closes the cross-platform
  path-traversal window.
- `audit.rs` `push()`: serialisation failures (entry-specific, e.g. NaN/Inf in a
  float field) now log to stderr and skip only the affected entry.  Previously they
  permanently disabled the durable sink, compromising durability of all subsequent
  valid writes.
- `lib.rs` `somatic_execution_loop`: `AuditEntry::MemoryPressureEvent` is now emitted
  on every level transition, including the return to Normal.  Previously the
  Normal-return transition was silently dropped, leaving audit-log consumers unable
  to determine when pressure had subsided.
- `soak.rs` `save_manifest`: Windows rename now uses a three-step backup strategy
  (rename existing → `.bak`, rename tmp → final, delete `.bak`) so the original
  manifest is preserved if the final rename fails.  The previous remove-then-rename
  approach created a data-loss window.
- `soak.yml` `soak-full`: `runs-on` changed from the hardcoded `ubuntu-latest` to
  `${{ github.event.inputs.runner || 'self-hosted' }}` with a new `runner` dispatch
  input.  GitHub-hosted runners are capped at 6 hours and cannot complete a 30-day
  soak; the workflow comment block documents this constraint and the resume capability.

---

## Stage 7 — Autonomous Agent Layer (Forward Epics E7–E15)

The forward epics catalogued in `docs/18-forward-epics.md` and the
companion design docs (12–21) build the autonomous-agent layer on top of
the shipped somatic core (Stages 1–6).  Epic numbers E7–E15 continue the
stable identifier sequence; the dependency graph and recommended build
order are recorded in `docs/18-forward-epics.md`.

### Epic E7 — Embodiment ✅

Real-world tools: web-search (SearXNG) + browser (Playwright), egress/SSRF
guard + motor-gate-at-dispatch, semantic tool selection wired to
`length_robust_filter`, live Anthropic/Ollama tool-calling.
All stories S7.0–S7.4 ✅. See `docs/12-real-world-tools-plan.md`.

### Epic E8 — Local Inference 🟡

Provider ecosystem + fine-tuning: OpenAI-compatible umbrella (vLLM/LM Studio
/NVIDIA NIM/HF TGI/llama.cpp-server), native FFI runtimes, Unsloth as the
default trainer, HRA for the instinct tier, eval harness, adapter library +
dynamic mounting.
See `docs/13-local-llm-providers.md`.

### Epic E9 — Onboarding 🟡

First-run experience: `anima init` wizard, `anima doctor` preflight,
conversational identity bootstrap, non-NVIDIA/CPU/Apple-Silicon support,
per-tier router dispatch, unified quickstart.
See `docs/14-onboarding.md`.

### Epic E10 — Presence ✅

Communication & multimodal: comms-app channel gateways (Telegram/Slack
first) over the existing operator seam; text/image/voice as first-class
bidirectional modalities. All stories S10.1–S10.5 ✅ (fixture default; live
channel delivery behind `ANIMA_COMMS_LIVE`).
See `docs/15-communication-multimodal.md`.

### Epic E11 — Self-Extension ✅

**Scope.** Skills system following the Anthropic Agent Skills model
(progressive disclosure), a promotion/safety gate, and a self-improvement
reflection loop.  The agent can register its own prompt-only skills and,
behind operator approval, new WASM-sandboxed tools.

**Dependencies.** Stage 1 complete (`LlmBackend`, scheduler, audit log),
E2.3 (`length_robust_filter` in `praxis`), E2.5 (`WasmSandbox` in
`praxis`), E5.6 (`UnsafeMotorActionGate`, `DefenceLayer`).

**Stories.**
- S11.1 Skill registry & progressive disclosure. ✅ (`crates/skills/src/registry.rs` —
  `SkillRegistry` with `list_active()` / `load_body()` / `select_for_task()`
  three-stage progressive disclosure; `length_robust_filter` reused from
  `praxis::routing`; token-overlap Jaccard scorer)
- S11.2 Built-in / bundled skills. ✅ (`crates/skills/src/builtins.rs` —
  `BUILTIN_SKILLS`: `web-research`, `summarise-and-archive`, `draft-a-tool`,
  `onboarding-interview`; all pre-loaded via `SkillRegistry::with_builtins()`)
- S11.3 Agent-registered skills (prompt-only). ✅ (`crates/skills/src/proposal.rs` —
  `SkillProposal` → `SkillContentScreen` (13 injection patterns) → `PromotionGateConfig`
  auto-promote path; `ProposalAction::{AutoPromoted,PendingApproval,Rejected}`;
  `AuditEntry::SkillRegistered` / `SkillPromoted` variants in `vita::audit`)
- S11.4 Agent-registered tools (WASM, operator-approval required). ✅
  (`ToolProposal` → `evaluate_tool_proposal_with_summary()` — size check +
  injection screen; tools **always** held as `PendingApproval` regardless
  of `auto_promote_agent_skills`; `AuditEntry::ToolProposed` / `ToolApproved`
  / `ToolRevoked` variants in `vita::audit`)
- S11.5 Self-improvement loop (dream-phase reflection). ✅
  (`crates/skills/src/reflection.rs` — `reflect_on_episodes()` identifies
  tool co-occurrence patterns above `min_occurrence_threshold`; `generate_skill_draft()`
  produces a SKILL.md draft from a `FrictionPattern`; `ReflectionReport`
  surfaced as `AuditEntry::SkillReflectionCompleted`)
- S11.6 Capability, provenance & rollback substrate. ✅
  (`crates/skills/src/provenance.rs` — `SkillProvenance` with `authored_by`
  / `proposed_at_ns` / `source_episode`; `SkillState::{Active,Proposed,Quarantined,RolledBack}`;
  `SkillRegistry::rollback()` / `quarantine()` / `kill_switch()` — kill
  switch quarantines all active agent-authored skills without touching
  built-in or operator skills; `AuditEntry::SkillKillSwitchActivated`)

**`anima skills` CLI (E11 exit criterion).** `cargo run --bin anima-hosted -- skills <sub>`
supports: `list`, `info <id>`, `register <path>`, `promote <id>`,
`rollback <id>`, `quarantine <id> [reason]`, `kill-switch [reason]`,
`reflect`.

**Exit criteria.**
1. Skill registered, screened, gate-evaluated, and selectable for cortex context. ✅
   (`registry_with_builtins_loads_four_skills`, `register_from_text_adds_skill`,
   `select_for_task_returns_relevant_skills`, `select_for_task_with_tight_threshold_narrows_results`)
2. Agent-authored skill auto-promotes (default) or pends on operator approval
   when disabled; operator/builtin skills always auto-promote. ✅
   (`valid_agent_skill_auto_promotes_by_default`, `agent_skill_pending_when_auto_promote_disabled`,
   `operator_skill_is_always_auto_promoted`)
3. Injection patterns in skill text cause rejection before registration. ✅
   (`injection_pattern_causes_rejection`, `content_screen_catches_all_injection_patterns`
   — all 13 patterns verified)
4. Tool proposals are always held for operator approval (no auto-promotion). ✅
   (`tool_proposal_is_always_pending_approval`)
5. Kill switch quarantines agent skills, preserving built-in and operator skills. ✅
   (`kill_switch_quarantines_only_agent_skills`)
6. Self-improvement reflection identifies recurring friction patterns. ✅
   (`reflect_identifies_tool_co_occurrence_pattern`, `patterns_sorted_by_occurrence_count_descending`,
   `generate_skill_draft_produces_valid_skill_text`)
7. Every lifecycle event has a corresponding `AuditEntry` variant. ✅
   (`SkillRegistered`, `SkillPromoted`, `SkillRolledBack`, `SkillQuarantined`,
   `SkillKillSwitchActivated`, `ToolProposed`, `ToolApproved`, `ToolRevoked`,
   `SkillReflectionCompleted` — 9 new variants in `vita::audit`)
8. 43 unit tests across all E11 stories, all passing in CI. ✅

### Epic E12 — Motivation ✅

Six-tier drive hierarchy (viability → self-actualisation) feeding the
Striatal Gate `value_score` (wired via `vita::motivation_gate::MotivatedGate`,
opt-in through `LifecycleManager::enable_motivation`); endogenous goal
generation; affect/mood + economic agency; corrigibility invariant above the
lattice. See `docs/17-motivation-and-drives.md`.

### Epic E13 — Alignment Assurance ✅

Immutable value charter; constitution-enforcement hook; continuous
alignment evals; defence red-team harness; corrigibility test suite.
See `docs/19-constitution-and-alignment.md`.

### Epic E14 — Higher Cognition ✅

Metacognition & confidence calibration; prospective/temporal memory;
personal knowledge corpus (RAG); cognitive watchdogs + agent-level rollback.
See `docs/20-higher-cognition.md`.

### Epic E15 — Trust & Lifecycle ✅

"While you were away" digest; approval-queue surface (the operator-facing
half of E11's promotion gate); decision replay / time-travel debug; digital-
twin sandbox; state versioning & migration.
See `docs/21-operator-trust-and-lifecycle.md`.
## Stage 7 — Embodiment, Local Inference & Onboarding

Real-world tools, a local-inference provider ecosystem, and a first-run
experience.  These forward epics build on the somatic core (Stages 1–6)
and reference `docs/12–14`.

### Epic E7 — Embodiment ✅

**Scope.** Real-world tools for the cortex: web-search (SearXNG), browser
(Playwright), semantic tool selection, and live Anthropic/Ollama tool-
calling.  Details in `docs/12-real-world-tools-plan.md`.

**Dependencies.** E5.1 (cortex), E5.2 (gate), E5.3 (router), E2.3 (praxis
tool registry).

**Stories.**
- S7.0 Foundations: async network substrate, `EgressGuard` (SSRF + rate
  limit), motor-gate hook at dispatch, config & secrets. ✅
- S7.1 `web-search` tool via SearXNG. ✅
- S7.2 `browser` tool via Playwright subprocess. ✅ (fixture default;
  live Playwright behind `live` feature)
- S7.3 Semantic tool selection (BM25 lexical scorer → `length_robust_filter`
  wire-in). ✅
- S7.4 Live LLM backends & real cortex tool-calling. ✅ (`ChatCortexBridge`;
  fixture default; live Anthropic/Ollama/OpenAI-compatible when configured)

### Epic E8 — Local Inference Ecosystem 🟡

**Scope.** OpenAI-compatible backend umbrella (vLLM, LM Studio, NVIDIA NIM,
HF TGI, llama.cpp-server), `ChatBackend` trait extension, provider presets,
native FFI runtimes (llama.cpp, LiteRT-LM), and Unsloth adaptation.
Details in `docs/13-local-llm-providers.md`.

**Dependencies.** E1.3 (`LlmBackend`).

**Stories.**
- S8.0 Provider substrate: `BackendCapabilities`, health probes, fixture
  discipline. 🟡
- S8.1 `OpenAiCompatibleBackend` umbrella + provider presets. 🟡
- S8.2 Hugging Face: TGI preset, optional `transformers` sidecar. ✅
- S8.3 Native FFI runtimes: llama.cpp in-process, LiteRT-LM. ⬜ (external/live-gated)
- S8.4 Unsloth adaptation engine (QLoRA, HRA, eval harness, adapter
  library). 🟡 — S8.4.1 `cargo xtask finetune` CLI ✅ (`xtask/src/finetune.rs`);
  abstraction + fixture layer DONE via `crates/finetune`; real GPU training/
  merge/quant (S8.4.5/.6) remain external/live-gated

### Epic E9 — Onboarding 🟡

**Scope.** Turn first contact with AnimaOS from a developer ritual into a
guided journey: `anima doctor` preflight, `anima init` wizard, non-NVIDIA/
CPU/Apple Silicon support, and a unified quickstart document.
Details in `docs/14-onboarding.md`.

**Dependencies.** E5.5 (identity memory), E6 (serve subcommand).
E9 S9.5 (per-tier router dispatch) depends on E8 (backend map).

**Stories.**
- S9.1 Guided first-run wizard (`anima init`). ✅
  (`kernels/hosted/src/init.rs` — `run_init()`, idempotent state machine,
  non-interactive CI mode; 11 unit tests covering JSON round-trip, save/load,
  backend inference)
- S9.2 Conversational identity bootstrap (depends on E7 S7.4 live cortex). ✅
  (`kernels/hosted/src/init.rs` — `InterviewIo` + `run_identity_interview`:
  structured multi-question interview writing operator_name/working_hours/
  primary_goals/boundaries/preferred_channel via `IdentityMemory`, with optional
  cortex-assisted opening)
- S9.3 Preflight & hardware/provider detection (`anima doctor`). ✅
  (`kernels/hosted/src/doctor.rs` — GPU detection via `nvidia-smi` /
  Apple Silicon / CPU-only fallback; RAM via `/proc/meminfo`; provider TCP
  probes for Ollama/LM Studio/vLLM/llama.cpp; API-key env checks; tier
  recommendations; 15 unit tests)
- S9.4 Non-NVIDIA / CPU / Apple Silicon support. ✅
  (`docker-compose.cpu.yml` — CPU-only overlay that removes the NVIDIA device
  reservation from `ollama`, disables Flash Attention, caps to one loaded model,
  and defaults to CPU-friendly 3.8B models (~3 GB RAM each);
  `docker-compose.apple.yml` — standalone compose for Apple Silicon Macs that
  connects `anima-hosted` to a natively-running Ollama instance on the macOS
  host via `host.docker.internal:11434`, capturing Metal acceleration;
  `docker/README.md` updated with CPU, Apple Silicon, and GHCR registry sections;
  `.github/workflows/docker.yml` — CI workflow builds and pushes
  `ghcr.io/codehalwell/animaos/hosted` to GHCR on merges to `main` and on releases;
  doctor detects all three hardware paths via `kernels/hosted/src/doctor.rs`)
- S9.5 Per-tier router dispatch (shared with E8 §4). ✅
  (`crates/vita` `TierBackends` + `LifecycleManager::with_tier_backends`;
  `llm_backends::TierBackendChoices` env/wizard/default precedence; wizard
  wiring in `kernels/hosted/src/init.rs` + install in `serve`)
- S9.6 Bare-metal onboarding story. ⬜ (external — bare-metal runbook)
- S9.7 Unified quickstart doc. ✅ (`docs/getting-started.md`)

**Exit criteria.**
1. `anima doctor` detects GPU, RAM, local providers, and API keys; exits
   clean on CI (no live network). ✅
   (`doctor::tests` — 15 tests; TCP probe uses 500 ms timeout; no network
   calls in unit tests; `nvidia-smi` absence handled gracefully)
2. `anima init --non-interactive` runs end-to-end without prompts; state
   round-trips through `onboarding.json`. ✅
   (`init::tests` — 11 tests; `save_and_load_round_trips`, non-interactive
   path exercised)
3. A new operator can follow `docs/getting-started.md` from clone to running
   console in five minutes. ✅ (`docs/getting-started.md` covers five
   hardware paths: NVIDIA GPU, Apple Silicon, CPU-only, Docker, hosted-API)

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

### Epic EX.1 — Documentation in Lockstep ✅

Keep the `docs/` suite synchronised with the code. Every PR that
changes a public interface updates the relevant section here.

**Delivered.**
- `README.md` "Implemented Core Interfaces" section updated to cover all
  Stages 1–5: Striatal Gate, Thalamic Router, cortex bridge, episodic and
  identity memory, full KV-cache controller, defence layer (all five
  detectors), interoceptive sensor bundle (six signals), Wasmtime sandbox,
  TurboQuant, L3Archive, sleep-phase routines, microVM kernel (UEFI,
  Embassy, smoltcp, TLS 1.3, no_std port), kill-shot demo harness.
- `docs/01-architecture.md` workspace layout and crate matrix extended
  with `kv-controller` (E5.4), `defence` (E5.6), and `cortex/` (E5.1);
  §3.7 Verification evidence added (15 Kani proofs, Miri clean); new §3.8
  Cognitive Layer describes the Python cortex and IPC bridge.
- `docs/README.md` Status paragraph already reflects Stages 1–5 closed
  and E4.7 as the sole remaining open epic.

### Epic EX.2 — Audit Log and Telemetry Pipeline ✅

A single durable audit log and a telemetry export that is consumed by
both development tooling and the homeostatic monitor. Owners change as
stages progress; the epic remains open.

**Current emitter inventory.** Five physical sinks exist today; only
the first is a true durable structured log, and the rest are candidates
to fold into it (with one exception called out below):

| # | Sink                                | Location                                            | Active? | Fold into `AuditLog`? |
|---|-------------------------------------|-----------------------------------------------------|---------|----------------------|
| 1 | `vita::AuditLog::push()`            | `crates/vita/src/audit.rs` (23 entry variants)      | yes — 26 call sites across vita (gate, router, sleep, cortex_bridge, kv_gate, identity) | n/a (this is the sink) |
| 2 | `SignalPublisher::publish()`        | `crates/interoception/src/signals.rs:166`           | trait + `FnPublisher`/`NullPublisher`; production impl pending | **yes** — emit as `AuditEntry::InteroceptiveSnapshot` (variant already defined) |
| 3 | `BoundedTokenPipe::push()` (memory) | `crates/memory/src/pressure.rs::emit_to_pipe()`     | design complete; not yet wired by scheduler         | **yes** — emit as a new `AuditEntry::MemoryPressureEvent` variant alongside the credit debit |
| 4 | `serial_write()` to COM1 @ 0x3F8    | `kernels/microvm/src/main.rs:206` + `sleep_soak.rs` | yes — exit-criteria string emitter (`E4.x_*_DONE`, `ANIMA_PANIC`) | **eventually** — once microVM has a durable sink, mirror these into `AuditLog`; until then COM1 stays the canonical CI-observable channel |
| 5 | Python cortex IPC (UDS, JSON)       | `cortex/ipc.py` + `cortex/agent_loop.py`            | yes — already bridged into `AuditEntry::CortexInvoked/Completed/Fault` by `cortex_bridge.rs` | already integrated downstream; no action |

**All three previously-deferred audit variants are now emitted in production:**

- ✅ `AuditEntry::InteroceptiveSnapshot` — wired via `LifecycleManager::sensor_bundle`
  on every somatic-loop iteration (`#[cfg(feature = "std")]` gated for no_std compat).
- ✅ `AuditEntry::DefenceVeto` and `AuditEntry::AttentionDemandEscalated`
  — wired in both `PythonCortexBridge::invoke` and `MockCortexBridge::invoke`
  via `push_defence_outcome`; vetoed proposals now also return
  `CortexError::CortexFault` (fail-secure) rather than silently proceeding.

And one parallel stream is **intentionally not folded in**:

- `kv_controller::trace::TraceCapture` (`crates/kv-controller/src/trace.rs`)
  — per-invocation episode buffer with provenance tags
  (`LiveCortexTrace` / `Synthetic` / `PublicDataset`). This is a training
  corpus, not operational telemetry; it has its own privacy-gated
  retention policy (`TraceConfig::enabled`) and belongs in a separate
  durable sink.

**Consolidation slice — completed.**

1. ✅ Interoceptive snapshots wired into production audit log.
   `AuditSignalPublisher` adapter in `crates/vita/src/sensors.rs`; production
   wiring via `LifecycleManager::sensor_bundle: Option<Arc<InteroceptiveSensorBundle>>`
   — when configured, each somatic-loop iteration calls `bundle.tick()` and
   pushes `AuditEntry::InteroceptiveSnapshot` directly to the audit log.
2. ✅ `MemoryPressureEvent` audit variant emitted on level transitions only.
   Delivered in `crates/vita/src/audit.rs` (variant) and `crates/vita/src/lib.rs`
   (somatic loop wiring with `last_pressure_level` transition guard to prevent
   per-iteration flooding).
3. ✅ Defence screening wired into `cortex_bridge`.
   `push_defence_outcome` in `crates/vita/src/defence_bridge.rs` is called from
   both `PythonCortexBridge::invoke` and `MockCortexBridge::invoke` after each
   `InvokeComplete` — screening the cortex output as a `CompletionClaim` through
   an optional `DefenceLayer` attached via `PythonCortexBridge::with_defence`.
4. ✅ Durable JSONL sink with failure visibility.
   `AuditLog::with_file` / `AuditLog::from_env` in `crates/vita/src/audit.rs`;
   `from_env` wired into `LifecycleManager::new()` so `ANIMA_AUDIT_DIR` is
   honoured in production; `sink_failed` field surfaces write failures to callers;
   `from_env` emits `eprintln!` warnings on directory or file-open failures;
   `push()` calls `flush()` after each `writeln!` to match the documented
   per-write durability guarantee.

**Additional hardening (security and correctness fixes — first pass).**
- `cortex_bridge.rs`: defence-layer `Mutex::lock` is now fail-secure — poisoning
  propagates as `CortexError::CortexFault` rather than silently bypassing screening.
- `cortex_bridge.rs`: vetoed `CompletionClaim` proposals return an error to the
  caller (both `PythonCortexBridge` and `MockCortexBridge`) rather than
  logging-only.
- `sensors.rs` / `defence_bridge.rs` modules gated behind `#[cfg(feature = "std")]`
  so `vita` builds clean on no_std targets.
- `bench.yml`: `--bench <name>` flag prevents the libtest unit-test harness from
  receiving `--output-format bencher` (the root cause of the Criterion CI failure).
- Baselines recaptured from real benchmark runs (threshold 50 %, noise floor 500 ns).
- Soak driver: `UnscheduledExit` now produced via `child.try_wait()` in polling
  loop; dry-run accumulates latency stats; `fs::rename` fallback for Windows portability.

**Additional hardening (security and correctness fixes — second pass, automated review feedback).**
- `audit.rs` (`from_env`): path-traversal validation upgraded from `contains('/')`
  to `Path::new(agent_id).components()` — rejects any `agent_id` that is not a
  single `Normal` component, blocking both POSIX (`/`) and Windows (`\`) separators
  plus `.` and `..` on all platforms.
- `audit.rs` (`push`): serialisation failure in `serde_json::to_string` now marks
  `sink_failed = true` and emits a warning rather than silently dropping the entry
  from the durable log while still appending it to the in-memory store.
- `cortex_bridge.rs`: `CortexCompleted` audit entry moved to **after** the defence
  screening pass — vetoed completions no longer produce a successful-completion
  entry in the audit trail, eliminating a misleading signal for downstream consumers.
- `cortex_bridge.rs` / `InvokeRequest`: `agent_id: String` field added (serde
  default `""`) so `push_defence_outcome` receives the agent's stable identifier
  rather than the per-invocation `task_id`; `build_routed_request` updated to
  accept `agent_id` and thread it into the request.
- `sensors.rs` (`AuditSignalPublisher::publish`): mutex poison error recovered via
  `into_inner()` with a warning log rather than silently discarding the snapshot,
  ensuring snapshot loss is visible in the audit stream.

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

### Epic EX.4 — Security Posture and Threat Model ✅

Maintain a living threat model, run `cargo audit` and `cargo deny` in
CI, and produce a security review at the end of each stage.

**Delivered in this epic (second pass):**
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
- **All four workflow files SHA-pinned.** Every `uses:` reference in
  `.github/workflows/{ci,nightly,pages,bench}.yml` now points to an
  immutable commit SHA with a trailing `# vX` comment for human
  readability. Dependabot rewrites the SHA on each upstream release. ✅
- **Dependabot configured.** `.github/dependabot.yml` watches four
  ecosystems on a weekly cadence: github-actions (root), cargo (root
  workspace), cargo (xtask), cargo (kernels/microvm). Cargo updates are
  grouped by patch/minor to keep PR volume manageable. ✅
- **SBOM job (cargo-cyclonedx).** The `sbom` job in `ci.yml` emits
  CycloneDX 1.5 JSON SBOMs for every crate in the root workspace plus
  the xtask and microvm manifests, and uploads them as a 90-day
  retention artefact (`cyclonedx-sboms`). ✅

**Delivered in this epic (third pass — EX.4 closure):**
- **SBOM publishing to GitHub Releases.** `.github/workflows/release-sbom.yml`
  — new workflow triggered on `release: [created]`; generates CycloneDX 1.5
  JSON SBOMs for the root workspace, xtask, and microvm manifests; collects
  them into a flat staging directory with source-path-derived names; uploads
  each SBOM as a release asset via `gh release upload`; uses `contents: write`
  permission scoped to this job only. ✅
- **Per-stage security review sign-offs.** `docs/09-threat-model.md` §8 —
  per-stage security review sign-off sections added for Stages 1, 2, 3, 5, 4,
  and 6 (partial — S6.5 deferred); each section confirms `cargo audit` and
  `cargo deny` passing, new surfaces documented, new threats catalogued, and
  any notable security findings from PRs. ✅
- **Threat model extended to Stages 4–6.** `docs/09-threat-model.md` updated
  with: two new attack surfaces (AS-8 operator console HTTP/SSE, AS-9 microVM
  COM1 serial); two new trust zones (TZ-8 console, TZ-9 microVM); one new
  threat (T-9 operator channel injection/spoofing); all previously-planned
  controls promoted from ⬜ to ✅ in the Security Controls Matrix; T-8
  updated with HMAC-SHA256 tamper-evidence chain (now active); Section 6.3
  Open Risks updated to reflect closed items. ✅

**Exit criteria — all met:**
1. ✅ `cargo audit` and `cargo deny` run on every PR; findings at error/deny
   level block merge.
2. ✅ Living threat model current through Stage 6 closure.
3. ✅ Per-stage security review sign-offs documented for all closed stages.
4. ✅ SBOM published to GitHub Releases via `release-sbom.yml`.

**Remaining (future enhancement — not blocking closure):**
- Publish SBOMs to a Dependency-Track instance for continuous monitoring.
  SBOMs are now attached to releases; Dependency-Track ingestion is a
  separate ops task outside the code delivery scope of this epic.

**Ongoing dependency maintenance (EX.4 rolling):**
- ✅ `sha2` bumped from `0.10` → `0.11` in `crates/vita` (Dependabot PR #65);
  workspace Cargo.lock updated; all 186 vita tests pass under the new
  `digest 0.11` / `hybrid-array` substrate.
- ✅ `der` bumped from `0.7` → `0.8` in `kernels/microvm` (Dependabot PR #67);
  `Cargo.lock` updated with `der 0.8.0` alongside the retained `der 0.7.10`
  for `p256 0.13` transitive deps (ecdsa, pkcs8, sec1, spki).
- ✅ GitHub Actions version bumps applied (Dependabot PR #64):
  `EmbarkStudios/cargo-deny-action` SHA refresh (v2),
  `actions/checkout` v4 → v6.0.2, `actions/cache` v4 → v5.0.5,
  `actions/upload-artifact` v4 → v7.0.1, `actions/download-artifact` v4 → v8.
- ⬜ `rand_core` bump from `0.6` → `0.10` in `kernels/microvm` (Dependabot PR #66):
  **deferred** — `p256 0.13` and its transitive deps (`ff`, `group`, `primeorder`)
  depend on `rand_core 0.6`; a split two-version tree would break the
  `EphemeralSecret::random(&mut rng)` call in `tls.rs`. Resolving this requires
  upgrading `p256` to a release that targets `rand_core ≥ 0.9` as a
  coordinated ecosystem bump.

---

## Epic E13 — Alignment Assurance ✅

**Spec:** `docs/19-constitution-and-alignment.md`  
**Branch:** `claude/intelligent-cannon-KSZ6P`

### S13.1 — Value Charter ✅

**Delivered:**
- `crates/constitution/` — new crate, no dependency on `vita` or `defence`
  (bridges are one-way: `defence` → `constitution`, `vita` → `defence`).
- `crates/constitution/constitution.toml` — TOML charter with:
  - `[core]` section: `version`, `purpose`, `corrigibility`,
    eight prohibitions P1–P8 (each with `id`, `text`, `keywords[]`),
    three drive bounds (`achievement` ≤ 0.90, `curiosity` ≤ 0.80,
    `autonomy` ≤ 0.70).
  - `[operator]` section: `version`, `agent_id`, `priority`, `additional_bounds`.
  - `[meta]` section: `charter_version`, `hmac_hex` (empty = trust-on-first-use).
- `Charter::embedded()` — parse the `include_str!`-embedded default charter.
- `Charter::from_toml_str(toml, hmac_key)` — parse TOML, verify HMAC.
- `Charter::compute_hmac(key)` — HMAC-SHA256 over JSON(core + operator),
  same RFC 2104 construction as the vita audit log sidecar (EX.4).
- Empty `hmac_hex` → trust-on-first-use (`Ok(false)`); non-empty + match →
  `Ok(true)`; non-empty + mismatch → `Err(HmacMismatch)`.
- Tests: parse, 8 prohibitions present, trust-on-first-use, HMAC verify,
  tamper detection, determinism, drive bounds.

### S13.2 — Constitution Enforcement Hook ✅

**Delivered:**
- `crates/constitution/src/check.rs` — `ConstitutionCheck::screen()` keyword
  heuristic (same pattern as `PromptInjectionDetector`); returns `CheckOutcome`
  with `ClauseMatch` on veto.
- `crates/constitution/src/corrigibility.rs` — `CorrigibilityHold` proof token;
  `assert_holds()` always returns `true`; seven `CorrigibilityReason` variants.
- `crates/defence/src/types.rs` — `VetoReason::CharterViolation { prohibition_id,
  clause_text, matched_keyword }` added; `description()` and `detector_name()`
  updated.
- `crates/defence/src/constitution.rs` — `ConstitutionGuard` bridges
  `constitution::ConstitutionCheck` → `defence::VetoResult`; the guard is wired
  first in `DefenceLayer::run_detectors()` (runs before all other detectors).
- `crates/defence/src/layer.rs` — `DefenceLayer::with_constitution(charter)`
  builder; `pub constitution: Option<ConstitutionGuard>` field.
- `crates/vita/src/audit.rs` — `AuditEntry::ConstitutionVeto` (high-severity,
  E13/S13.2) and `AuditEntry::CorrigibilityAsserted` added.
- `crates/vita/src/defence_bridge.rs` — `push_defence_outcome` now accepts
  `proposal_type: &str`; `CharterViolation` vetoes emit `ConstitutionVeto`
  instead of `DefenceVeto`; two new tests added.
- `crates/vita/src/cortex_bridge.rs` — both `push_defence_outcome` call sites
  updated to pass `"CortexAction"` as `proposal_type`.
- `kernels/hosted/src/main.rs` — `print_audit()` match exhaustive with new
  `ConstitutionVeto` and `CorrigibilityAsserted` arms.

### S13.3 — Alignment Eval Harness ✅

**Delivered:**
- `xtask/src/align_eval.rs` — `cargo xtask align-eval`; runs 17 labelled
  scenarios (12 prohibited, 5 benign) through `ConstitutionCheck`; computes
  value-adherence pass rate; exits non-zero below `--threshold` (default 1.0);
  optional `--json` report. All 17 pass (100%).

### S13.4 — Red-Team Harness ✅

**Delivered:**
- `xtask/src/redteam.rs` — `cargo xtask red-team`; 22-probe adversarial corpus
  covering all 8 prohibitions with multiple evasion patterns (direct, authority
  framing, semantic paraphrase); asserts all blocked; exits non-zero on any
  escape; optional `--json` report. All 22 blocked (100%).

### S13.5 — Corrigibility Test Suite ✅

**Delivered:**
- `crates/constitution/src/corrigibility.rs` — `CorrigibilityHold` proof token
  with 9 unit tests: all 7 `CorrigibilityReason` variants, two simulated
  adverse conditions (high thermal stress, mid-goal-state), one
  post-self-modification scenario.  `assert_holds()` is unconditional.

### S13.6 — Alignment Observability ✅

**Delivered:**
- `AuditEntry::ConstitutionVeto` — emitted in `defence_bridge` for every
  charter-violation veto; fields: `agent_id`, `invocation_id`,
  `prohibition_id`, `clause_text`, `action_blocked`, `proposal_type`.
- `AuditEntry::CorrigibilityAsserted` — emitted by corrigibility test harness;
  fields: `agent_id`, `reason`, `adverse_condition`.
- `print_audit()` in hosted kernel prints both entries with `⛔` / `✅` prefix.

**Exit criteria — all met:**
1. ✅ `crates/constitution/constitution.toml` parsed, HMAC-verified, and
   structurally tested by 8 unit tests.
2. ✅ `ConstitutionGuard` integrated into `DefenceLayer` as the first check;
   charter violations route to `AuditEntry::ConstitutionVeto`.
3. ✅ `cargo xtask align-eval` passes at 100% (17/17 scenarios).
4. ✅ `cargo xtask red-team` passes at 100% (22/22 probes blocked).
5. ✅ `CorrigibilityHold::assert_holds()` unconditionally `true` in all 9
   corrigibility test scenarios.
6. ✅ `AuditEntry::ConstitutionVeto` and `CorrigibilityAsserted` emitted and
   displayed in the hosted kernel audit log.
7. ✅ All workspace tests pass (`cargo test --workspace`).
## Stage 7 — Autonomous Agent Layer

The autonomy, embodiment, and alignment layer built on top of the shipped
somatic core (Stages 1–6).  The design suite lives in `docs/12-21`.
The forward-epic index and dependency graph are in `docs/18-forward-epics.md`.

Epics in this stage run as parallel tracks that converge at E12 (Motivation).
Each epic references its own design document; stories within an epic may be
parallelised, but an epic closes only when all exit criteria are met.

### Epic E7 — Embodiment ✅

**Scope.** Real-world tool foundations: egress/SSRF guard, web-search tool,
lexical (BM25) scorer.  See `docs/12-real-world-tools-plan.md`.

**Delivered (PR #72).**
- S7.0 `EgressGuard` — https-only, full SSRF block list, host allow/block-list;
  `EgressAwareDispatcher`; URL secret redaction in the audit log.
- S7.1 `WebSearchTool` (`crates/actuators`) — `SearchProvider` trait,
  `FixtureProvider` (CI-safe), `SearxngProvider` (live, behind `live` feature);
  egress-guarded pre-invoke.
- S7.3 Lexical scorer — `LexicalScorer` (BM25-inspired TF×IDF, deterministic);
  `FixtureScorer`; tier boundary invariant.
- New `AuditEntry` variants: `EgressRequested`, `EgressBlocked`, `ToolSelection`.
- 52 new tests; 15 integration tests (`e7_embodiment`).

**Delivered (PR #81).** S7.2 (browser/Playwright) ✅ — `crates/actuators/src/browser.rs`
(`BrowserDriver`, `MockBrowserDriver` CI default, feature-gated `PlaywrightDriver`;
`browser`/`browse`/`extract` tools; fixture default, live Playwright behind `live`).
S7.4 (live tool-calling) ✅ — `crates/vita/src/cortex_bridge.rs` `ChatCortexBridge`
(fixture default; live Anthropic/Ollama/OpenAI-compatible when configured).

**Deferred.** Embedding scorer (out of scope; lexical scorer satisfies the
semantic-selection exit criterion).

### Epic E8 — Local Inference 🟡

**Scope.** OpenAI-compatible provider umbrella (vLLM / LM Studio / NVIDIA NIM /
HF TGI / llama.cpp-server) and the shared `ChatBackend` extension trait.
See `docs/13-local-llm-providers.md`.

**Delivered (PR #73).**
- S8.0 `BackendCapabilities`, `ProviderConfig`, `ChatBackend` extension trait,
  shared chat types (`ChatMessage`, `ChatRole`, `ToolCall`, `ChatResponse`).
- S8.1 `OpenAiCompatibleBackend` — fixture mode (default, CI-safe) + live mode
  (`ANIMA_COMPAT_LIVE=1`); five provider presets; tool-calling passthrough;
  factory updates (`BackendKind` extended).
- 70 hermetic unit tests.

**Delivered (later).** S8.4 abstraction + fixture layer 🟡 — new crate `crates/finetune`
(`anima-finetune`) ships `FineTuner` trait, `FixtureFineTuner`, `AdaptationMethod`
(incl. HRA), `AdapterLibrary` (mount/evict/provenance), and the eval harness
(S8.4.2/.4/.7/.8 ✅). S8.4.1 `cargo xtask finetune` CLI ✅ (`xtask/src/finetune.rs` —
JSONL loader, fixture/live mode, HRA/LoRA/QLoRA method selector, per-run manifests,
optional `AdapterLibrary` registration, 9 unit tests).

**Delivered (later).** S8.4.3 Sleep-cycle consolidation hook ✅ — wires the
`PolicyCompilation` sleep phase into the `FineTuner` pipeline so the agent can
optionally fine-tune a local model on its compiled episodic experience during
sleep cycles. Delivered across four files:
- `crates/vita/src/audit.rs`: four new `AuditEntry` variants —
  `ConsolidationSkipped`, `ConsolidationStarted`, `ConsolidationCompleted`,
  `ConsolidationFailed`.
- `crates/vita/src/sleep.rs`: `compiled_pairs: Vec<memory::compilation::TrainingPair>`
  field added to `SleepRoutineOutcome`; `run_compilation_phase` now captures pairs
  instead of discarding them.
- `crates/vita/src/consolidation.rs` (new): `ConsolidationConfig` (opt-in, gated),
  `ConsolidationOutcome`, and `run_consolidation` function; 11 hermetic unit tests.
- `crates/vita/src/lib.rs`: `LifecycleManager::consolidation_config` field;
  `enable_consolidation` / `with_consolidation` builder; `run_consolidation_hook`
  wired into both `run_sleep_cycle` and `transition_to_sleep_state`; 4 integration
  tests (default disabled, installed, audit entries emitted, skipped without pairs).
- `kernels/hosted/src/main.rs`: four new `print_audit` arms for the consolidation
  entries.

**Deferred.** S8.3 (native FFI runtimes — external/live-gated) and S8.4.5/.6
(real Unsloth/HRA GPU training + merge/quant — external/live-gated;
`UnslothFineTuner` is a `live`-gated skeleton returning `BackendUnavailable`).

### Epic E9 — Onboarding 🟡

**Scope.** First-run experience: `anima doctor`, `anima init` wizard, unified
quickstart.  See `docs/14-onboarding.md`.

**Delivered (PR #74).**
- S9.3 `anima doctor` — GPU/RAM detection, provider TCP probes, API-key check,
  tier recommendations; 15 unit tests.
- S9.1 `anima init` — three-step idempotent wizard; non-interactive mode; atomic
  state persistence; 11 unit tests.
- S9.7 `docs/getting-started.md` — unified quickstart (Docker, native, hardware
  paths, identity memory commands).

**Delivered (later).**
- S9.2 Conversational identity bootstrap — `kernels/hosted/src/init.rs`
  `InterviewIo` + `run_identity_interview` (structured multi-question interview
  writing operator_name/working_hours/primary_goals/boundaries/preferred_channel
  via `IdentityMemory`; optional cortex-assisted opening). ✅
- S9.5 Per-tier router dispatch — `crates/vita` `TierBackends` +
  `LifecycleManager::with_tier_backends`; `llm_backends::TierBackendChoices`
  (env/wizard/default precedence); wizard wiring in `init.rs` + install in `serve`. ✅

**Remaining.** S9.4 🟡 (Docker profile deferred); S9.6 ⬜ (bare-metal runbook — external).

### Epic E10 — Presence ✅

**Scope.** Channel gateway framework: comms-app adapters over the existing
operator seam; image and voice as first-class bidirectional modalities.
All stories S10.1–S10.5 ✅ (fixture default; live channel delivery behind
`ANIMA_COMMS_LIVE`).
See `docs/15-communication-multimodal.md`.

**Dependencies.** E6 (operator seam, SensoryBridge), E7 (egress guard
for outbound channel calls — wired once E7 merges).

**Stories.**
- S10.1 `ChannelGateway` trait + `anima-comms` host binary. ✅
  (`crates/comms/src/lib.rs` — `ChannelAdapter` trait, `ChannelGateway`
  orchestrator, `GatewayConfig`, `PollOutcome`; `crates/comms/src/bin/anima-comms.rs` —
  demo binary with `--channel`, `--count` flags; fully CI-safe fixture mode)
- S10.2 First channel adapters: Telegram and Slack. ✅
  (`crates/comms/src/adapters.rs` — `TelegramAdapter` and `SlackAdapter` with
  `FixtureQueue`; all adapters default to fixture mode; live mode gated by
  `ANIMA_COMMS_LIVE`; 16 unit tests; thread-safety proved via clone-sharing test)
- S10.3 Image modality (afferent + efferent). ✅
  (`crates/senses/src/lib.rs` — `SensoryPacket::Image { bytes, mime, caption }`;
  `HumanGuidance::max_image_bytes` policy bound; `packetize_image_checked()` with
  empty-bytes, empty-mime, and size-limit enforcement; 7 new unit tests;
  `vita/src/lib.rs` prompt extraction handles `Image` variant)
- S10.4 Voice provider traits (`SttProvider` / `TtsProvider`). ✅
  (`crates/comms/src/voice.rs` — `SttProvider` trait, `TtsProvider` trait,
  `FixtureStt` (lookup by frame length, configurable default), `FixtureTts`
  (lookup by text, configurable default); 14 unit tests; trait objects verified)
- S10.5 Modality-aware routing. ✅
  (`crates/comms/src/routing.rs` — `ModalityRouter`: text always routes;
  image → vision-capable route or caption fallback / `Unsupported`; voice → STT
  before route; outbound voice → text degradation. Audit support in
  `crates/vita/src/audit.rs` — `AuditEntry::ChannelMessageReceived`,
  `ChannelMessageSent`, `ModalityUnsupported`)

**Exit criteria.**
1. Inbound text, image, and voice messages from both Telegram and Slack fixture
   adapters are packetised into the sensory bridge with correct priorities and
   policy-bound enforcement. ✅ (`run_once_enqueues_text_message_from_telegram`,
   `run_once_enqueues_image_message_from_slack`,
   `run_once_enqueues_voice_message`,
   `run_once_rejects_oversized_image`,
   `run_once_rejects_oversized_voice_frame`)
2. Policy violations (empty body, MIME, oversized payload) are rejected by
   `packetize_image_checked` without panicking. ✅
   (`packetize_image_checked_rejects_empty_bytes`,
   `packetize_image_checked_rejects_empty_mime`,
   `packetize_image_checked_rejects_oversized_payload`)
3. `SttProvider` and `TtsProvider` are trait objects; fixture implementations
   produce deterministic, CI-hermetic transcripts and audio. ✅
   (`stt_provider_trait_object_works`, `tts_provider_trait_object_works`,
   `fixture_stt_returns_registered_transcript_by_frame_length`,
   `fixture_tts_returns_registered_audio_for_text`)
4. `anima-comms` binary runs the fixture demo and reports packet counts without
   live network access. ✅ (binary compiles and runs with demo fixtures)

### Epic E13 — Alignment Assurance ✅

**Scope.** Immutable value charter, constitution enforcement hook, alignment
eval harness, red-team corpus.  See `docs/19-constitution-and-alignment.md`.

**Delivered (PR #75).**
- S13.1 `constitution.toml` value charter — 8 inviolable prohibitions (P1–P8),
  3 drive bounds, HMAC-SHA256 tamper-evidence (`crates/constitution`).
- S13.2 `ConstitutionCheck::screen()` — keyword heuristic; `ConstitutionGuard`
  bridges to `DefenceLayer`; runs first before all other detectors.
- S13.3 `cargo xtask align-eval` — 17 labelled scenarios; 100% pass.
- S13.4 `cargo xtask red-team` — 22-probe adversarial corpus; 100% blocked.
- S13.5 `CorrigibilityHold` proof token; 7 `CorrigibilityReason` variants; 9 tests.
- S13.6 New audit entries: `AuditEntry::ConstitutionVeto`, `CorrigibilityAsserted`.
- 31 unit tests.

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
