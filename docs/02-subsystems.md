# 02 — Subsystems

Detailed specifications for the major subsystems of Anima. Each section covers the design intent, the mechanism, and the relevant code shape.

---

## 1. Memory: Synaptic Storage and the CLS Hierarchy

The memory subsystem implements a three-tier model derived from the Complementary Learning Systems framework in cognitive neuroscience. The justification for three tiers (rather than the conventional two-tier cache + storage) is that the working context of an LLM has fundamentally different access patterns from a recency cache.

### 1.1 The Three Tiers

**L1 — Working Context.** Memory-mapped via PagedAttention directly into the model's active attention field. Strict token allocations are reserved for system instructions, human guidance, and active scratchpad entries. Access cost: near zero. Capacity: bounded by model context window. Eviction policy: managed by `vita`'s consolidation routines, not by demand paging.

**L2 — Warm Memory Cache.** Hosted in host RAM (or microVM RAM, depending on target) as a concurrent `scc` hashmap. Stores raw token vectors and unmapped physical KV cache blocks. Managed by the Adaptive Replacement Cache policy, which handles both recency and frequency variations more gracefully than pure LRU for the bimodal access patterns we observe (repeated reference to recent context plus occasional hits on frequently-cited older items).

**L3 — Cerebral Archival Store.** Persistent embedded LanceDB instance. Items are retrieved by vector similarity rather than path lookup. Survives sleep cycles. The L3 store is the only memory tier that persists across process restarts.

### 1.2 Promotion and Demotion

Items move between tiers under two pressures:

- **Demand-side promotion.** L2 → L1 occurs when a retrieval query in the active loop matches an item in L2 above a similarity threshold. L3 → L2 occurs when a dreaming-state random walk surfaces an item that the prior 24 hours of activity suggests is relevant.
- **Pressure-side demotion.** L1 → L2 occurs when the stress index (see §3) crosses 0.75 and `vita` triggers context pruning. L2 → L3 occurs continuously during sleep states.

Eviction from L3 is rare and requires explicit policy. The default is unbounded growth bounded only by the underlying storage capacity. This is intentional — semantic floor (§1.3) prevents the agent from forgetting things it once knew well.

### 1.3 Emotionally Modulated Decay

The baseline activation value of an episodic memory node degrades over time according to an exponential decay model modulated by emotional context:

$$
S(t) = \max\left( S_{\text{floor}},\; S_0 \cdot e^{-\lambda t} \cdot \left(1 + \alpha \cdot \text{arousal} + \sigma \cdot \text{surprise}\right) \right)
$$

Where:
- $S_0$ is the initial activation value at the time of memory formation.
- $\lambda$ is the decay constant (default: 0.02 per hour of wall time, 0.005 per hour of sleep state).
- $\text{arousal}$ and $\text{surprise}$ are scalars in $[0, 1]$ assigned at formation time by the consolidation routine.
- $\alpha$ and $\sigma$ are weighting parameters (default: 1.5 and 2.0 respectively, giving surprise slightly more weight than raw arousal).
- $S_{\text{floor}} = 0.3$ is the absolute semantic floor. Memories at this level are retrievable but never surface unprompted.

The floor exists to prevent distilled, high-generation knowledge from being erased by the decay loop. Without it, a sufficiently long-lived agent could forget its own training.

### 1.4 What's stored

| Tier | Typical content | Lifetime |
|------|----------------|----------|
| L1 | Current task prompt, recent tool outputs, scratchpad, system directives | Minutes |
| L2 | Recent conversation turns, retrieved L3 items, computed embeddings | Hours |
| L3 | All past conversations, learned tool schemas, dream-discovered associations, training data | Indefinite |

---

## 2. Praxis: The Efferent Actuator Core

Praxis is the agent's motor cortex. It is the subsystem through which the agent acts on the world: invoking tools, calling APIs, executing generated code, sending messages. Where `senses` is afferent (input), `praxis` is efferent (output).

External toolsets, hardware engines, and MCP/A2A endpoints are exposed as device drivers under `/dev/anima/praxis/tools/`.

### 2.1 Length-Robust Relative Routing

A naive tool router selects tools whose match score against the current query exceeds a fixed threshold. This breaks down in two ways: as the tool registry grows, marginal tools accumulate and dilute the context; and as the query becomes more specific, the absolute scores drop and good tools fall below the threshold.

Anima uses relative filtering instead. Tools are admitted to the active set if their score is within a factor of the best-scoring candidate:

$$
T_{\text{filtered}} = \left\{ t \in T \;\middle|\; \text{score}(t, q) \ge \tau_{\text{rel}} \cdot \max_{t' \in T}\left(\text{score}(t', q)\right) \right\}
$$

