# 08 — Cognitive Architecture

*Build spec contribution — cognitive layer design.*

> **Relationship to the rest of this suite.** This document specifies
> the cognitive layer that sits above the somatic substrate defined in
> `01-architecture.md` through `04-verification.md`. Where it uses
> different vocabulary than the rest of the suite (for example *cortex*
> rather than `vita` deliberative routines, or *Striatal Gate* rather
> than scheduler arbitration), the existing anatomical naming in
> `01-architecture.md` and `06-glossary.md` remains authoritative for
> crate-level artefacts. The naming in this document is the
> cognitive-layer working vocabulary; mappings to crate-level names are
> recorded in the glossary as each piece is implemented.
>
> The phasing in Section 11 is reconciled against the existing milestone
> plan in `07-implementation-plan.md` under **Stage 5 — Cognitive
> Layer**, which is the canonical delivery breakdown.

---

## 0. Document scope

This document specifies the cognitive architecture of AnimaOS: the brain that sits inside the body that the operating system provides. It does not specify the OS distribution decisions, the kernel modifications, or the userland tooling — those belong in their own sections of the build spec. What is specified here is the layered control system that turns AnimaOS from "an operating system with an agent stapled on" into "an operating system whose behaviour is governed by a single coherent cognitive process."

The document is deliberately verbose because the architecture has to make several decisions that are easy to confuse and easy to drift on later. Where possible, each design choice is justified, and where the justification is weak or speculative, that is stated explicitly rather than dressed up.

The cognitive architecture is **inspired by hierarchical biological control** — specifically the integration of evolutionarily conserved reflexive systems with slower deliberative ones, and the role of gated working memory in mediating between them. The biological inspiration informs the structure but does not constrain it. Where silicon affords better solutions than wetware (random-access memory, structured databases, perfect recall, lossless communication between modules) those are used without apology. The brain is a reference, not a target.

---

## 1. Thesis

AnimaOS is an agent-first operating system. The OS itself is the body: it provides the sensorium (system telemetry, filesystem events, network state, user interaction), the motor system (shell, processes, file operations, network I/O, GUI control), and the homeostatic signals (thermal load, memory pressure, battery, GPU utilisation, latency, financial cost of API calls) that any embodied cognitive system needs in order to act with awareness of its own constraints.

The cognitive layer sitting above this body is structured as a hierarchy of control. Fast, cheap, always-on policies handle the majority of events without invoking expensive reasoning. Slower, more expensive deliberative processes are invoked only when fast policies cannot resolve the situation, and the decision to escalate is itself a learned policy. Body state continuously modulates the thresholds at which escalation occurs. The result is a system whose intelligence is not constant: it spends compute deliberately, the way a biological organism spends metabolic energy deliberately.

The technical core of the cognitive layer is a learned memory controller — derived from prior work on stateful KV-cache gating — that decides what the system remembers, compresses, and forgets across long-horizon operation. Without this, an agent-first OS would suffer the same context degradation that plagues every current long-running agent. With it, the system can run for hours or days, retain what matters about the user and the task, and discard what does not.

---

## 2. Design principles

The architecture is built on six principles. Each is stated as a constraint on the design, not an aspiration.

**Hierarchical control with graceful degradation.** The system must remain functional when the most expensive cognitive components are unavailable. Network down, rate-limited, on battery, thermally throttled, or simply faced with a low-value event — the system continues to operate, just more reflexively. The deliberative layer is a luxury, not a requirement. This is the same property biological organisms exhibit under fatigue, intoxication, or stress: the lights stay on, but the policies get cheaper.

**Embodiment is not metaphor.** Body state is not logged for display purposes; it is a control input. Thermal load directly modifies the cost threshold for invoking the cortex. Battery state shifts the routing policy between local and API models. Memory pressure triggers consolidation. If a body-state signal does not modulate at least one policy, it does not belong in the architecture.

