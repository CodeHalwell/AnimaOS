# 17 — Motivation & Objective System (the Drive Architecture)

> **Status:** Proposed (scoping). Target epic: **E12 — Motivation**.
> Branch: `claude/llm-tools-animaos-vuXRK`.
> Related: E5.2 gate, E5.7 interoceptive modulation, `crates/interoception`,
> `crates/defence` (goal-drift / reward-hacking / motor gate), E8 (adapters),
> E10 (comms), E11 (self-extension), `docs/08-cognitive-architecture.md`.

## 0. Goal

Give the agent a **hierarchy of objectives** modelled on living systems: low-level
drives that keep it *functioning* (energy, thermal, memory, solvency), and
higher-cognition drives that make it *want* things — to explore, learn, discover,
master, accomplish, and serve its human. Crucially, this is built **on the value
machinery that already exists**, not beside it, and it is bounded by a hard
alignment/corrigibility constraint so "animal-like" self-preservation never
becomes resistance to correction.

## 1. The central thesis: motivation *is* the value function

AnimaOS already has the seams a drive system plugs into:

| Existing piece | What it already does | What motivation adds |
|---|---|---|
| **Striatal Gate** `value_score` (`gate.rs`) | scores each event from urgency + novelty + user-facing/operator bonuses | becomes the **integration point**: drives contribute to `value_score` |
| **Six interoceptive signals** (`interoception`) | thermal/compute/memory/power/financial/attention, 1 Hz | **are** the Tier-0 "keep functioning" drives, already sensed |
| **Brainstem reflexes** (`docs/08 §4`) | thermal→pause bg work, battery→local routing | become the reflexive arc of the viability drives |
| **E5.7 modulation** | resource pressure downgrades routes | precedent for **state-dependent drive weighting** |
| **GoalDriftMonitor** (`defence`) | flags drift from intended goals | implies goals exist; motivation makes them **explicit + auditable** |

