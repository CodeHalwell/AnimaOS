# 21 — Operator Trust & Agent Lifecycle

> **Status:** Proposed (scoping). Target epic: **E15 — Trust & Lifecycle**.
> Branch: `claude/llm-tools-animaos-vuXRK`.
> Related: E5.x (audit log), E11 (self-extension), E12 (motivation),
> E14 (rollback), `crates/console`, `docs/11-operator-interface.md`.

## 0. Goal

The operational layer for *living with* a long-running autonomous agent: let the
operator **trust** it (see what it did, approve what it proposes), **debug** it
(replay its decisions, sandbox changes), and **keep** it (migrate its self
across OS upgrades). As the agent gains autonomy (E11/E12), these stop being
nice-to-haves and become the difference between a trustworthy agent and an
opaque one.

## 1. Current state (grounding)

- **Audit log** is already a durable, structured, append-only JSONL event stream
  (`$ANIMA_AUDIT_DIR/<agent_id>.jsonl`, optional HMAC chain) with 26+
  `AuditEntry` variants — an excellent substrate for replay and digests.
- **`anima why`** explains the *last* decision; there is no longitudinal view.
- **Console** streams live vitals/events but has **no approval-queue** and no
  "what happened while I was away" summary.
- **Identity / adapters / memory** persist to disk, but there is **no versioned
  snapshot or migration** across AnimaOS upgrades.

## 2. Workstreams — Epic E15, stories `S15.x`

### S15.1 — "While you were away" digest

- A periodic, operator-facing **summary** of autonomous activity: goals pursued,
  actions taken, tools/skills used, decisions made and why, anything notable or
  blocked. Generated from the audit log (no new instrumentation) and delivered
  via the operator's preferred channel (E10).
- Tunable cadence + salience (don't narrate every clock tick); ties to the E12
  affect/attention signals for what counts as "notable".

### S15.2 — Approval-queue surface

- The operator-facing half of the E11 promotion gates: a queue of pending
  proposals (new skills, new tools, weight updates) awaiting sign-off, each with
  its provenance, sandbox-test results, defence/eval verdicts, and a
  one-click **approve / reject / rollback**.
- Surfaced in the console and over E10 channels (approve a tool from your phone).
  Every decision audited.

### S15.3 — Decision replay / time-travel debugging

- A harness that **replays the audit log** to step through the agent's past
  decisions deterministically: at any point see the gate inputs, drive
  decomposition (E12), route chosen, tools called, and memory state. Extends
  `anima why` from "the last one" to "any decision, with full context".
- Invaluable for debugging emergent behaviour and for the E13 alignment evals
  (replay a flagged episode to see exactly what drove it).

### S15.4 — Digital-twin sandbox

- A **shadow agent** that mirrors the real agent's state, against which a
  proposed change (a new skill/tool from E11, a fine-tune from E8, a config
  change) can be exercised on recorded/synthetic scenarios **before** it touches
  the live agent. The safe staging ground for self-modification.
- Reuses the CI-hermetic fixture machinery; pairs with S15.3 (replay recorded
  scenarios through the twin).

### S15.5 — State versioning & migration

- A **versioned snapshot** of the whole agent self — identity, skills, adapters
  (E8), knowledge corpus (E14), memory checkpoints — with a schema version.
- A **migration path** so an agent that has "lived" for months survives an
  AnimaOS upgrade: schema migrations transform old snapshots forward; a failed
  migration is recoverable. Also the substrate for E14's whole-agent rollback
  and the S15.4 twin.
- Snapshots are the unit of backup/restore and of moving an agent between hosts
  (container → bare-metal, per `docs/10`).

## 3. How they interlock

```
 audit log ──► S15.1 digest        ──► operator (via E10)
     │     └─► S15.3 replay/debug   ──► E13 evals, debugging
     ▼
 E11 proposals ──► S15.2 approval queue ──► S15.4 twin (test) ──► promote
                                                  │
 agent self ──► S15.5 versioned snapshot ◄────────┘ ──► E14 rollback, host migration
```

## 4. Cross-cutting & dependencies

- **Audit log** is the spine for S15.1/S15.3 — minimal net-new instrumentation.
- **E11** produces the proposals S15.2 gates and S15.4 tests; **E14** consumes
  S15.5 snapshots for rollback; **E8** adapters and **E14** corpus are part of
  the snapshot; **E10** is the delivery channel; **E13** evals consume replay.
- Preserves the operator-interface invariants (`docs/11`): observe + approve,
  never preempt the kernel.

## 5. Open questions

- Digest generation: template/deterministic vs LLM-summarised (and which tier).
- Snapshot size/cadence — full vs incremental, especially with adapters + corpus.
- How faithful the digital twin must be to be trustworthy (full state vs
  behavioural approximation).
- Replay determinism with live LLM backends (record/replay token streams, à la
  the existing fixture mode).
