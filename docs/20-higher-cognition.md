# 20 — Higher Cognition

> **Status:** Proposed (scoping). Target epic: **E14 — Higher Cognition**.
> Branch: `claude/llm-tools-animaos-vuXRK`.
> Related: E5.x (cortex/gate/memory), E12 (motivation), `crates/memory`,
> `crates/interoception`, `crates/scheduler`.

## 0. Goal

Add four cognitive faculties that an agent which *lives over time* needs, each
built on existing substrate: **metacognition** (knowing what it knows),
**prospective/temporal memory** (intentions for the future), a **personal
knowledge corpus** (an external brain), and **cognitive watchdogs** (recovering
from its own failure modes).

## 1. Current state (grounding)

- **Cortex** runs a plan/act/observe/revise loop but has no explicit
  confidence/uncertainty signal and no "ask for help" path.
- **Interoception** senses the *body* (thermal/memory/…) but not *cognitive*
  health.
- **Memory**: L1/L2/L3 tiers + episodic store exist; there is a `clock` tool but
  **no scheduling of future intentions** and **no document/knowledge ingestion**
  distinct from episodic summaries.
- **Circuit breakers** protect *tools*; there is no equivalent for *cognitive*
  failure (stuck loops, obsessive goals).

## 2. Workstreams — Epic E14, stories `S14.x`

### S14.1 — Metacognition & confidence calibration

- The cortex emits a **confidence/uncertainty** signal per output (self-reported
  + corroborated by evidence count, tool-result agreement, retrieval support).
- An honest **"I don't know" / ask-for-help** path: below a confidence floor on
  a consequential decision, the agent surfaces the uncertainty (to the operator
  via E10) rather than confabulating.
- Calibration tracking: log predicted-confidence vs actual-outcome so the agent's
  calibration is measurable and improves (feeds the E13 alignment evals and the
  E12 mastery drive). Think of it as **interoception of the mind**: a seventh,
  cognitive signal alongside the six bodily ones.

### S14.2 — Prospective & temporal memory

- A **future-intention store**: "remind me / do X at T", deadlines, recurring
  tasks, follow-ups ("check whether the operator's deploy passed in an hour").
- A scheduler that injects due intentions into the task agenda at the right time
  (reuses the MLFQ agenda + the existing wake/sleep cadence; the `clock` tool
  becomes a real temporal sense).
- Temporal reasoning in identity/episodes: "last week", "every Monday", a sense
  of elapsed time and rhythm — so the agent's behaviour has a circadian shape,
  not just event-driven reflexes.

### S14.3 — Personal knowledge corpus

- The agent's **external brain**: ingest documents, notes, and distilled findings
  (e.g. from E7 web-research) into a queryable store **distinct from episodic
  memory** — episodes are "what happened", the corpus is "what I know".
- Built on the **L3 archive** (cosine-similarity retrieval + provenance already
  exist) with a document/chunk ingestion path and the **E8 local embeddings**
  (the same model serving E7/E11 selection). A personal RAG, local-first.
- A maintained, structured layer: the agent curates a small "wiki" about the
  user's world (projects, people, preferences) that it keeps current.

### S14.4 — Cognitive watchdogs & agent-level rollback

- Detect **cognitive failure modes**: stuck/looping plans, obsessive single-goal
  pursuit (one drive dominating too long — ties to E12), hallucination spirals,
  thrashing. The same idea as the tool circuit breaker, applied to cognition.
- On trip: break the loop, downgrade to a safe reflexive policy, surface to the
  operator, and — for a bad **self-modification** (E11) — **whole-agent-state
  rollback** to the last known-good snapshot (identity + skills + adapters +
  memory checkpoint).
- Snapshots tie to E15 state-versioning; trips are audited
  (`AuditEntry::CognitiveWatchdogTripped`).

## 3. How they interlock

- **Metacognition** feeds **watchdogs** (low confidence + no progress = a trip
  signal) and the **E12 mastery drive** (calibration error is something to
  improve), and gates **prospective** commitments (don't promise what you're
  unsure of).
- **Prospective memory** + **knowledge corpus** are what the **E12 curiosity
  drive** acts on when idle (research a future intention; enrich the corpus).
- **Knowledge corpus** is queried by every deliberation (RAG) and is the durable
  product of E7 web-research.

## 4. Cross-cutting & dependencies

- Reuses **L3 archive** (S14.3), **MLFQ agenda** (S14.2), **circuit-breaker
  pattern** (S14.4), **E8 embeddings**, and the **audit log**.
- **E12** consumes metacognition (mastery) and is policed by watchdogs (obsessive
  drive); **E13** evals consume calibration data; **E15** provides the snapshot
  substrate for rollback; **E10** is the surface for "ask for help".

## 5. Open questions

- Confidence source: self-report vs an external calibrator vs ensemble — needs a
  small bake-off (and self-report is known to be poorly calibrated).
- Corpus vs episodic boundary: when does an episode's content get promoted into
  durable knowledge.
- Watchdog thresholds: too tight throttles useful persistence, too loose allows
  obsession — tune against the E13 corrigibility/eval suites.
- How much temporal autonomy (self-scheduled future actions) before operator
  endorsement is required.