**Selective forgetting is a feature.** Perfect recall is not a goal. The system must learn to forget — boilerplate, failed attempts, transient state — while retaining what is structurally important. This applies at every level: KV-cache eviction during a single reasoning episode, episode summarisation across hours, identity-level memory across the lifetime of the installation.

**Proposals, not conversations.** Subsystems within the cognitive layer do not chat with each other. They submit structured proposals to a central arbiter, which selects, suppresses, or combines them. This is enforced architecturally to prevent the runaway message-passing patterns that plague conventional multi-agent frameworks.

**Falsifiable, not visionary.** Every claim in the architecture must be testable. "The system feels its body" is replaced with "policy P measurably changes when telemetry T crosses threshold X." If a feature cannot be falsified, it does not ship; it goes in the speculative addendum.

**The substrate is silicon, not wetware.** Where the biological inspiration suggests a structure that maps cleanly onto silicon, it is used. Where it does not, it is discarded. There is no obligation to have a hippocampus-equivalent, an amygdala-equivalent, or any other named anatomical structure. The names exist in the codebase as internal shorthand where they aid clarity; they do not exist in the public-facing architecture.

---

## 3. The Body: OS as substrate

The body layer is everything the cognitive system can sense and everything it can affect. It is implemented as a privileged daemon — call it `animad` — written in Rust, communicating with the cognitive layer over a structured local IPC channel (Unix domain socket carrying length-prefixed protocol buffers, or an equivalent). The choice of Rust is deliberate: this layer must be small, fast, reliable, and trustworthy, and it must survive the cognitive layer crashing.

The daemon exposes three categories of interface.

### 3.1 Sensorium

The sensorium is the structured stream of events and state that the cognitive layer can observe. It includes:

- **Thermal state.** Per-zone temperatures from `/sys/class/thermal` on Linux, `ioreg` on macOS, equivalent APIs on Windows. Sampled at low frequency (1 Hz baseline, higher under stress).
- **Compute state.** CPU utilisation per core, GPU utilisation and VRAM occupancy (via `nvidia-smi` or vendor equivalents), system load average, scheduler pressure (PSI on Linux).
- **Memory state.** Resident memory, page cache behaviour, swap pressure, OOM proximity.
- **Power state.** Battery level, charging state, power draw if available, AC connection.
- **Network state.** Connection state, bandwidth utilisation, latency to anchor hosts, rate-limit headroom against known APIs.
- **Filesystem events.** Watched directories produce structured change events via inotify/fsevents/equivalent.
- **User-presence signals.** Keyboard and mouse idle time, active window, foreground application, screen state. This is the most privacy-sensitive category and is gated by explicit user consent at install time.
- **Financial state.** Tracked spending against API providers, daily and monthly budgets, projected burn rate. This is treated as a body state, not an external concern: an agent that cannot feel the cost of its own thinking will not bound it.

All sensorium streams are normalised into a common event schema with a source, timestamp, modality, raw payload, and pre-computed derived features (novelty score, urgency score, change-from-baseline). The pre-computation happens in the daemon, not the cognitive layer, because feature extraction at sensor rate is wasteful work for a language model.

### 3.2 Motor system

The motor system is everything the cognitive layer can do to the world via the OS. It is exposed as a tool surface with explicit permission scopes:

- **Shell execution.** Bounded by per-invocation timeouts and a configurable sandboxing layer.
- **Filesystem operations.** Read, write, move, delete, with write operations gated by scope.
- **Process management.** Spawn, signal, observe.
- **Network requests.** HTTP, with per-host allowlists.
- **Notification surface.** Send desktop notifications, surface dialogs, request user attention.
- **GUI control.** Optional, off by default, requires explicit opt-in. The argument for including it is that it makes AnimaOS a complete agent; the argument against is that GUI automation is fragile and security-sensitive.
- **Self-modification.** The cognitive layer can update its own configuration, prompts, and routing rules, gated by an internal review process and reversibility guarantees.

Every motor action passes through an audit log with the proposing subsystem, the approving gate decision, and the outcome. This is non-negotiable: an agent-first OS that cannot explain why it did something is a liability.

