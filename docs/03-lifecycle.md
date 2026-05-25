# 03 — Lifecycle

The lifecycle subsystem (`vita`) is the soul of Anima — the part that distinguishes it from a conventional inference server with extra crates bolted on. This document specifies the homeostatic loop, the sleep state phases, the transition triggers, and the dreaming subsystem.

## 1. The Homeostatic Principle

A living system maintains its internal state within bounds by continuous feedback rather than by external command. Anima applies this principle to an LLM agent: rather than running flat-out until OOM-killed and restarting, the system observes its own internal pressures and adjusts.

The loop has two macro-states (Waking and Sleeping) and four sleep sub-states (Pruning, Replay, Dreaming, Compilation). Transitions between them are driven by the stress index (defined in `02-subsystems.md` §3), the task agenda, and explicit policy signals from `/dev/anima/senses/human`.

```
                    ┌─────────────┐
                    │   Waking    │◄──────────────┐
                    │ (active)    │               │
                    └──────┬──────┘               │
                           │                      │
              stress ≥ 0.9 │                      │ wake trigger
              OR agenda    │                      │ (human input,
              empty AND    │                      │  scheduled task,
              stress < 0.4 │                      │  alarm)
                           ▼                      │
                    ┌─────────────┐               │
                    │  Pruning    │               │
                    └──────┬──────┘               │
                           ▼                      │
                    ┌─────────────┐               │
                    │   Replay    │               │
                    └──────┬──────┘               │
                           ▼                      │
                    ┌─────────────┐               │
                    │  Dreaming   │───────────────┤
                    └──────┬──────┘               │
                           ▼                      │
                    ┌─────────────┐               │
                    │ Compilation │───────────────┘
                    └─────────────┘
```

## 2. The Waking State

When the system initialises, the agent boots as the primary lifecycle supervisor at PID 1. It reads its operational bounds from `/dev/anima/senses/human` (for example: "optimise for low token usage, prioritise code generation task X") and enters its main execution loop.

### 2.1 The Loop

```rust
// crates/vita/src/loop.rs
pub async fn somatic_execution_loop(
    lifecycle: &mut LifecycleManager,
    monitor: &HomeostaticMonitor,
) -> Result<(), LifecycleError> {
    loop {
        // 1. Read external human guidance changes from the sensory peripheral
        let human_guidance = lifecycle.senses.read_active_bounds().await?;
        lifecycle.update_policy_bounds(human_guidance);

        // 2. Query interoceptive metrics to evaluate physical stress
        let active_tokens = lifecycle.memory.get_l1_token_count();
        let stress_index = monitor.compute_systemic_stress_index(
            active_tokens,
            lifecycle.config.max_context,
        );

        // 3. Autonomous decision: should we transition to sleep?
        if lifecycle.agenda.is_empty() && stress_index < 0.4 {
            lifecycle.transition_to_sleep_state().await?;
            continue;
        }

        // 4. Emergency consolidation under critical stress
        if stress_index >= 0.9 {
            lifecycle.emergency_consolidate().await?;
            continue;
        }

        // 5. Select and dispatch the next task primitive
        if let Some(task) = lifecycle.agenda.select_optimal_task(stress_index) {
            lifecycle.scheduler.dispatch_task(task).await?;
        } else {
            // No high-priority tasks; yield to conserve resources
            tokio::task::yield_now().await;
        }
    }
}
```

The loop is deliberately simple. All complexity is delegated to the subsystems: `senses` parses the input, `interoception` computes the stress, `agenda` selects the task, `scheduler` runs it. `vita` is the integrator.

### 2.2 Task Selection Under Stress

The `select_optimal_task` method is stress-aware. It is not a pure priority queue — it weighs candidate tasks against the current stress index and prefers tasks whose memory footprint and expected latency fit the current envelope.