Default $\tau_{\text{rel}} = 0.7$. This makes the filter behave consistently across registry sizes: a small registry surfaces a small number of close matches; a large registry surfaces the same close matches and prunes the long tail.

### 2.2 Circuit Breakers

Every tool driver is wrapped in a state monitor that observes failure rates and trips when the failure pattern indicates the tool is unhealthy. This isolates the failure from the system core.

```rust
// crates/praxis/src/breaker.rs
pub struct CircuitBreaker {
    pub failure_count: u32,
    pub state: BreakerState,
    pub last_failure: Option<std::time::Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    Closed,   // Pathway healthy
    Open,     // Fault detected; execution blocked
    HalfOpen, // Tentatively re-admitting traffic
}

impl CircuitBreaker {
    pub fn verify_pathway_health(&mut self) -> Result<(), &'static str> {
        if self.state == BreakerState::Open {
            if let Some(last_fail) = self.last_failure {
                if last_fail.elapsed() > std::time::Duration::from_secs(30) {
                    self.state = BreakerState::HalfOpen;
                    return Ok(());
                }
            }
            return Err("Execution pathway blocked by active circuit breaker.");
        }
        Ok(())
    }
}
```

The breaker has three states. `Closed` is the default — traffic flows. `Open` is triggered after a configurable failure threshold (default: 5 failures within 60 seconds); the tool is blocked from invocation for 30 seconds. `HalfOpen` admits a single request; success resets to `Closed`, failure reopens for another 30 seconds with exponential backoff.

The breaker's state is observable from `interoception`, allowing the stress index to incorporate praxis health.

### 2.3 Wasmtime Sandboxing

Untrusted code — particularly code the agent generates itself for one-off tool use — runs inside a wasmtime sandbox. The sandbox is configured with:

- A gas meter set per-invocation (default: 100M instructions).
- A linear memory limit (default: 16 MiB).
- No host capabilities by default; capabilities are explicitly granted per invocation.
- Timeout enforcement at the wasmtime epoch deadline level.

The sandbox's import surface is a small set of typed function imports corresponding to the granted capabilities. Even WASI is granted selectively: a code-execution sandbox might be granted `wasi:io/streams` but not `wasi:filesystem` or `wasi:sockets`.

### 2.4 MCP and A2A Buses

Both Model Context Protocol (MCP) and Agent-to-Agent (A2A) protocols are exposed through praxis. They appear to the agent as ordinary tool drivers under `/dev/anima/praxis/tools/mcp/<server>/` and `/dev/anima/praxis/tools/a2a/<peer>/`. From the agent's perspective there is no architectural distinction between calling a local tool and calling a remote agent.

Network egress for these protocols runs over the `smoltcp` stack with `rustls` for transport security. No host TCP/IP is used.

---

## 3. Interoception: The Stress Index

The `interoception` crate provides real-time monitoring of the agent's internal state. Unlike conventional telemetry (which exists primarily for human operators reading logs), Anima's interoception exists primarily for the agent itself: it is the signal that drives autonomic decisions about when to sleep, prune, or escalate.

### 3.1 The Systemic Stress Index

A composite scalar in $[0, 1]$ indicating overall system pressure:

```rust
// crates/interoception/src/lib.rs
pub struct HomeostaticMonitor {
    pub rolling_ttft: VecDeque<f32>,
    pub baseline_ttft: f32,
    pub beta: f32, // Balance parameter: latency vs token pressure
}

impl HomeostaticMonitor {
    pub fn compute_systemic_stress_index(
        &self,
        active_tokens: u32,
        max_context: u32,
    ) -> f32 {
        if self.rolling_ttft.is_empty() { return 0.0; }

        let avg_ttft: f32 =
            self.rolling_ttft.iter().sum::<f32>() / self.rolling_ttft.len() as f32;
        let latency_ratio = if self.baseline_ttft > 0.0 {
            avg_ttft / self.baseline_ttft
        } else {
            1.0
        };
        let memory_ratio = active_tokens as f32 / max_context as f32;

        (self.beta * latency_ratio) + ((1.0 - self.beta) * memory_ratio)
    }
}
```

The index is a weighted combination of two pressures: latency degradation (time-to-first-token relative to baseline) and context saturation (active tokens relative to maximum). The default $\beta = 0.4$ gives slightly more weight to memory pressure than latency, on the grounds that latency degradation is often a symptom of memory pressure and double-counting should be avoided.

### 3.2 Thresholds