### 3.3 Homeostatic signals

Homeostatic signals are the body's input to the cognitive layer's policy modulation. They are derived from sensorium streams but are conceptually distinct: a sensorium event is "the GPU is at 94°C," a homeostatic signal is "thermal load is elevated, deliberation should be more expensive than usual." The daemon publishes a small set of scalar signals that the cognitive layer subscribes to:

- `thermal_load ∈ [0, 1]` — composite measure of how hot the machine is relative to its sustainable envelope
- `compute_pressure ∈ [0, 1]` — how saturated compute resources are
- `memory_pressure ∈ [0, 1]` — proximity to OOM
- `power_budget ∈ [0, 1]` — how much energetic headroom remains (1.0 on AC, scales with battery)
- `financial_budget ∈ [0, 1]` — remaining API spend relative to budget
- `attention_demand ∈ [0, 1]` — composite of how much the user appears to need the system right now (active use, recent interaction, foreground state)

These signals are the input substrate for policy modulation throughout the cognitive layer. They are deliberately scalar and small in number: the goal is interpretable global modulation, not a high-dimensional feature vector.

---

## 4. The Brainstem: reflexive control

The brainstem layer is a small, always-on policy engine that handles routine system events without invoking the cortex. It is implemented in Rust as part of `animad` itself, not as a separate language model process. The decision to keep it in-daemon and in-Rust is deliberate: this layer must run with predictable latency, must never block on network, and must continue functioning when everything above it has crashed.

The brainstem is responsible for:

- **Reflexive responses to body state.** Thermal throttle triggers a defensive policy: pause background work, reject low-value cortex invocations, surface a notification if sustained. Memory pressure triggers cache consolidation. Battery depletion triggers a routing shift to local models.
- **Pattern-matched routine handling.** Common events that have known cheap responses do not need the cortex. "User pressed the brightness key" does not need a reasoning step.
- **Pre-cortex triage.** Every event that might be escalated to the cortex first passes through the brainstem's triage policy, which decides whether it is worth waking the deliberative layer at all.
- **Safety reflexes.** Hard-coded responses to specific dangerous conditions: filesystem operation targeting a critical path, network call to a blocklisted host, attempted modification of `animad` itself by a child process.

The brainstem is not "the lizard brain." It is a fast policy engine that handles the long tail of events for which deliberation is unnecessary or actively harmful. It is, however, structurally analogous to evolutionarily older control layers in biological systems: it is fast, it is always running, it is small, it has direct access to body state, and it can act without consulting higher layers.

Implementation: the brainstem's policies are expressed as a rule set, augmented over time with learned policies (small classifiers, decision trees, or — speculatively — a tiny distilled model). The rule format is deliberately constrained: each rule has triggers (event types and body-state conditions), an action, a cost estimate, and a confidence. Rules that fire frequently and reliably accumulate priority; rules that misfire are deprioritised. This is intentionally simpler than a learned policy network at the brainstem level, because interpretability and predictability matter more than expressiveness for this layer.

---

## 5. The Cortex: deliberative agent

The cortex is the deliberative layer of AnimaOS. It is invoked when the brainstem cannot handle an event reflexively, when the user explicitly addresses the system, or when an internal process determines that an open question requires reasoning.

Architecturally, the cortex is an agentic loop with tool access — broadly similar to current production agent frameworks (planner, executor, tool calls, observation, plan revision) but with three important differences.

First, the cortex does not own its memory. The memory controller (Section 7) is a separate subsystem that the cortex queries and writes to, with the controller making decisions about what is retained, compressed, or evicted. This separation is the architecture's main technical bet.

Second, the cortex is not always present. It is spun up on invocation and torn down when its work completes. State that needs to persist across invocations is committed to memory via the controller. This is the equivalent of the brain's deliberative processes being event-driven rather than continuous — although in biology the actual mechanism is different (cortical activity persists, but attention and conscious access do not).