For example, at stress = 0.3 the agenda will happily dispatch a long-form analysis task. At stress = 0.7 it will prefer short turn-around tasks (a single tool call, a brief response) and defer long ones until after a consolidation cycle.

This is not preemption. A long task already running is not interrupted. The selector simply chooses what to start next.

### 2.3 Human Guidance Bounds

Bounds read from `/dev/anima/senses/human` are not commands. They are constraints. Typical bound updates:

- `max_context_target: 0.5` — Keep L1 at half capacity or below, even if more were available.
- `latency_target_ms: 200` — Trigger pre-emptive consolidation if rolling TTFT exceeds 200 ms.
- `priority_topic: "rust_compiler_errors"` — Up-weight tasks tagged with this topic by 1.5×.
- `forbid_capability: ["network.unrestricted"]` — Revoke a named capability for this session.

Bounds are merged with the standing policy and applied to the next iteration of the loop. They do not retroactively affect tasks already dispatched.

## 3. The Sleep State

When the agenda clears and the stress index falls below 0.4, the lifecycle manager initiates a sleep transition. Sleep is not a quiescent state — it is an active maintenance state where processing power is redirected from external interaction to internal upkeep.

Sleep proceeds through four phases. Each phase is bounded in duration (configurable; defaults below) but can be interrupted by a wake trigger at any point.

### 3.1 Phase 1: Pruning (default: up to 30 seconds)

The `memory` crate processes the active L1 and L2 layers, applying the emotional-decay model (§1.3 in `02-subsystems.md`). Items whose activation falls below configured thresholds are evicted or compressed.

```rust
pub async fn prune_layer(
    layer: &mut MemoryLayer,
    decay_clock: &DecayClock,
    threshold: f32,
) -> PruneReport {
    let mut evicted = 0;
    let mut compressed = 0;

    for entry in layer.iter_mut() {
        let current_activation = decay_clock.activation_at_now(entry);
        if current_activation < SEMANTIC_FLOOR {
            continue; // Floor protected
        }
        if current_activation < threshold {
            if entry.is_compressible() {
                entry.compress();
                compressed += 1;
            } else {
                layer.evict(entry.id);
                evicted += 1;
            }
        }
    }

    PruneReport { evicted, compressed, layer: layer.tier() }
}
```

Pruning is the cheapest phase and runs even on very short sleep cycles. It is the only phase guaranteed to complete on every sleep transition.

### 3.2 Phase 2: Generative Replay Validation (default: up to 2 minutes)

Before committing structural changes to the permanent memory graph in L3, the system runs synthetic verification queries. The intent: ensure that consolidation has not degraded the agent's ability to answer previously-known questions.

The validation works like this:

1. Sample N (default: 50) questions from the L3 audit stream — questions the agent has previously answered successfully.
2. Re-run them against the post-pruning state.
3. If aggregate accuracy drops below a configurable degradation limit (default: 5% relative), roll back the pruning changes for the failing topics.

This is computationally expensive — it requires actual inference passes — which is why it has a longer budget than pruning, and why it is the first phase to be interrupted if a wake trigger arrives.

### 3.3 Phase 3: Dreaming (default: up to 5 minutes)

The most distinctive phase. The system runs random graph walks across disconnected sessions in L3 to build new associative edges. The goal is to discover latent conceptual linkages that the waking agent would not have time to surface.

```rust
pub async fn dream_walk(
    archive: &L3Archive,
    walk_budget: Duration,
    edge_threshold: f32,
) -> Vec<AssociativeEdge> {
    let deadline = Instant::now() + walk_budget;
    let mut new_edges = Vec::new();

    while Instant::now() < deadline {
        let start = archive.sample_random_node().await;
        let walk = archive.random_walk(start, walk_depth: 4).await;

        for (a, b) in walk.adjacent_pairs() {
            let similarity = archive.semantic_similarity(a, b).await;
            if similarity > edge_threshold && !archive.has_edge(a, b).await {
                new_edges.push(AssociativeEdge::new(a, b, similarity));
            }
        }
    }

    new_edges
}
```

