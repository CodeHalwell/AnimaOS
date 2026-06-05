# 16 — Skills & Self-Extension

> **Status:** Proposed (scoping). Target epic: **E11 — Self-Extension**.
> Branch: `claude/llm-tools-animaos-vuXRK`.
> Related: E7 (tools + semantic selection), E8 (providers/Unsloth), E10 (comms),
> `docs/09-threat-model.md`, `crates/defence`.

## 0. Goal

Give the agent a **Skills** system following the Anthropic Agent Skills model,
and the ability to **register its own new skills and new tools** so it can
improve how it does things over time — all behind the existing self-modification
safety boundary. This is the project's self-improvement loop, and it is the
highest-capability / highest-risk surface in the roadmap, so safety framing is
load-bearing throughout.

## 1. Background: the Anthropic Agent Skills model

A **Skill** is a folder containing a `SKILL.md` with YAML frontmatter and
optional bundled resources/scripts:

```
my-skill/
  SKILL.md          # frontmatter: name, description; body: instructions
  reference.md      # optional, loaded on demand
  scripts/run.py    # optional executable helper
```

The defining property is **progressive disclosure**:

1. **Always loaded:** only the `name` + `description` (cheap — a few tokens).
2. **Loaded when relevant:** the `SKILL.md` body (the procedure), when the model
   judges the skill applies.
3. **Loaded on demand:** linked files / scripts, only if the procedure needs them.

Skills are **model-invoked** (the model decides when to use one) and composable.
This maps cleanly onto AnimaOS's existing pieces: progressive disclosure is the
same shape as the **E7 semantic tool filter** (`length_robust_filter` over
descriptions), and skill scripts are the same shape as **praxis tools**.

## 2. Current state (grounding)

- **No skill concept exists** in the repo today.
- **Tools** are static `ToolDriver` impls compiled in (`praxis`); the registry
  *can* register at runtime (`ToolRegistry::register`) but nothing authors tools
  dynamically.
- **Self-modification is already gated:** `UnsafeMotorActionGate::
  screen_self_modification` blocks changes unless `allow_self_modification` is
  set **and** a verified `self.modify` / `self.*` capability is present
  (`crates/defence/src/motor_gate.rs`). This is the gate every self-extension
  must pass.
- **Sandboxing exists:** `praxis::WasmSandbox` runs untrusted code fuel-metered,
  memory-capped, and capability-gated — the substrate for agent-authored tools.
- **Consolidation/"dreaming"** sleep phases (E5.x) are where reflection on
  recurring needs naturally lives.

## 3. Workstreams — Epic E11, stories `S11.x`

### S11.1 — Skill registry & progressive disclosure

- `SkillRegistry` scanning `~/.anima/<agent_id>/skills/**/SKILL.md`. Parse
  frontmatter (`name`, `description`, optional `version`, `capabilities`).
- Expose **only metadata** to the cortex by default; load the body when a skill
  is selected, and linked files only when the body references them — true
  three-stage progressive disclosure.
- Skill selection reuses **E7 S7.3**: embed each skill `description`, score
  against the task, `length_robust_filter` → candidate skills. One scorer serves
  both tools and skills.

### S11.2 — Built-in / bundled skills

- Ship a starter set authored by us (not the agent): the **onboarding-interview**
  skill (E9 S9.2), a **web-research** skill (orchestrates E7 search+browse), a
  **summarise-and-archive** skill (L3), and a **draft-a-tool** meta-skill
  (feeds S11.4). Bundled, read-only, versioned with the repo.

### S11.3 — Agent-registered **skills** (lower risk)

- The agent authors a new `SKILL.md` (instructions/prompts only — no new
  executable surface). Flow:
  draft → **defence screen** (injection / goal-drift / reward-hacking on the
  skill text) → **self-modification gate** (`self.modify` capability) →
  write to a `proposed/` dir → **promotion** (operator approval *or* an
  auto-approve policy for prompt-only skills within a sandboxed scope) →
  active.