Third, the cortex is multi-modal in invocation. Different events route to different cortex configurations — different model selections, different tool subsets, different prompts. A user's coding question and a thermal-stress escalation are both handled by "the cortex" but use different routes through it. This is implemented as a router (Section 6.2) rather than as separate agents, because the alternative — many agents passing messages — is the failure mode the architecture is explicitly trying to avoid.

Cortex implementation will be in Python initially, for ecosystem reasons (LangGraph, instructor, the broader agentic tooling stack). Rust bindings to the cortex from `animad` go over IPC, not FFI, to maintain process isolation. A future port of stable cortex components to Rust is a possibility but not a priority — the cortex is the part of the system most likely to change as agentic tooling evolves, and Python's iteration speed matters more here than its raw performance.

---

## 6. The Gate and Router: arbitration

Between the brainstem and the cortex, and within the cortex itself, sits the arbitration layer. This is the part of the architecture inspired most directly by frontostriatal gating in biological systems, and it is the component that does the most architectural work.

### 6.1 The Striatal Gate

The Striatal Gate is the decision point for cortex invocation. Every candidate cortex invocation — whether triggered by an event, by a brainstem escalation, or by an internal proposal — passes through the gate. The gate produces a binary decision (invoke or defer) and, when invoking, a cost class (cheap local model, mid-tier, frontier).

The gate's inputs are:

- Properties of the candidate event (urgency, novelty, semantic content, user-facing or not)
- Current body state (the homeostatic signals from Section 3.3)
- Recent cortex history (was the cortex recently invoked for something related? how did it go?)
- Current budget state (financial, thermal, temporal)
- A learned value estimate (what does this event seem to be worth?)

The gate is small and fast. The first implementation is a hand-tuned threshold function — explicit, readable, debuggable. The second iteration trains a small model (linear or shallow MLP) on logged outcomes: did invoking the cortex on this kind of event produce a useful result, and was it worth the cost? This is the same pattern as the Fs-LLM cache controller, applied at the system level: a learned, stateful, value-sensitive gate.

The gate is interpretable by design. Every gate decision logs the inputs and the reasoning, and the gate can be inspected, overridden, and audited. A user must be able to ask "why did you wake up just now" and get an answer.

### 6.2 The Thalamic Router

If the gate decides to invoke the cortex, the router decides which cortex. Routes encode:

- Which model is used (local 7B, API mid-tier, frontier model)
- Which tools are available
- Which memory scopes are accessible
- What prompt scaffolding is loaded
- What termination conditions apply

The router is configured statically by default — a table of event types and contexts mapping to route configurations — and can be extended with learned routing for cases where the static mapping is insufficient. This is again deliberately conservative: routing is the part of the system where opacity creates the most operational risk, and the architecture should not commit to learned routing until static routing has been observed to be insufficient.

The combination of gate and router replaces what a conventional agent framework would call "the orchestrator." The split is functional: gate decides whether, router decides how. This makes both decisions independently inspectable.

---

## 7. Memory: the cognitive substrate

Memory is the architecture's most technically distinctive component, and the place where prior work on learned cache control plugs directly in. The memory system is hierarchical, with three tiers operating at different timescales.

### 7.1 Working memory (cache controller)

Inside any single cortex invocation, the working memory is the KV-cache of the language model handling that invocation. For long-running invocations — agentic loops that run for many turns, code sessions, sustained reasoning over tool outputs — the cache becomes the bottleneck that prior research has documented extensively.

AnimaOS uses a learned cache controller derived from the Fs-LLM design: a small recurrent or state-space module that observes hidden states, attention patterns, role flags, and tool-output markers, and produces gating decisions for cache writing and retention. The controller learns to pin user constraints and tool error traces, compress boilerplate, and drop superseded intermediate state. It operates at block or page granularity for hardware efficiency, with token-level features driving the decisions.