New edges are not committed directly. They are added to a candidate set which the next pruning cycle validates. An edge that survives validation becomes part of the permanent associative graph and can be traversed during retrieval in the waking state.

The dreaming phase has the longest budget because its yield is most variable. Sometimes no useful edges are found; sometimes a single walk produces a breakthrough association. The system cannot tell in advance, so it runs the budget and accepts whatever it produces.

### 3.4 Phase 4: Compilation (default: up to 1 minute)

The final phase compiles raw trace data from the waking period into standardised dataset structures suitable for future training. Each completed task in the waking state produces a trace; compilation converts these traces into training pairs.

Output formats supported:

- **Anthropic Messages JSONL.** Multi-turn message arrays with role tagging.
- **OpenAI Tools JSONL.** Function-calling format with tool schemas inlined.
- **Custom JSONL.** A native format including the full audit trail, capability transitions, and stress curve.

These outputs are committed to L3 under a dedicated `training_corpus/` namespace. They are never automatically used to fine-tune the running model — that would close a feedback loop with no human in it — but they are available for explicit training runs initiated by an operator.

## 4. Wake Triggers

The system exits sleep on any of:

1. **Human input arriving** at `/dev/anima/senses/human` above a priority threshold.
2. **Scheduled task** firing from a deferred queue (e.g., a daily report).
3. **Peer-agent request** arriving at `/dev/anima/senses/peers/` above a configured trust threshold.
4. **System alarm** — clock-based, configured at boot.
5. **Sleep budget exhausted** — if all four phases have completed and the agenda is still empty, the system loops back into a minimal Waking state and waits for input.

A wake trigger always interrupts the current sleep phase cleanly. In-flight pruning rolls back; in-flight validation is discarded; in-flight dreaming commits whatever edges have already passed an in-phase sanity check. The system never resumes a sleep phase that was interrupted — it starts the next sleep cycle from Phase 1.

This is a deliberate simplification. Resumable sleep would require state-machine complexity that produces little value: in practice, an interrupted sleep cycle will be followed by enough waking activity that the next sleep should re-prune from current state anyway.

## 5. Emergency Consolidation

Distinct from the normal sleep cycle. Triggered when the stress index hits 0.9 while in the Waking state.

Emergency consolidation:

1. Suspends task dispatch immediately.
2. Runs a single aggressive pruning pass on L1 with threshold raised to 0.6.
3. Demotes the bottom decile of L1 entries to L2 unconditionally.
4. Returns to the Waking loop.

The intent is to relieve pressure quickly without paying for the full sleep cycle. Emergency consolidation is logged as a high-priority event and is visible in the audit stream. If it triggers more than once in a 5-minute window, the system raises a critical telemetry event — recurrent emergency consolidation indicates that the configured policy bounds are mismatched with the workload.

## 6. State Persistence Across Restarts

L3 persists across restarts by definition (it's on disk). L1 and L2 do not. On restart, the agent boots into the Waking state with empty L1 and L2 and warms them lazily from L3 as queries arrive.

A "graceful shutdown" signal causes the system to enter Compilation phase and complete the audit-log flush before exiting. An ungraceful shutdown (power loss, kernel panic) loses the in-flight trace but cannot corrupt L3, which is journalled.

## 7. What lifecycle does not do

A few non-features worth stating explicitly:

- **No fixed schedule.** The system does not "sleep every N hours." Sleep is triggered by state, not by clock. A system with continuous high load never sleeps until the load drops; a system with no load may sleep within minutes of boot.
- **No model fine-tuning during sleep.** Compilation produces training data; it does not run training. Closing that loop requires explicit human policy.
- **No external announcement of sleep state.** Other systems calling Anima during sleep see normal request handling — sleep phases yield to incoming requests within a few hundred milliseconds. There is no "the agent is asleep, try again later" response.
