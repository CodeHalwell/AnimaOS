# 06 — Glossary

Anima uses anatomical and biological vocabulary throughout. This document maps every metaphorical term to its precise engineering meaning, and documents the migration from the earlier Axon OS naming.

The goal is unambiguous: a developer should be able to read any sentence in the documentation, look up any unfamiliar term here, and know exactly what code or system component it refers to.

## 1. Anatomical Terms

| Term | Engineering Meaning | Crate / Location |
|------|--------------------|------------------|
| **Anima** | The whole system; the autonomous-agent OS as a single organism | (project name) |
| **Corpus** | The Trusted Computing Base: privileged kernel code with audited `unsafe` | `crates/corpus` |
| **Vita** | The lifecycle director — wake/sleep state machine and policy interpretation | `crates/vita` |
| **Praxis** | The efferent (output) subsystem: tool dispatch, MCP/A2A buses, sandboxes | `crates/praxis` |
| **Senses** | The afferent (input) subsystem: stream parsers for text, voice, RPC | `crates/senses` |
| **Self** | The capability and identity system; the immune system equivalent | `crates/self` |
| **Interoception** | Real-time internal-state monitoring; the stress index | `crates/interoception` |
| **Memory** | The three-tier context/cache/archive system | `crates/memory` |
| **Scheduler** | The MLFQ task scheduler (kept neutral; "reflex" was reserved for the loop) | `crates/scheduler` |

## 2. Lifecycle Vocabulary

| Term | Engineering Meaning |
|------|--------------------|
| **Waking** | The macro-state during which the agent is dispatching tasks and responding to input |
| **Sleeping** | The macro-state during which the agent is performing internal maintenance |
| **Pruning** | First sleep phase: applies emotional decay to L1/L2, evicts or compresses below-threshold entries |
| **Replay** | Second sleep phase: re-runs sampled past questions to validate that pruning hasn't degraded knowledge |
| **Dreaming** | Third sleep phase: random graph walks across L3 to discover new associative edges |
| **Compilation** | Fourth sleep phase: compiles waking-state traces into training data formats |
| **Emergency consolidation** | Stress-triggered rapid pruning during the Waking state; bypasses the full sleep cycle |
| **Homeostatic loop** | The continuous Waking-state loop that integrates sensory input, stress monitoring, and task dispatch |

## 3. Memory Vocabulary

| Term | Engineering Meaning |
|------|--------------------|
| **L1 / Working Context** | Tokens mapped into the model's active attention field |
| **L2 / Warm Memory Cache** | RAM-resident concurrent hashmap of recent tokens and KV-cache blocks |
| **L3 / Cerebral Archival Store** | Embedded LanceDB vector store; persistent across restarts |
| **Semantic floor** | The minimum activation value (default 0.3) below which decay does not pull entries; protects high-generation knowledge from erosion |
| **Arousal** | Emotional weighting scalar $[0, 1]$ assigned at memory formation; modulates decay rate |
| **Surprise** | Emotional weighting scalar $[0, 1]$ assigned at memory formation; weighted more heavily than arousal by default |
| **Associative edge** | A connection between two L3 entries discovered during Dreaming and validated during Pruning |
| **Audit stream** | The dedicated L3 namespace containing capability operations and lifecycle events; emotional weighting prevents decay |

## 4. Praxis Vocabulary

| Term | Engineering Meaning |
|------|--------------------|
| **Tool driver** | A handler for a single tool, exposed as a file under `/dev/anima/praxis/tools/` |
| **Circuit breaker** | Per-tool state monitor that blocks invocation after repeated failures |
| **Length-robust relative routing** | The filter that admits tools by relative score (τ_rel × max) rather than absolute threshold |
| **MCP** | Model Context Protocol — exposed as remote tools under `/dev/anima/praxis/tools/mcp/<server>/` |
| **A2A** | Agent-to-Agent protocol — peer agents exposed as remote tools under `/dev/anima/praxis/tools/a2a/<peer>/` |
| **Sandbox** | A wasmtime instance with gas metering, memory bounds, and capability-typed imports |

## 5. Interoception Vocabulary