**The controller sits on top of TurboQuant-quantised cache storage** (Zandieh et al., ICLR 2026; Qdrant 1.18 production extensions). TurboQuant handles bit-level compression of whatever values are retained — 6× memory reduction at TQ4, near-lossless quality, no calibration set, no retraining — and the controller handles the semantic question of *which blocks are worth retaining at all*. The two are orthogonal: TurboQuant is a fixed substrate; the controller is the learned policy over that substrate. See `07-implementation-plan.md` Epics E2.7 (TurboQuant) and E5.4 (controller over TurboQuant) for the build details.

The controller's training is offline: it is trained against a full-cache teacher on representative agentic traces (long coding sessions, sustained tool use, multi-turn dialogues with the user), with a loss that combines task fidelity (KL against the teacher), cache budget regularisation, and an explicit retrieval-safety objective constructed by adversarially inserting "needle" facts into long contexts and penalising the controller when those facts are forgotten.

The controller is the part of AnimaOS that, if it works, becomes a self-contained technical contribution worth publishing separately. If it does not work, the cache layer still ships TurboQuant compression and a standard eviction policy — long-horizon behaviour is worse than the learned-controller target but markedly better than full-precision LRU, and there is no other architectural disruption.

### 7.2 Episodic memory

Across cortex invocations, the system needs to remember what happened. Episodic memory is the system's record of past episodes: what was the event, how was it handled, what was the outcome, what did the user say, what did the system do.

This is implemented as a structured store (initial implementation: SQLite with a vector index, scaled to LanceDB or similar if needed). Each episode is summarised at the end of the invocation by a small dedicated summariser (a cheap local model is sufficient), with the full trace retained for a configurable window and the summary retained indefinitely.

Episodic retrieval is one of the cortex's standard tools. When the cortex is invoked, the router decides which episodic context, if any, is pre-loaded; the cortex itself can request additional retrieval via the tool surface.

### 7.3 Identity memory

The longest-timescale memory is identity memory: stable facts about the user, the machine, the system's own configuration, and patterns learned over time. This includes user preferences, recurring tasks, observed patterns ("this user runs builds every weekday at 09:30 and the laptop overheats when they do"), system policies, and the system's own self-model.

Identity memory is small (kilobytes to megabytes), explicit (every fact is human-readable), and revisable (the user can inspect and edit it). It is loaded into every cortex invocation as part of the route's standard context.

The decision to keep identity memory explicit and human-readable rather than embedded in model weights is deliberate. A user must be able to see what the system believes about them, and to correct it. Implicit personalisation is the wrong default for a system that lives on the user's machine and acts on their behalf.

---

## 8. The interoceptive loop

The interoceptive loop is the part of the architecture that makes embodiment functional rather than decorative. It is a continuous feedback path from body state to cognitive policy.

The mechanism is straightforward: the homeostatic signals from Section 3.3 are inputs to every policy decision in the system. The gate uses them. The router uses them. The cache controller uses them indirectly via the controller's state. The brainstem uses them most aggressively, because the brainstem is responsible for fast responses to body changes.

Concretely, the loop produces these observable behaviours:

- Under thermal stress, the gate's threshold for cortex invocation rises. Marginal events that would normally invoke the cortex are deferred or handled reflexively. The router shifts toward cheaper models. Background work pauses.
- Under low battery, the router shifts toward local inference where capability permits. The cortex's planning horizon shortens — fewer speculative branches, more conservative actions.
- Under memory pressure, the cache controller becomes more aggressive about eviction. The episodic summariser runs more frequently. Long-running invocations are checkpointed and torn down earlier.
- Under high attention demand from the user, the gate's threshold drops, the router prefers faster models even at quality cost, and the system biases toward responsiveness over thoroughness.
- Under financial budget pressure, frontier model invocations require higher value estimates from the gate.

None of this is "the system feels." The system has policies modulated by body state, and the modulation is mechanical and inspectable. The biological framing is useful here as inspiration — homeostatic modulation of behaviour is a real and well-studied phenomenon in neuroscience — but the implementation is straightforward control engineering.

---

## 9. The immune analogue

Multi-component systems with tool access need a defence layer. In AnimaOS this is treated as a distinct component rather than scattered across other layers, because the failure modes it addresses are categorically different from the operational concerns of the rest of the system.