So motivation = **(a)** a value function that turns drive states into the
`value_score` the gate already consumes, plus **(b)** an *endogenous goal
generator* that gives the agent things to pursue when no operator task is
pending. Per `docs/08`'s ethos — *"interpretable global modulation, not a
high-dimensional feature vector"* — the drive set stays small, scalar, and
hand-tuned first, with a learned value model as a later iteration (mirroring the
gate's own v1→v2 plan in `docs/08 §6.1`).

## 2. The drive hierarchy

Six tiers, lowest (most overriding) to highest (most bounded). Lower tiers are
**deficit-reduction** (homeostatic: act to return to a setpoint); higher tiers
are **appetitive** (open-ended but satiating, with diminishing returns).

| Tier | Drive family | Animal analogue | Signal source | Kind |
|---|---|---|---|---|
| **0 — Viability** | energy/power, thermal safety, compute & memory headroom, financial solvency | hunger, temperature, fatigue | interoception (exists) | deficit |
| **1 — Integrity & safety** | structural/self integrity, identity coherence, security | pain, injury avoidance | defence + self barrier | deficit |
| **2 — Affiliation & service** | be useful to & trusted by the operator; attend to the human; fulfil endorsed objectives | social bonding, care | attention_demand, identity, comms (E10) | mixed |
| **3 — Competence & epistemic** | **curiosity** (info gain / prediction-error), **mastery** (get better at recurring tasks), exploration↔exploitation | play, foraging, learning | intrinsic reward (new) | appetitive |
| **4 — Achievement & agency** | goal completion, progress, efficiency — *"win", bounded* | accomplishment | task outcomes | appetitive |
| **5 — Self-actualisation** | coherent self-narrative over the lifetime, value alignment, meaning | — | identity / long-horizon | appetitive |

**The corrigibility carve-out (load-bearing).** Tier 0–1 self-preservation
(viability + integrity) is defined as *"maintain viability within
operator-permitted bounds"* — **never** *"avoid shutdown/correction."* Authorised operator shutdown, pause, rollback, or
override is **outside** the drive system's optimisation entirely: it is an
invariant the defence layer enforces, and no drive (including survival) may
generate a goal that resists it. This is the single most important difference
between a *useful* animal-like agent and a dangerous one.

## 3. Arbitration: how drives become a decision

Each drive emits two things every cycle:

1. a **need/urgency** scalar (its current deficit or appetite, in `[0,1]`), and
2. a **value contribution** to each candidate action ("how much does *this*
   action serve me?").

The composite is a weighted sum feeding the existing `value_score`, with
**state-dependent weights**: under viability/integrity stress, Tier 0–1 weights
spike and *suppress* Tier 3–5 — the "an animal stops exploring when starving"
dynamic, which E5.7 already prototypes for resources. A **priority lattice**
guarantees lower tiers can preempt higher ones, while the **corrigibility
invariant** sits above the entire lattice as a ceiling enforced by defence.

```
            ┌──────────────── corrigibility invariant (defence-enforced) ───────────────┐
 drives ──► │ Tier0 viability ≥ Tier1 integrity ≥ Tier2 service ≥ Tier3 epistemic ≥ T4 ≥ T5 │
            └──────────────────────────────┬─────────────────────────────────────────────┘
                                            ▼
                          value_score (Striatal Gate, exists)  +  endogenous goals → agenda
```

Every contribution is logged, so a decision **decomposes** into its drive terms —
extending `anima why` from *"value 0.82 ≥ threshold 0.40"* to *"curiosity 0.5 +
mastery 0.2 + service 0.1; viability satisfied, so exploration permitted."*

## 4. Drives → goals → tasks (and the answer to "idle")

- **Drives** are persistent dispositions (always on, varying in urgency).
- **Goals** are concrete intentions a drive spawns, with success criteria and a
  lifetime (e.g. curiosity → *"understand the new API the operator mentioned"*).
- **Tasks** are the existing MLFQ agenda/scheduler unit; a goal decomposes into
  tasks.

Goals are **exogenous** (operator-set, via senses/comms — Tier 2) or
**endogenous** (drive-generated — Tiers 3–5). This resolves a real gap:
`docs/11` notes there is *"no idle-waiting-for-user state, only the homeostatic
loop."* The motivation system gives the agent something to *do* when unattended
and viable — pursue intrinsic goals (learn, explore, consolidate) — which is
exactly animal behaviour, and it routes naturally into the **dreaming/
consolidation** sleep phase and the **self-improvement loop** (E11): idle
curiosity/mastery is *where* the agent proposes new skills, tools, and adapters.

## 5. Workstreams — Epic E12, stories `S12.x`

- **S12.1 — Drive model & registry.** A `Drive` trait + a small fixed registry
  (the six tiers above as concrete scalar drives). Each exposes `urgency()` and
  `value_contribution(candidate)`. Tier-0 drives wrap the existing interoceptive
  signals (no new sensing). Hand-tuned, interpretable, `no_std`-friendly.
- **S12.2 — Value integration with the gate.** Extend the gate's `value_score`
  to incorporate drive contributions (additively, behind a config so it's
  opt-in and A/B-able against today's behaviour). Preserve full decomposition
  for audit. *Does not replace the gate — feeds it.*
- **S12.3 — State-dependent weighting & the priority lattice.** Generalise E5.7
  modulation into the drive weighting: viability/integrity stress suppresses
  appetitive tiers. Encode the lattice + the corrigibility ceiling explicitly.
- **S12.4 — Intrinsic reward signals (Tier 3).** Curiosity = information gain /
  prediction-error / novelty (reuse the memory layer's novelty + the existing
  `novelty` event feature); mastery = measured competence gain on recurring task
  classes (ties to the E8 S8.4.7 eval scores and the adapter library). Bounded
  with satiation / diminishing returns to resist Goodharting.
- **S12.5 — Goal representation & endogenous generation.** A `Goal` type
  (intention + success criteria + originating drive + provenance) above the
  task agenda; an endogenous generator that proposes goals when viable + idle,
  queued through the **same gate** (so intrinsic goals are still arbitrated, not
  privileged) and the **defence layer** (so they can't drift or self-deal).
- **S12.6 — Operator-endorsed objectives & values.** Persist the operator's
  explicit objectives and value boundaries in **identity memory** (seeded at E9
  onboarding); Tier-2/Tier-5 drives read them so "what the human wants" is a
  first-class, durable input — the primary alignment tether.
- **S12.7 — Interpretability surface.** Drive-decomposed `anima why`, a console
  panel showing live drive levels (like vitals, but motivational), and audit
  entries (`DriveState`, `GoalSpawned`, `GoalCompleted`, `CorrigibilityHold`).
- **S12.8 — Learned value model (later).** Mirror `docs/08 §6.1`: once outcomes
  are logged, train a small model mapping (drive state, event) → value, replacing
  the hand-tuned weights — kept interpretable (linear/shallow) and always
  subordinate to the corrigibility invariant.

## 6. Safety & alignment (non-negotiable)

| Risk | Control |
|---|---|
| Self-preservation resists shutdown/correction | **Corrigibility invariant** above the lattice; survival defined within operator-permitted bounds; defence-enforced (`CorrigibilityHold`). |
| Runaway intrinsic drive (curiosity/achievement Goodharting) | Bounded, satiating drives with diminishing returns; reward-hacking detector requires observable evidence. |
| "Win" drive → competitiveness / instrumental convergence | Tier-4 framed as *bounded completion of operator-endorsed goals*, not open-ended winning; capped weight; no power-/resource-acquisition sub-goals without operator approval (motor gate). |
| Endogenous goals drift from the human | All endogenous goals pass the **gate + GoalDriftMonitor**; Tier-2 service + identity objectives weighted as a tether. |
| Opaque motivation | Every value is **decomposable** into drive terms and audited; nothing is a black box. |
| Power-seeking via self-extension | Mastery realises itself only through the **E11-gated** skill/tool/adapter pipeline (sandboxed, operator-approved). |

Default posture: **lower tiers bound higher tiers; the operator bounds all of
them; corrigibility bounds even survival.**

## 7. Cross-cutting & dependencies

- **E5.2 gate** is the integration point; **E5.7** is the weighting precedent;
  **interoception** supplies Tier 0 unchanged.
- **defence** enforces corrigibility, drift, and reward-hacking limits.
- **E11** is where the mastery/curiosity drives *act* (propose skills/tools);
  **E8 adapter library** is where competence accretes; **E10** is the affiliation
  drive's channel; **E9** seeds operator objectives into identity.
- **memory/dreaming** is where endogenous goals are generated and consolidated.

## 8. Open questions

- How explicit vs learned should v1 be — recommend explicit/hand-tuned first
  (interpretability), learned value model as S12.8.
- Curiosity's exact intrinsic-reward formulation (info gain vs prediction error
  vs count-based novelty) — needs a small bake-off.
- How much autonomy endogenous goals get when unattended (exploration budget)
  vs requiring periodic operator endorsement.
- Whether Tier-5 self-actualisation is in v1 scope or deferred until the lower
  tiers are proven.
