# 14 — Onboarding & First-Run Experience

> **Status:** Proposed (scoping). Target epic: **E9 — Onboarding**.
> Branch: `claude/llm-tools-animaos-vuXRK`.
> Related: E7 (tools), E8 (providers), E10 (communication), E11 (skills).

## 0. Goal

Turn first contact with AnimaOS from a developer `docker compose` ritual into a
guided journey that respects the project's inversion — **the agent is the user,
the human is a sensor/operator** (`docs/11-operator-interface.md §1`). Onboarding
is not "configure your account"; it is "bring an agent to life, seed who it
serves, and attach to it."

Closes the five gaps identified in the onboarding review.

## 1. Current state (grounding)

- Supported path is containerised: `docker compose up --build` →
  `anima-hosted serve` → console on `http://127.0.0.1:8088/`
  (`README.md`, `docker/README.md`).
- Identity is a JSON/YAML doc under `~/.anima/<agent_id>/`, seeded **manually**
  via `anima-hosted identity set <key> <value>` (CLI confirmed in
  `kernels/hosted/src/main.rs`).
- `anima-hosted` subcommands today: `serve`, `identity show|set`, `why`.
- The three-tier router exists but `anima-hosted` binds **one** backend at
  startup via `ANIMA_BACKEND` (`docker/README.md` limitation #1).
- The supported path assumes an NVIDIA CUDA GPU.

## 2. Workstreams — Epic E9, stories `S9.x`

### S9.1 — Guided first-run / `anima init` wizard *(gap 1)*

- A new `anima-hosted init` subcommand (and an equivalent first-load panel in
  the console) that runs once and walks the operator through:
  preflight → provider → models → identity → "your agent is awake."
- Idempotent and resumable; writes a single `~/.anima/<agent_id>/onboarding.json`
  state file so re-runs pick up where they left off.
- Pure-terminal first (works over SSH / headless); the browser console mirrors
  it using the existing SSE/guidance transport — no new protocol.

### S9.2 — Conversational identity bootstrap *(gap 2)*

- Instead of `identity set k v`, the agent **interviews** the new user on first
  wake: who they are, how to address them, working hours, goals, boundaries,
  preferred comms channel (feeds E10). Answers populate `IdentityMemory` via the
  existing `set_fact` path (so the audit trail is identical).
- Implemented as a built-in onboarding **skill** (depends on E11) once the live
  cortex (E7 S7.4) can hold a conversation; until then, a scripted fallback
  asks the same questions deterministically.
- Identity write-back is gated and audited exactly as manual edits are.

### S9.3 — Preflight & hardware/provider detection *(gap 4)*

- `anima doctor`: detects GPU vendor/VRAM (NVIDIA via `nvidia-smi`, Apple
  Silicon via `system_profiler`/Metal, CPU-only fallback), available local
  providers (probe Ollama/LM Studio/vLLM ports — reuse E8 health probes), and
  RAM/disk headroom.
- Emits a capability profile that the wizard (S9.1) uses to **recommend** a
  provider + model quant the host can actually run, instead of assuming a 3090.
- Explicit, friendly degradation: CPU-only → recommend a small instinct model;
  Apple Silicon → recommend Metal-backed llama.cpp/Ollama; no GPU at all →
  hosted-API-only mode (frontier route only).

### S9.4 — Non-NVIDIA / CPU / Apple-Silicon support *(gap 4)*

- Make the container + native paths work off-CUDA: a CPU-only compose profile
  (no `--gpus`), an Apple-Silicon native path (Metal via host Ollama/llama.cpp),
  and documented minimums.
- This is mostly packaging + docs + E8 provider presets; the cognitive code is
  already backend-agnostic at the `LlmBackend` trait.

### S9.5 — Per-tier router dispatch *(gap 3)*

- Replace the single-backend `LifecycleManager::new(Arc<dyn LlmBackend>)` with a
  **router-aware backend map** (`ModelSelector → Arc<dyn LlmBackend>`), so the
  cheap-local/mid-tier/frontier tiers actually dispatch to the bound providers.
- This is the `docker/README.md` limitation #1 / `docs/10 §"LifecycleManager is
  single-backend"` follow-up, and a hard prerequisite for the wizard's
  "pick a model per tier" step to mean anything. Shared with E8 §4.

### S9.6 — Bare-metal onboarding story *(gap 5)*

- A documented, scripted path for the microVM/native target: build, key
  provisioning, first boot, attaching `anima-console serial`. Not a wizard
  (it's a build), but a coherent runbook so the production surface isn't a
  cliff. Aligns with `docs/10` migration order flavours A–C.

### S9.7 — Unified quickstart doc

- A single `docs/getting-started.md` stitching preflight → up → console →
  identity into one first-time narrative (today it is scattered across README,
  `docker/README.md`, and docs 10/11).

## 3. The target journey (after E9)

```
anima doctor            → "NVIDIA 3090, 24GB. Ollama detected on :11434."
anima init              → pick providers per tier (recommended defaults pre-filled)
                          → pulls models with a progress UI
                          → agent wakes and *interviews* you (identity bootstrap)
                          → "I'm awake. Here's how to reach me." (hands off to E10)
open console / channel   → ongoing interaction
```

## 4. Cross-cutting & dependencies

- **E8 S8.0** health probes power S9.3; **E8 §4 / S9.5** tier→backend map is
  shared. **E11** skills enable the conversational bootstrap (S9.2).
- Identity writes stay on the existing gated/audited `set_fact` path.
- Everything ships CI-hermetic: the wizard has a `--non-interactive` fixture
  mode driven by a config file for tests.

## 5. Open questions

- Wizard surface priority: terminal-first vs console-first (recommend
  terminal-first, console mirror).
- How much of S9.2 waits on the live cortex vs ships scripted now.
- Minimum supported CPU-only model (latency vs capability floor).