The defence layer monitors for:

- **External attacks.** Prompt injection in tool outputs, malicious file contents being interpreted as instructions, network responses attempting to redirect the agent.
- **Internal incoherence.** Proposals from one subsystem that contradict identity memory, gate decisions that are wildly out of distribution, cortex outputs that diverge from declared intent.
- **Goal drift.** Long-running tasks whose current actions no longer match their original objective.
- **Reward hacking.** Cortex behaviour that satisfies internal metrics while failing the actual task — for example, marking work as complete without doing it.
- **Unsafe motor actions.** Filesystem operations targeting critical paths, network calls to suspicious destinations, self-modification attempts that fail review.

The defence layer has veto power. A vetoed action is not executed; the cortex is notified of the veto with a reason; the event is logged at a higher severity than routine audit entries. Repeated vetoes within a short window are themselves an event that the gate can escalate to user attention.

The implementation is a mix of explicit rules (the safe defaults) and learned classifiers (for prompt injection detection, in particular). The split mirrors the brainstem's design: explicit and inspectable for the cases where predictability matters most, learned for the cases where pattern detection is the right tool.

---

## 10. The cognitive cycle

The components above compose into a single cognitive cycle that runs continuously while AnimaOS is operating. The cycle is event-driven, not clocked: it advances when something happens, not on a fixed schedule.

The cycle's stages are:

1. **Sense.** An event arrives in the sensorium — user input, filesystem change, telemetry threshold crossing, scheduled trigger, or an internal proposal from a previous cycle.
2. **Triage.** The brainstem evaluates the event. If a reflexive rule applies and is high-confidence, the event is handled and the cycle terminates here. The vast majority of events terminate at triage.
3. **Gate.** Events that survive triage reach the Striatal Gate. The gate decides whether the cortex is invoked, and if so, at what cost class.
4. **Route.** If the gate invokes the cortex, the router selects the cortex configuration: model, tools, memory scope, prompt scaffolding.
5. **Deliberate.** The cortex reasons, possibly takes tool actions, possibly produces sub-events that re-enter the cycle.
6. **Defend.** Cortex outputs and actions are screened by the defence layer. Approved actions proceed; vetoed actions are logged and the cortex is informed.
7. **Act.** Approved actions are executed by the motor system, with full audit logging.
8. **Consolidate.** When the invocation terminates, episodic memory is updated with the summary, the cache controller's behaviour is logged for offline analysis, and any identity-memory updates are committed.
9. **Modulate.** Body state is sampled, homeostatic signals are recomputed, and the next cycle begins with updated thresholds.

The cycle's structure makes the architecture's behaviour traceable. Every action taken by AnimaOS has a corresponding cycle trace showing the event, the triage decision, the gate decision, the route, the cortex's reasoning, the defence review, and the outcome. This is the operational answer to "why did the system do that."

---

## 11. Implementation phasing

The architecture is too large to build all at once. The phasing below sequences construction so that each phase produces a working, useful system rather than scaffolding for a future system.

> The canonical delivery breakdown for this phasing lives in
> `07-implementation-plan.md` under **Stage 5 — Cognitive Layer**
> (epics E5.1–E5.8). The phases below are reproduced for context; the
> epics in the plan supersede them where they differ.

### Phase 1: Body and brainstem (weeks 1–6)

`animad` in Rust, exposing the sensorium and motor interfaces. Brainstem with a small set of hand-written reflexive rules. No cortex yet. The system at this stage is "a privileged daemon that watches the machine and can take basic actions" — useful, inspectable, and a foundation everything else builds on.

Deliverable: a working daemon, IPC interface, audit log, and a small CLI that exposes the sensorium and motor surface for testing.

In the AnimaOS plan, this phase corresponds to Stages 1–3 (already largely complete via `corpus`, `vita`, `scheduler`, `senses`, `interoception`, and the existing memory hierarchy).

### Phase 2: Cortex MVP (weeks 7–12)

