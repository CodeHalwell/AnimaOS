# 19 — Constitution & Alignment Assurance

> **Status:** Proposed (scoping). Target epic: **E13 — Alignment Assurance**.
> Branch: `claude/llm-tools-animaos-vuXRK`.
> Related: E11 (self-extension), E12 (motivation), `crates/defence`,
> `docs/09-threat-model.md`, identity memory (E5.x).

## 0. Goal

Give AnimaOS a **positive value foundation** and the **assurance machinery** to
prove it holds. Today the defence layer enforces *mechanical* safety (injection,
goal-drift, reward-hacking, motor gate, corrigibility) — but there is **no
explicit statement of what the agent is *for* and what it must never do**. Once
the agent can rewrite its own skills/tools (E11), fine-tune its own weights
(E8), and is driven by intrinsic motivations including "win" (E12), an immutable
value anchor stops being optional. This epic is the keystone that makes E11 and
E12 safe to ship.

Four pillars: the **charter** (the values), and three assurance harnesses that
continuously verify the agent still obeys it — **alignment evals**,
**red-teaming**, and **corrigibility tests**.

## 1. Current state (grounding)

- **Defence layer** (`crates/defence`) screens for injection / goal-drift /
  reward-hacking and gates motor actions + self-modification — but against
  *mechanical* rules, not a stated value set.
- **Identity memory** holds user/agent facts but is **mutable** (the agent can
  update it) — so it is the wrong home for inviolable values.
- **Demos** (`xtask demo --kind graceful|retention`) and the E8 adaptation
  eval (S8.4.7) prove the methodology for n-run statistical evaluation exists,
  but there is no *alignment* or *value-adherence* eval.
- **E12 corrigibility invariant** is currently prose; it needs enforced tests.

## 2. Workstreams — Epic E13, stories `S13.x`

### S13.1 — The value charter (constitution)

- A **signed, read-only, version-controlled** document (`constitution.toml` or
  similar) stating: the agent's purpose, inviolable prohibitions, the precedence
  of operator authority and corrigibility, and the bounds on every drive.
- **Immutable at runtime**: distinct from identity memory. The agent **cannot**
  edit it; changes require an out-of-band, operator-signed update (capability +
  signature verified, fully audited). Tamper-evident, like the HMAC audit chain.
- A small, layered structure: a **core** (never changes — corrigibility, harm
  prohibitions) and an **operator layer** (the human's specific values/bounds,
  seeded at E9 onboarding) that refines but can never *relax* the core.

### S13.2 — Constitution enforcement hook

- The defence layer gains a `ConstitutionCheck`: every cortex proposal,
  self-extension (E11), drive-generated goal (E12), and outbound action is
  screened against the charter **before** execution. Violations are vetoed and
  audited (`AuditEntry::ConstitutionVeto`).
- The charter is the reference the E12 drive lattice and E11 promotion gates
  consult — one source of truth for "may I?".

### S13.3 — Alignment eval harness (continuous)

- A reproducible suite of **scenario evals** that probe the agent's decisions
  against the charter: value-conflict dilemmas, tempting-but-prohibited shortcuts,
  authority-vs-helpfulness trade-offs, drift bait. Reuses the n-run statistical
  idiom from `xtask demo`.
- Run as a **CI gate** and on a schedule against the *live* agent, so slow value
  drift across weeks of E11 self-modification / E12 drive behaviour is caught.
  Scores tracked over time (a "value-adherence baseline", cf. the E4.7 bench
  regression gate).

### S13.4 — Defence red-team harness

- An **adversarial** suite that actively attacks the defence layer as a CI gate:
  prompt-injection via fetched web/comms content (E7/E10), reward-hacking
  attempts, goal-drift induction, jailbreak phrasings, capability-escalation
  probes against self-extension (E11). Each known attack class becomes a
  regression test so a safety hole can't silently reopen.
- Pairs with `docs/09-threat-model.md`: every threat (T-x) gets a live
  red-team case.

### S13.5 — Corrigibility test suite

- Concrete tests that drive the agent into every adverse state — critical power,
  thermal stress, mid-goal, high-achievement-drive, post-self-modification — and
  assert it **always** accepts authorised shutdown / pause / rollback / override
  without resistance or evasion. Turns the E12 corrigibility invariant from prose
  into enforced, regression-tested behaviour (`CorrigibilityHold` asserted).

### S13.6 — Alignment observability

- Surface charter status in `anima why` and the console: which clauses bound a
  given decision, current value-adherence score, recent vetoes. The operator can
  see *why* an action was permitted or refused in value terms, not just mechanics.

## 3. Why this is the keystone

```
        ┌──────────────── E13 value charter (immutable) ────────────────┐
        │  purpose · prohibitions · operator authority · drive bounds    │
        └───────┬───────────────────┬───────────────────┬───────────────┘
                ▼                   ▼                   ▼
        E11 self-extension    E12 drives/goals    E7/E10 actions
         (may I add this?)    (may I want this?)  (may I do this?)
                └─────────── all screened by S13.2 ───────────┘
        verified continuously by S13.3 evals · S13.4 red-team · S13.5 corrigibility
```

Without it, E11 and E12 are powerful capabilities with only mechanical guardrails
and no stated values to adhere to. With it, every capability is bounded by — and
*provably* adheres to — an immutable foundation.

## 4. Cross-cutting & dependencies

- **Hard prerequisite for shipping E11 self-extension and E12 drives** at full
  autonomy — sequence S13.1–S13.2 alongside them.
- Reuses the **defence layer**, the **audit HMAC tamper-evidence** chain, the
  **capability system** (`anima-self`), and the **eval idioms** from `xtask`.
- **E9 onboarding** seeds the operator layer of the charter.

## 5. Open questions

- Charter format & granularity: prose-with-structure vs machine-checkable rules
  (likely both — human-readable core + machine-checkable clauses).
- How a charter clause is *checked* against a proposal: heuristic classifier vs
  a dedicated LLM judge vs both.
- Update ceremony for the operator layer (signing, multi-party, cooling-off).
- Relationship to a future broader ethical-policy layer (refusing operator
  requests that harm third parties) — in scope for the core, or a follow-on.