| Range | State | Action |
|-------|-------|--------|
| $[0, 0.4)$ | Relaxed | Normal operation; sleep candidate if agenda empty |
| $[0.4, 0.6)$ | Engaged | Normal operation; no sleep |
| $[0.6, 0.75)$ | Elevated | Defer non-essential tool calls; warn dreaming routines |
| $[0.75, 0.9)$ | Stressed | Trigger pre-emptive L1 → L2 demotion; reduce batch size |
| $[0.9, 1.0]$ | Critical | Emergency consolidation; reject new task admissions |

These thresholds are configurable per deployment. The values above are defaults derived from the homeostatic principle that the system should make adjustments well before any single resource is exhausted.

### 3.3 What's not in the index

The stress index deliberately does not include:

- **CPU utilisation.** Anima is inference-bound; CPU pressure is a symptom rather than a cause.
- **Network latency.** Praxis circuit breakers handle network pathology directly.
- **Disk I/O.** L3 access patterns are bursty and the rolling window would either suppress relevant signal or oscillate.

Adding these would inflate the index without improving its predictive value for the decisions it drives.

---

## 4. Senses: The Afferent Bridge

The `senses` crate parses raw input streams from hardware drivers into structured events on the inter-crate event bus. It is deliberately thin: parsing only. Interpretation of what events mean is the responsibility of `vita`.

### 4.1 Supported Input Modalities

- **Text streams.** UTF-8 over a Unix socket or named pipe. Packetised at newline or every 4 KiB, whichever first.
- **Voice streams.** Raw PCM (16-bit, 16 kHz) over a streaming socket. Voice activity detection runs in the driver; only voiced segments are forwarded.
- **RPC intents.** Length-prefixed `postcard`-encoded messages over Unix socket. Used for programmatic operators (other agents, scripts, web frontends).

### 4.2 Event Schema

All sensory events share a common envelope:

```rust
pub struct SensoryEvent {
    pub source: SourceId,           // /dev/anima/senses/<source>
    pub timestamp: Instant,
    pub priority: Priority,         // Tagged at the driver level
    pub payload: SensoryPayload,
}

pub enum SensoryPayload {
    Text(TextBuffer),
    Voice(VoiceFrame),
    Rpc(RpcIntent),
    SystemEvent(SystemEventKind),
}
```

Priority is assigned at the driver level based on configurable rules. Direct human input is always `High`; system clock ticks are `Low`; peer-agent messages are configurable.

### 4.3 Backpressure

Sensory streams are bounded. If the agent cannot consume events as fast as they arrive, the driver applies backpressure to the sender (via TCP windowing for network sources, via socket buffer fill for local sources). The crate does not buffer indefinitely. This is a deliberate choice: an agent that is too overloaded to process input should communicate that fact to the input source, not silently accumulate unprocessed events.

---

## 5. Self: Capability Tokens and Identity

The `self` crate provides the agent's self/non-self barrier. It is the equivalent of an immune system, plus an identity tracker.

### 5.1 Typestate Capabilities

Capabilities in Anima are values whose Rust type encodes their permission level. A capability cannot be forged or modified without unsafe code (which is forbidden outside `corpus`). Passing a capability to a function consumes it; functions return new capabilities reflecting the operation performed.

```rust
pub struct NetworkCap<S: NetworkState> {
    _phantom: PhantomData<S>,
    handle: NetworkHandle,
}

pub trait NetworkState {}
pub struct Restricted;
pub struct Unrestricted;
impl NetworkState for Restricted {}
impl NetworkState for Unrestricted {}

impl NetworkCap<Restricted> {
    pub fn upgrade(self, token: ElevationToken) -> NetworkCap<Unrestricted> {
        // Token consumed; new capability returned at elevated state
        NetworkCap { _phantom: PhantomData, handle: self.handle }
    }
}
```

This pattern is applied throughout: memory access capabilities, praxis invocation capabilities, sensory subscription capabilities. The compiler enforces the capability graph at build time.

### 5.2 Identity Tracking

Each running task is associated with a UID/GID equivalent — but the IDs identify *roles* rather than users. A task running the L3 consolidation routine has `role: consolidator`; a task handling a human request has `role: responder`. Roles determine which capabilities are issued.

The role set is fixed at build time and lives in a `roles.toml` file at the workspace root. There is no dynamic role creation. This is intentional: an agent that can mint new roles for itself has effectively defeated the capability system.

### 5.3 Audit Trail

Every capability operation (issuance, consumption, upgrade, revocation) emits an event to a dedicated audit stream. The stream is persisted in L3 with the highest emotional weight (so it never decays out of the archive) and is the canonical record of what the agent did and under what authority.