- Provenance + versioning + one-command rollback. Every step audited
  (`AuditEntry::SkillRegistered` / `SkillPromoted` / `SkillRolledBack`).

### S11.4 — Agent-registered **tools** (higher risk)

- The agent authors a new tool as a **WASM module** run under
  `praxis::WasmSandbox` (never native code). Flow:
  draft (codegen via the cortex) → compile to WASM → **execute in sandbox with
  fixtures** (fuel/memory/capability limits) → **defence eval** → **self-mod
  gate** → `proposed/` → **operator approval required** (tools are an
  action surface; default is human-in-the-loop, not auto-approve) → register in
  `ToolRegistry` with its own circuit breaker.
- The new tool's `ToolSpec.description` is embedded so it immediately joins the
  E7 semantic-selection candidate pool — the loop closes: *the agent extends the
  very set of tools E7 selects from.*
- Hard constraints: WASM-only, capability bitmask scoped to exactly what the
  tool declared, egress still through the E7 guard, no filesystem-critical
  access without a verified capability.

### S11.5 — The self-improvement loop

- During the **dreaming/consolidation** sleep phase, the agent reflects on the
  episode log (E5.x) for recurring friction ("I keep hand-assembling the same
  three tool calls"), and proposes a new skill (S11.3) or tool (S11.4) to
  collapse it. Proposals queue for the next wake's promotion gate.
- Optional deeper rung: pair with **E8 Unsloth** to fine-tune behaviour the
  same way — but model/weight changes are the strictest-gated of all (separate
  eval, never auto-promoted).

### S11.6 — Capability, provenance & rollback substrate

- Every skill/tool carries provenance (`authored_by: builtin | operator | agent`,
  timestamp, source episode) and a declared capability set checked against
  `anima-self`.
- A registry-wide **kill switch** and per-artifact rollback; a "quarantine" state
  for anything whose circuit breaker trips repeatedly or that defence flags
  post-promotion.

## 4. Safety model (non-negotiable)

| Risk | Control |
|---|---|
| Agent writes malicious/native code | **WASM-only** sandbox; no native tool authoring; fuel/memory/cap limits. |
| Privilege escalation | Capability bitmask scoped to declaration; `self.modify` required to register; `anima-self` typestate. |
| Prompt-injection authoring a skill | Defence injection detector screens skill/tool source before promotion. |
| Goal drift via self-rewrites | Goal-drift monitor + operator promotion gate for tools (human-in-the-loop default). |
| Silent capability creep | Full provenance, audit, kill switch, rollback, post-promotion monitoring. |
| Reward hacking ("I made a tool" w/o real work) | Reward-hacking detector requires observable evidence (sandbox test results). |

Default posture: **skills (prompt-only) may auto-promote within a sandboxed
scope; tools (action surface) require explicit operator approval.** Both are
gated, sandboxed where executable, and fully reversible.

## 5. Cross-cutting & dependencies

- **E7 S7.3** semantic selection is reused verbatim for skill/tool discovery and
  auto-extended by S11.4.
- **E7 defence-at-dispatch + egress guard** apply to agent-authored tools.
- **E9** onboarding ships as a skill (S9.2 ↔ S11.2).
- **E8 Unsloth** is the weight-level analogue of self-extension (S11.5).
- Reuses `WasmSandbox`, `CircuitBreaker`, `UnsafeMotorActionGate`, `anima-self`,
  and the audit log — minimal net-new safety machinery.

## 6. Open questions

- Auto-promotion scope for prompt-only skills: how wide before human review.
- Tool authoring: cortex-codegen-to-WASM toolchain choice (e.g. compile a
  restricted DSL vs full language → WASM).
- Where skills live on the bare-metal target (no `~/.anima` filesystem).
- Whether weight-level self-improvement (S11.5 Unsloth rung) is in-scope for v1
  or deferred behind a longer safety review.
