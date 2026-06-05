# 18 — Forward Epics: Index, Dependencies & Sequencing

> **Status:** Living index. Branch: `claude/llm-tools-animaos-vuXRK`.
> This catalogues the **proposed forward epics E7–E12** (docs 12–17) that build
> the autonomous-agent layer on top of the shipped somatic core (E1–E6).

## 0. Numbering

Shipped epics occupy **E1–E6** (E6 = Operator Console, `docs/11`) plus the
cross-cutting **EX** series. The forward work continues at **E7** and up. Doc
file numbers (12–18) are a separate sequence from epic numbers — the table below
maps them.

## 1. The forward epics

| Epic | Doc | Theme | One-line scope |
|---|---|---|---|
| **E7 — Embodiment** | [12](./12-real-world-tools-plan.md) | Real-world tools | web-search (SearXNG) + browser (Playwright), egress/SSRF guard + motor-gate-at-dispatch, semantic tool selection wired to `length_robust_filter`, live Anthropic/Ollama tool-calling. |
| **E8 — Local Inference** | [13](./13-local-llm-providers.md) | Provider ecosystem + fine-tuning | OpenAI-compatible umbrella (vLLM/LM Studio/NVIDIA NIM/HF TGI/llama.cpp-server), native FFI runtimes (llama.cpp, LiteRT-LM), Unsloth as the default trainer, HRA for the instinct tier, eval harness, adapter library + dynamic mounting. |
| **E9 — Onboarding** | [14](./14-onboarding.md) | First-run experience | `anima init` wizard, `anima doctor` preflight, conversational identity bootstrap, non-NVIDIA/CPU/Apple-Silicon support, per-tier router dispatch, unified quickstart. |
| **E10 — Presence** | [15](./15-communication-multimodal.md) | Communication & multimodal | comms-app channel gateways (Telegram/Slack first) over the existing operator seam; text/image/voice as first-class bidirectional modalities (vision, whisper.cpp STT, Piper TTS). |
| **E11 — Self-Extension** | [16](./16-skills-and-self-extension.md) | Skills & self-improvement | Anthropic Agent Skills model (progressive disclosure); agent-registered skills (prompt-only) and tools (WASM-sandboxed, operator-approved); dreaming-phase self-improvement loop. |
| **E12 — Motivation** | [17](./17-motivation-and-drives.md) | Drives & objectives | six-tier drive hierarchy (viability → self-actualisation) feeding the Striatal Gate `value_score`; endogenous goal generation; corrigibility invariant above the lattice. |

## 2. The shared spine (why these interlock, not stack)

The epics deliberately reuse a small set of primitives rather than each
inventing its own. Building these once, well, is the real work:

| Shared primitive | Defined/extended in | Reused by |
|---|---|---|
| **Chat + tool-calling `LlmBackend` extension** | E7 §, E8 §5 | E7 (live tool-calling), E8 (all providers), E9, E10 |
| **`length_robust_filter` semantic selector** (exists in `praxis`) | E7 S7.3 | E7 (tools), E11 (skills + tools), E8 (adapters) |
| **Egress guard + motor-gate-at-dispatch** | E7 S7.0 | E10 (channel calls), E11 (agent-authored tools) |
| **Local embeddings** | E8 (providers) | E7 selector, E11 selector, L3 memory |
| **Subprocess-IPC pattern** (`ChildGuard`, length-prefixed JSON/UDS — exists) | cortex bridge | E7 (Playwright), E8 (transformers sidecar), E10 (gateways) |
| **`WasmSandbox` + `UnsafeMotorActionGate` + `anima-self`** (exist) | `praxis`/`defence` | E11 (self-extension), E12 (corrigibility) |
| **Striatal Gate `value_score`** (exists) | E5.2 | E12 (drives feed it) |
| **Adapter library + provenance** | E8 S8.4.8 | E11 (mastery), E12 (competence accretion) |
| **Identity memory** (exists) | E5.x | E9 (bootstrap), E12 (operator objectives) |

## 3. Dependency graph

```
        ┌─────────────── shared LlmBackend chat/tool-calling extension ───────────────┐
        ▼                                                                              ▼
  E7 Embodiment ◄───── local embeddings, providers ─────► E8 Local Inference
   │  │  │                                                  │   │
   │  │  └── egress guard ──► E10 Presence ◄── vision/STT/TTS ┘   └── adapters ─┐
   │  └──── semantic selector ──────────────┐                                    │
   │                                         ▼                                    ▼
   └──── live cortex ──► E9 Onboarding ◄── skills ── E11 Self-Extension ◄─────────┘
                              │                  │ (sandbox, defence, gate)
                              └── preferred channel (E10), seeds objectives ──┐
                                                                              ▼
                                                                        E12 Motivation
                                                            (capstone: feeds the gate,
                                                             acts via E11, accretes via E8)
```

Reading it: **E7 + E8 are the foundation** (and share one trait extension);
**E11 self-extension** sits on E7's selector/sandbox + E8's adapters; **E9/E10**
are the user-facing layers; **E12 motivation** is the capstone that integrates
everything into the existing gate.

## 4. Recommended build sequence

1. **Shared `LlmBackend` chat + tool-calling extension** — backward-compatible
   default methods. Unblocks E7 and E8 simultaneously; do it first.
2. **Foundational vertical slice:** E8 S8.0–S8.1 (provider substrate +
   OpenAI-compatible umbrella) **with** E7 S7.0–S7.1 (egress guard + web-search +
   semantic selector, BM25 scorer first). Proves
   `cortex → select → gate → tool → result` end-to-end, CI-hermetic.
3. **Complete E7/E8 cores:** E7 browser (S7.2); E8 native runtimes + Unsloth/HRA
   + adapter library (S8.3–S8.4). Land **per-tier router dispatch** (E9 S9.5 /
   E8 §4) here — many things need it.
4. **E11 Self-Extension** — reuses E7's selector + sandbox + defence; gated.
5. **E9 Onboarding + E10 Presence** — user-facing layers on the now-real stack.
6. **E12 Motivation** — capstone; integrate drives into the gate, wire the
   dreaming-phase endogenous-goal loop, accrete competence via E8 adapters.

## 5. Open cross-epic decisions

- Confirm crate placement: `crates/actuators` (E7 network tools) and where E8
  providers/adapters live relative to `llm-backends`.
- First comms channels (E10): Telegram + Slack recommended.
- Auto-promotion scope for prompt-only skills (E11) vs human-in-the-loop.
- Mid-tier backend binding (E8 §4): Ollama-large vs Claude Haiku.

## 6. Noted but unscoped (candidate E13+)

Surfaced during design; **not yet scoped** — flagged so they are not lost:

- **Value charter / "constitution"** — an immutable value foundation the drive
  system (E12) and self-extension (E11) are anchored to and **cannot rewrite**.
  Arguably the most important safety gap once an agent can self-modify *and* has
  intrinsic drives; the defence layer enforces *mechanical* safety but there is
  no positive value anchor yet.
- **Continuous alignment & cognitive-health evaluation** — ongoing verification
  that the agent stays aligned and cognitively healthy across weeks of
  self-modification (E11) and drive-driven behaviour (E12); metacognition /
  confidence calibration.
- **Trust, human-identity & privacy** — authenticating *which human* is messaging
  (E10 channels), multi-user/relationship model, consent UX, and data governance
  for an agent brokering a person's private life.