| Term | Engineering Meaning |
|------|--------------------|
| **Stress index** | Composite scalar $[0, 1]$ combining latency degradation and context saturation |
| **TTFT** | Time to First Token; the primary latency signal |
| **Baseline TTFT** | The reference latency value used to compute the latency ratio |
| **β (beta)** | Weighting parameter balancing latency pressure against memory pressure in the stress index |
| **Telemetry stream** | The continuous output of system metrics, primarily consumed by `vita` for policy decisions |

## 6. Self Vocabulary

| Term | Engineering Meaning |
|------|--------------------|
| **Capability** | A typestate-pattern Rust value granting a specific permission |
| **Role** | A build-time-fixed identity (e.g., `consolidator`, `responder`) that determines which capabilities are issued |
| **Elevation token** | A single-use value that upgrades a restricted capability to an unrestricted one |
| **Self/non-self barrier** | The capability system as a whole; prevents tasks from acting outside their granted permissions |

## 7. Sensory Vocabulary

| Term | Engineering Meaning |
|------|--------------------|
| **Afferent** | Input direction: from the environment into the agent |
| **Efferent** | Output direction: from the agent into the environment |
| **Sensory event** | A parsed input wrapped in a common envelope with source, timestamp, priority, and payload |
| **Sensory node** | A mount point under `/dev/anima/senses/` corresponding to one input source |
| **Priority** | A driver-level tag determining how aggressively the agent should attend to an event |

## 8. Why the Metaphor

A note on philosophy, for anyone wondering why the documentation is full of nervous systems.

The vocabulary is load-bearing. It is not decoration. The anatomical terms describe the actual structure of the system in ways the conventional OS vocabulary does not:

- "Afferent" and "efferent" make the input/output asymmetry explicit. "I/O" does not.
- "Interoception" distinguishes internal-state monitoring from external telemetry. "Metrics" does not.
- "Pruning, dreaming, compilation" capture what the sleep phases actually do. "Background tasks" does not.
- "Capability" was already a term of art in OS security; we keep it. "Soul" or "essence" would be silly. We use anatomy where it clarifies and engineering vocabulary where it suffices.

The risk of an overcooked metaphor is real. If you find a passage in the documentation where the metaphor is doing more work than the engineering, flag it. Anatomy serves the architecture, not the other way around.

## 9. Migration from Axon OS

The earlier Axon OS specification used a different vocabulary. The Anima codebase initially landed under engineering-flavoured names (`kernel-core`, `lifecycle`, `toolbus`, `sensory-bridge`, `security`, `observe`) and has since been renamed to the anatomical names listed below. The renames are complete; all imports and Cargo packages use the new names.

| Axon OS Term | Anima Term | Notes |
|--------------|-----------|-------|
| Axon (system name) | Anima | An axon is only the output fibre of a neuron; "anima" captures the whole organism |
| Autonomic Substrate | Corpus | Same concept, named after the body itself |
| Somatic Layer | (unchanged conceptually) | The agent runtime; no rename, the term is accurate |
| `kernel-core` | `corpus` | Crate rename, complete |
| `lifecycle` | `vita` | Crate rename, complete |
| `toolbus` | `praxis` | Crate rename, complete |
| `sensory-bridge` | `senses` | Crate rename, complete |
| `security` | `self` (package: `anima-self`) | Directory rename complete; the Cargo package is `anima-self` because `self` is a reserved Rust keyword |
| `observe` | `interoception` | Crate rename, complete |
| `/dev/sensors/human` | `/dev/anima/senses/human` | Path rename; nested under the new namespace |
| `/dev/tools/` | `/dev/anima/praxis/tools/` | Path rename; nested under the new namespace |

The technical content of the original Axon specification carries through unchanged. The split between TCB and policy layers, the three-tier memory model, the homeostatic loop, the verification posture, and the roadmap are all preserved. Only the names and the framing have shifted.

## 10. Quick Reference

If you need to identify a component by its crate name:

```
anima         = the whole project
corpus        = TCB / autonomic substrate
vita          = lifecycle director
praxis        = efferent actuator core (tool bus)
senses        = afferent sensory bridge
self          = capability / identity (Cargo package: anima-self)
interoception = stress index and internal telemetry
memory        = three-tier CLS memory hierarchy
scheduler     = MLFQ task scheduler and token pipe
```

If a term in the documentation isn't in this glossary, that's a bug. File an issue.