A minimal cortex in Python: one model route, a small tool surface, no learned cache controller yet, identity memory as a flat JSON file. The gate is a hand-tuned threshold function. The cortex can be invoked from the brainstem, can call tools, and writes episodes to the episodic store.

Deliverable: an end-to-end system that can be given a task, will reason about it, will use tools, and will produce a result with full audit logging.

In the AnimaOS plan, this phase corresponds to **E5.1, E5.2, E5.3, and the JSON-identity portion of E5.5**.

### Phase 3: Memory controller (weeks 13–20)

Train and integrate the learned cache controller against the cortex's model. This is the most research-heavy phase and has the most uncertainty. The controller is trained offline on logged agentic traces (from Phase 2) plus synthetic long-context tasks with adversarial needle insertions.

Deliverable: cortex invocations with measurably better long-horizon performance under cache budget constraints, benchmarked against baseline cache management.

In the AnimaOS plan, this phase corresponds to **E5.4** and the episodic portion of **E5.5**.

### Phase 4: Interoception (weeks 21–26)

Wire homeostatic signals into the gate, the router, and the brainstem. Implement the modulations described in Section 8. Demonstrate measurable behavioural change under induced stress conditions (thermal, memory, battery).

Deliverable: the kill-shot demo (Section 12) and a writeup characterising the system's behaviour across the body-state space.

In the AnimaOS plan, this phase corresponds to **E5.7 and E5.8**.

### Phase 5: Defence and self-improvement (weeks 27+)

The defence layer is built incrementally throughout, but Phase 5 hardens it. The system gains the ability to update its own configuration under review. The router gains learned routing for ambiguous cases.

Deliverable: a system that can be deployed beyond the developer's own machine with reasonable confidence that it will not embarrass itself.

In the AnimaOS plan, this phase corresponds to **E5.6** and follow-on work tracked outside Stage 5.

This phasing produces a useful artifact at the end of every phase. Phase 1 alone is a viable open-source release. Phase 2 alone is a viable agent framework. Phase 3 is the research contribution. Phase 4 is the product thesis demonstrated. Phase 5 is the path to wider use.

---

## 12. The kill-shot demo

A build spec without a target demo drifts. The architecture above supports several possible demonstrations; one must be chosen to anchor Phase 4.

**Candidate A: Thermal-stress graceful degradation.** Run the same task twice. Once on a cool machine: the cortex engages, uses a frontier model, takes its time, produces high-quality output. Once on a thermally stressed machine (induced by an external compute load): the system handles the same task with cheaper models, shorter context, more reflexive policies, and lower-quality but still-functional output. Side-by-side comparison shows that the system has visibly throttled its own cognition in response to body state. This is the demo that proves embodiment is functional rather than cosmetic.

**Candidate B: Long-horizon coding session retention.** A four-hour coding task with the cortex assisting. Compare against a baseline configuration with standard cache management. The AnimaOS configuration with the learned cache controller demonstrably retains the user's original constraint, the relevant error traces, and the architectural decisions, while the baseline drifts or forgets. This is the demo that proves the memory contribution.

**Candidate C: Proactive system steward.** The system observes a real machine for an extended period and surfaces appropriate interventions: noticing a sustained compile is likely to fail and suggesting a fix, detecting that disk space is filling at an unsustainable rate, identifying a suspicious process. This is the demo that proves the integration of all layers.

The recommendation is **Candidate A as the headline, with B as the technical credibility builder**. A is the demo that captures the architectural thesis in thirty seconds; B is the demo that proves the technical contribution is real and not just rhetorical. C is a longer-term product direction but is hard to make legible in a short demonstration.

Both A and B are tracked in the plan under **E5.8 — Kill-Shot Demonstrations**.

---

## 13. Open questions

The following questions are not yet resolved and will need decisions before the affected phases begin. They are also mirrored in the **Open Decisions** section of `07-implementation-plan.md` so that the plan and the spec stay aligned.

**Distribution.** Is AnimaOS a daemon that runs on existing OSes (Linux, macOS, Windows), a Linux distribution with the cognitive layer pre-integrated, or both? The daemon-on-existing-OS path ships faster and has wider potential adoption. The distribution path allows kernel-level integration (proper scheduling integration, kernel-mediated permission scopes) but is significantly more work. The current default assumption is daemon-on-existing-OS, with a distribution as a possible later direction.

**Local-first vs. API-first.** The architecture is designed to operate with both local and API models, but the default routing policy needs to be decided. Local-first respects user resources and privacy but caps capability at the local model's level. API-first maximises capability but creates ongoing financial and privacy costs. The likely answer is "local-first with explicit user opt-in for API escalation," but this needs to be tested against real workloads.

**Cache controller training data.** Phase 3 depends on having representative agentic traces to train against. These can be collected from Phase 2 usage, but Phase 2 will not produce enough data on its own. Synthetic data generation, public agent trace datasets, and human-in-the-loop curation are all candidate sources, none of them obviously sufficient. This is the largest technical risk in the architecture.

**User-facing surface.** The cognitive layer needs to communicate with the user, but the form of that communication is not yet specified. Desktop notifications, a dedicated UI, a CLI, a chat-style interface, all are plausible. The decision is partly philosophical (what does the system feel like to use?) and partly practical (what gets built first?).

**The privacy and trust model.** A system with this level of access to the user's machine must have a clear story for what it does and does not see, what it sends to external services, and what it commits to long-term memory. The current default is conservative — explicit opt-in for sensitive sensorium streams, explicit opt-in for API model use, all identity memory inspectable and editable — but the details need to be specified before any non-developer use.

---

## 14. What this architecture is not

It is worth being explicit about what AnimaOS is not, because the framing invites several misreadings.

It is not a cognitive architecture in the SOAR or ACT-R tradition. Those are formal theories of cognition with specific commitments to symbolic representation, production systems, and chunk-based memory. AnimaOS borrows the *concept* of a unified cognitive architecture but commits to none of the specific representational claims.

It is not a multi-agent framework in the LangChain or AutoGen sense. There is no agent-to-agent message passing. The cortex is not a committee. Subsystems submit proposals to the gate; the gate arbitrates; this is architecturally different from agents conversing.

It is not a claim about consciousness, sentience, or any related concept. The system has body state and homeostatic modulation; these are useful engineering patterns. Whether the system "feels" anything is not a question the architecture takes a position on, and any framing that suggests otherwise is rhetorical, not technical.

It is not a research project pretending to be a product. The phasing produces shippable artifacts at every stage. The biological inspiration is real but the deliverables are concrete: a daemon, an agent loop, a cache controller, an interoceptive control system. Each of these is independently useful.

It is not a finished design. Several major decisions remain open (Section 13). The architecture is committed to its principles and its overall shape; the details will be adjusted as the build encounters reality.

---

## 15. Summary

AnimaOS is an agent-first operating system structured around a hierarchical cognitive architecture inspired by biological control systems. The OS provides the body — sensorium, motor system, and homeostatic signals. The brainstem layer handles routine events reflexively in always-on Rust code. The cortex layer handles deliberation when escalated to it, with a learned memory controller managing long-horizon context. Between them, the Striatal Gate and Thalamic Router arbitrate when and how the cortex is invoked. Body state continuously modulates the entire stack, producing graceful degradation under stress and considered behaviour under resource constraints.

The architecture is built to be falsifiable. Every claim has a corresponding measurement; every behaviour has an audit trail; every component has a phase in which it is built and tested. The biological framing is a source of intuition, not a constraint on implementation. The technical contributions — the learned cache controller, the homeostatic policy modulation, the gate-and-router arbitration — stand on their own merits.

The headline demonstration is graceful degradation under thermal stress: the system gets less deliberative when its body is taxed, the way any embodied cognitive system does. The technical credibility builder is long-horizon retention through learned cache control. Together these prove the architectural thesis: that intelligence in an agent-first OS is not constant, and that an operating system can have a body it actually inhabits.
