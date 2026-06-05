# 13 — Local LLM Provider Ecosystem

> **Status:** Proposed (scoping). Target epic: **E8 — Local Inference Ecosystem**.
> Branch: `claude/llm-tools-animaos-vuXRK`.
> Companion to [12 — Real-World Tools](./12-real-world-tools-plan.md) (E7); E8
> supplies the *brains* that E7's tools give *hands*.

## 0. Goal

Let an operator point AnimaOS at whichever local inference stack they already
run, without bespoke per-vendor code. Target ecosystem (operator's list):

**Hugging Face · Llama.cpp · Ollama · LM Studio · NVIDIA · vLLM · Unsloth ·
LiteRT-LM**

> Note: the operator's list wrote "ML Studio" / "LiteRT-ML"; these are
> **LM Studio** and **LiteRT-LM** respectively.

## 1. Current state (grounding)

| Concern | Today |
|---|---|
| Backend trait | `scheduler::backend::LlmBackend` (`crates/scheduler/src/backend.rs`): `id()`, `stream_completion(prompt, cancel)`, `model_id()`, `max_context_tokens()`, `estimate_token_count()`. **`no_std`-clean.** |
| Concrete backends | `AnthropicBackend`, `OpenAiBackend` (fixture replay), `OllamaBackend` (**live**, blocking `ureq`), `MockLlmBackend`. All in `llm-backends/` except the mock. |
| Selection | `BackendKind { Anthropic, OpenAi, Mock, Ollama }` + `BackendFactory::from_env_or_mock` (`llm-backends/src/factory.rs`). |
| Ollama precedent | `OllamaBackend::from_env()` reads `ANIMA_OLLAMA_URL` / `_MODEL` / `_CTX` / `_TIMEOUT`, streams newline-delimited JSON. This is the **template** for every HTTP backend below. |
| Gap for tools | The trait streams text from a **prompt string only** — it has **no notion of chat messages or tool-calling**. E7 Phase 4 (live tool-calling) needs this; see §5. |

## 2. The central insight: three integration shapes, not eight

The eight names are not eight backends. They fall into three buckets:

| Shape | Providers | Integration |
|---|---|---|
| **A. OpenAI-compatible HTTP server** | vLLM, LM Studio, NVIDIA NIM, Hugging Face TGI, llama.cpp `llama-server` | **One** generalized `OpenAiCompatibleBackend` parameterised by base URL + model + capability flags. ~90% of the work is shared. |
| **B. Native in-process runtime (FFI)** | llama.cpp (via bindings), LiteRT-LM | A `ToolDriver`-style FFI backend that loads a model file directly — no sidecar, true on-device. Heavier build, optional/feature-gated. |
| **C. Offline adaptation (not a runtime)** | **Unsloth (primary)** | The **canonical fine-tuning engine** for AnimaOS — not a serving backend. Produces adapters/GGUF that bucket A or B then serve. Ties into AnimaOS "dreaming"/consolidation. |

This means the epic is mostly: **build one OpenAI-compatible backend well**, add
a couple of native runtimes, and adopt **Unsloth as the default training
path** — *not* eight parallel HTTP clients.

> **Unsloth is the standard fine-tuning library for AnimaOS.** It is chosen for
> efficiency: custom Triton kernels give ≈2× faster LoRA/QLoRA training at
> materially lower VRAM (often single-consumer-GPU viable), with no accuracy
> loss vs. stock implementations. Any other trainer is the exception, not the
> default — all first-party adaptation tooling targets Unsloth.

## 3. Workstreams — Epic E8, stories `S8.x`

### Phase 0 — Provider substrate (`S8.0`)

- **S8.0.1 — Generalize the factory.** Extend `BackendKind` with the new
  variants and a `ProviderConfig { base_url, model, api_key?, ctx, timeout,
  capabilities }`. Keep `from_env_or_mock` working; add
  `BackendFactory::from_config`.
- **S8.0.2 — Capability descriptor.** A `BackendCapabilities` struct
  (`tools`, `embeddings`, `streaming`, `json_mode`, `vision`) so the router and
  E7's tool-calling loop can ask *"can this backend do tools?"* and fall back to
  prompt-format tool emulation when not.
- **S8.0.3 — Health/readiness probe.** A uniform `async fn health()` so the
  hosted kernel can verify a local server is up and the model is loaded before
  routing real traffic to it; emit an audit entry on failure.
- **S8.0.4 — Fixture discipline.** Every new backend ships a fixture/replay mode
  (default in CI). Live mode is env-gated; **no network in CI**.

### Phase 1 — OpenAI-compatible umbrella (`S8.1`) — covers vLLM, LM Studio, NVIDIA NIM, HF TGI, llama.cpp-server

- **S8.1.1 — `OpenAiCompatibleBackend`.** Generalize the existing `OpenAiBackend`
  into a base-URL-driven client against `/v1/chat/completions` (SSE streaming),
  following the `OllamaBackend` blocking-`ureq` pattern. Parse `data:` chunks →
  `StreamingCompletion::Token`.
- **S8.1.2 — Provider presets.** Thin constructors that only set defaults
  (base URL, env-var names, capability flags):

  | Preset | Default base URL | Env prefix | Notes |
  |---|---|---|---|
  | `vllm` | `http://vllm:8000/v1` | `ANIMA_VLLM_*` | Native tools + `/v1/embeddings`. |
  | `lmstudio` | `http://localhost:1234/v1` | `ANIMA_LMSTUDIO_*` | Desktop app; OpenAI-compatible server. |
  | `nvidia-nim` | `http://localhost:8000/v1` | `ANIMA_NIM_*` | NIM microservice; OpenAI-compatible. (Triton/TensorRT-LLM gRPC is a later S8.1.x.) |
  | `hf-tgi` | `http://tgi:8080/v1` | `ANIMA_TGI_*` | TGI Messages API. |
  | `llamacpp-server` | `http://localhost:8080/v1` | `ANIMA_LLAMACPP_*` | `llama-server --api`. |

- **S8.1.3 — Tool-calling passthrough.** When `capabilities.tools`, map E7
  `ToolSpec` → OpenAI `tools` schema and parse `tool_calls` from the response
  (requires the §5 trait extension). Otherwise use the prompt-format fallback.
- **S8.1.4 — `BackendKind::parse` + factory wiring + tests** (fixture-backed unit
  tests per preset; one env-gated live smoke test).

### Phase 2 — Hugging Face (`S8.2`)

HF spans more than one surface; pick the local-first ones:

- **S8.2.1 — TGI via the umbrella.** HF **Text Generation Inference** run
  locally already speaks OpenAI-compatible → it is just the `hf-tgi` preset from
  S8.1.2. Minimal new code.
- **S8.2.2 — `transformers` sidecar (optional).** For arbitrary HF models with
  no server, a Python `transformers` worker speaking the existing
  length-prefixed-JSON/UDS protocol (same shape as the cortex bridge). Heaviest
  path; feature-gated.
- **S8.2.3 — Model discovery.** Optional helper using the Hugging Face Hub to
  resolve model IDs / context windows for config validation (read-only; not on
  the inference hot path).

### Phase 3 — Native in-process runtimes (`S8.3`)

True on-device, no sidecar — aligns with the bare-metal ambition (roadmap
Phase 4).

- **S8.3.1 — llama.cpp FFI backend.** In-process via `llama-cpp-2`/`llama-cpp-rs`
  bindings, loading a GGUF directly. Feature-gated (`llamacpp-ffi`, std-only,
  links native lib). Gives a server-less local backend for embedded installs.
- **S8.3.2 — LiteRT-LM backend.** Google **LiteRT-LM** (formerly TF-Lite; the
  on-device LLM stack behind AI Edge) via its C API, running on CPU/GPU/NPU.
  Feature-gated (`litert`), targets edge/NPU hardware. The most "agent-OS-native"
  runtime: no daemon, no network.

### Phase 4 — Unsloth adaptation engine (`S8.4`) — **the** fine-tuning path

Unsloth is the **default, first-party fine-tuning engine** for AnimaOS (see §2).
It is **training, not serving** — model it as an offline capability that feeds
models into buckets A/B:

- **S8.4.1 — `anima-finetune` job.** An xtask/CLI wrapping an Unsloth LoRA/QLoRA
  run (Python) over a curated dataset, exporting an adapter + merged GGUF ready
  for Ollama/llama.cpp/vLLM. This is the canonical adaptation entrypoint, not a
  one-off script. Standard config surface: base model, dataset, LoRA rank,
  QLoRA on/off, max steps, export targets.
- **S8.4.2 — Provider abstraction (thin).** A `FineTuner` trait with Unsloth as
  the **default and only first-party impl**; the trait exists purely so the
  pipeline (dataset → train → export → eval → promote) is testable with a mock,
  not to invite alternative trainers.
- **S8.4.3 — Consolidation hook (research spike).** Wire the dataset source to
  AnimaOS's episodic memory / "dreaming" consolidation so the agent fine-tunes a
  small local model on its own experience during sleep cycles, then serves the
  result. **Highest-risk item in either epic** — significant safety/eval
  implications (catastrophic forgetting, value drift); any resulting model must
  pass the existing defence evaluation before promotion, behind explicit
  operator opt-in. Unsloth's efficiency is what makes this loop *plausible* on
  local hardware in the first place.

### Adaptation methods: LoRA and High-Rank Adaptation (HRA)

LoRA/QLoRA forces the update into a strict low-rank bottleneck
(`ΔW = BA`, `r ≤ min(d_in, d_out)`). That ceiling bites hardest exactly where
AnimaOS leans hardest: adapting the **smallest (instinct/cheap-local) model** on
**highly out-of-distribution** data — the agent's own episodic experience.
A tiny model has the least spare capacity, and self-experience is a large
distribution shift from the base pretraining. **High-Rank Adaptation (HRA)**
lifts the rank ceiling at a comparable parameter footprint, so it is a first-
class option for the instinct tier specifically.

**The deployment calculus inverts at instinct scale.** The standard warning
against Hadamard-style HRA (HiRA/BoHA) is the VRAM spike from materialising a
full dense `ΔW` at merge time — a real problem at **70B** scale. At
**sub-2B instinct scale that spike is trivial**, so the method with the *most*
expressiveness becomes the *cheapest* to merge precisely on the model we most
want to adapt. The smallest model is therefore the sweet spot on both axes:
most to gain from high rank, least to lose on merge cost.

**The remaining real constraint is quantised serving.** AnimaOS serves 4-bit
GGUF via Ollama/llama.cpp. A high-rank FP update cannot merge directly into an
NF4 base, so the method choice splits by merge path:

| Method | Class | Merge into 4-bit GGUF | Fit for AnimaOS instinct |
|---|---|---|---|
| **HyperAdapt** (Gurung & Campbell '25) | structural scaling, `W = S₁·W₀·S₂`, `n+m` params | **clean, cheap** broadcast | **Default for the quantised serving path.** |
| **OHoRA** | orthogonal/QR projection, ~0.04% params | clean (QR baseline step) | Strong alternative; merges cleanly. |
| **HiRA / BoHA** (Yu et al. '25) | Hadamard `ΔW=(B₁A₁)⊙(B₂A₂)` | dense materialise → **dequant→merge→requant** | Viable at instinct scale (spike is small); best raw expressiveness; BoHA's blockwise locality also curbs catastrophic forgetting — useful for the continual self-improvement loop. |
| **TeRA** (Gu et al. '25) | Tucker tensor net, vector-scaled | often left **unmerged** (layer-wise contraction) | Extreme param efficiency; merge complexity makes it a research item. |
| **HRP** (Chen et al. '25) | high-rank preheat → SVD → low-rank LoRA | **clean** (ends as LoRA) | Cheap robustness win: fixes LoRA init sensitivity, merges like vanilla LoRA. |

- **S8.4.4 — `AdaptationMethod` abstraction.** Extend the `anima-finetune`
  config with a pluggable method (default `qlora`). Integrate via the HF **PEFT**
  library where methods are already supported (e.g. HiRA) under Unsloth's
  optimised kernels; custom parameterisations (HyperAdapt diagonal scaling, HRP
  preheat+SVD) as first-party impls. Keep QLoRA the conservative default.
- **S8.4.5 — HRA for the instinct tier (headline).** Make HRA selectable —
  and *recommended* — for the cheap-local model. Default to **HyperAdapt**
  (clean quantised merge) for routine adaptation; offer **HiRA/BoHA** for
  high-expressiveness passes on heavily OOD self-experience data; offer **HRP**
  as a low-cost robustness upgrade over vanilla LoRA init. Per the report's own
  caveat, gate the choice behind eval: when a well-tuned LoRA (LR schedule + α)
  matches HRA on in-distribution tasks, prefer LoRA — adopt HRA when the eval
  shows a real margin on the agent's actual domain.
- **S8.4.6 — Merge & quantisation pipeline.** Implement both export paths:
  the **clean-merge** path (HyperAdapt/OHoRA/HRP → direct merge → GGUF export →
  Ollama hot-reload) and the **Hadamard** path (materialise dense `ΔW` →
  dequantise base → merge → requantise → GGUF), with a precision-delta check
  after requantisation so silent degradation is caught before promotion. Record
  the method + merge path in the model's provenance (E11 S11.6).

- **S8.4.7 — Adaptation eval harness (the LoRA-vs-HRA decider).** A reproducible
  bench that *earns* a method's adoption per domain rather than assuming it,
  honouring the report's caveat that a well-tuned LoRA often matches HRA
  in-distribution. Reuses AnimaOS's existing eval idioms — n-run statistical
  comparison (cf. `xtask demo --kind graceful`'s two-proportion z-test) and a
  needle-style probe (cf. `kv-controller` `NeedleBenchmarkConfig`).

  - **Candidates under matched budget.** Train each method (QLoRA baseline,
    HyperAdapt, HiRA/BoHA, HRP) on the same data with matched steps/params, so
    the comparison is fair.
  - **Four metrics, not one.** (1) *Task success* on a held-out slice of the
    domain (the agent's own episodic data for self-experience runs);
    (2) *OOD generalisation* on a shifted held-out set; (3) *retention /
    anti-forgetting* on a frozen "core competencies" probe set (cf.
    `xtask demo --kind retention`) — critical for the continual loop;
    (4) *merge fidelity* = the S8.4.6 precision-delta after requantisation.
  - **Adoption rule.** Promote HRA over the LoRA baseline only when it clears a
    configurable **margin threshold** on (1)+(2) **without** regressing (3)
    beyond tolerance. Otherwise keep LoRA. The winning scores are written into
    the adapter's metadata (S8.4.8) so selection is evidence-based, and the run
    is gated by defence eval before any promotion (E11 S11.5).
  - **Hermetic by default.** Fixture datasets + recorded scores for CI; live
    training runs are env-gated, exactly like the inference backends.

- **S8.4.8 — Adapter library & dynamic mounting.** A registry of saved,
  task-scoped adapters the agent can **mount on demand** instead of baking one
  monolithic model — so a small base model + a shelf of cheap specialists covers
  many domains.

  - **A serving distinction falls straight out of the merge maths.** Only
    LoRA-format / structurally-clean adapters are **cheaply hot-mountable** by
    the runtimes (vLLM native multi-LoRA via per-request `lora_request`;
    llama.cpp `--lora` hot-load). Hadamard HRA (HiRA/BoHA) yields a *dense* `ΔW`
    with no mountable adapter format — it must be **baked into a model variant**
    (S8.4.6 Hadamard path). So the library is two-tier:

    | Tier | Produced by | Served as | Swap granularity |
    |---|---|---|---|
    | **Mountable adapter** | LoRA/QLoRA, HRP (→LoRA), HyperAdapt* | hot-loaded onto a live base (vLLM/llama.cpp) | **per task/request** |
    | **Baked variant** | HiRA/BoHA, OHoRA, full merges | a distinct GGUF model | per route/model |

    *HyperAdapt's `S₁·W₀·S₂` scaling is a clean transform but not a stock LoRA
    format; mount it where the runtime allows a custom apply, otherwise bake.

  - **`AdapterLibrary`.** Lives at `~/.anima/<agent_id>/adapters/`; each entry
    carries metadata: base model + quant, method, **domain/description**,
    provenance (E11 S11.6), and the S8.4.7 eval scores.
  - **Task → adapter selection reuses E7.** Embed each adapter's `description`
    and select by task similarity with the same `length_robust_filter` that
    picks tools and skills — one selector across tools, skills, *and* adapters.
    The router (E5.3) thus gains an `AdapterSelector` alongside `ModelSelector`.
  - **Composition & limits.** Allow mounting a small set concurrently where the
    runtime supports it (vLLM), with a homeostatic cap (mounting cost feeds the
    E5.7 signals) and a circuit-breaker-style fallback to the bare base model if
    an adapter misbehaves.
  - **Self-improvement closes here.** The dreaming-phase loop (E11 S11.5)
    deposits new adapters into this library; routine tasks then mount the right
    specialist automatically — the agent grows a *shelf of competencies* rather
    than overwriting one model.

## 4. Route → backend mapping

E5.3's `ModelSelector` gains a config-driven mapping so an operator binds tiers
to whatever they run locally, e.g.:

```
frontier    → vllm (large model)        | or hosted Anthropic (E7 default)
mid-tier    → lmstudio / llamacpp-server
cheap-local → ollama (small instinct model) | litert on NPU
             └─ + task-specific adapter hot-mounted from the library (S8.4.8)
```

Backend selection (which base model) and adapter selection (which specialist to
mount) are two stages: the `ModelSelector` binds the tier to a base; an
`AdapterSelector` then mounts zero or more library adapters for the specific
task. The cheap-local tier is the prime beneficiary — a tiny base plus a mounted
specialist often matches a much larger generalist on the task at hand.

The mapping lives in config, validated at startup (reuse the E5.3
route-validation discipline), never hard-coded.

## 5. Required trait extension (shared dependency with E7 Phase 4)

The current `LlmBackend` streams text from a prompt string. To drive real
tool-calling (E7 S7.4) **and** to support chat-shaped local models, add — in a
backward-compatible way (default methods so existing impls compile unchanged):

- a **chat/messages** request shape (system/user/assistant/tool roles), and
- optional **tool definitions in / tool calls out**, gated by
  `BackendCapabilities.tools` with a prompt-format fallback.

This is the single most important cross-cutting item; **E8 S8.0/S8.1 and E7
S7.4 should share it.** Sequence E8 Phase 0–1 alongside E7 Phase 4.

## 6. Cross-cutting

- **Embeddings reuse.** vLLM/llama.cpp/Ollama expose `/v1/embeddings` (or
  native) — the same local stack can serve the **E7 S7.3 tool scorer** and L3
  memory embeddings, avoiding a second model dependency.
- **Homeostatics.** Local inference is power/thermal-heavy but financially free;
  feed per-backend cost profiles into the E5.7 modulation signals (a local
  frontier model may be *power*-expensive but *financially* free — different
  trade-off than a hosted API).
- **Observability.** Audit `model_id`, backend `id`, and health-probe outcomes;
  surface in `anima why` and the operator console.
- **Security.** Local endpoints still get the E7 egress treatment (a "local" URL
  can be an SSRF pivot); API keys (where used, e.g. NIM) go through the E7
  redaction path.

## 7. Recommended sequencing

1. **S8.0 + S8.1** — substrate + OpenAI-compatible umbrella. Single biggest
   unlock: vLLM, LM Studio, NVIDIA NIM, HF TGI, and llama.cpp-server all light up
   at once. Do this **with** the §5 trait extension (shared with E7 P4).
2. **S8.2** — HF (mostly the TGI preset; sidecar only if needed).
3. **S8.3** — native FFI runtimes (llama.cpp in-process, then LiteRT-LM) for
   server-less/edge.
4. **S8.4** — Unsloth adaptation: QLoRA job (S8.4.1–.3) → eval harness
   (S8.4.7, the LoRA-vs-HRA decider) → HRA methods + dual merge/quant pipeline
   (S8.4.4–.6) → adapter library + dynamic mounting (S8.4.8). The dreaming hook
   (S8.4.3) and weight-level self-improvement stay a gated research spike. Build
   the eval harness *before* adopting any HRA method so adoption is evidence-led.

## 8. Risks & open questions

- **Trait churn:** adding chat+tools to `LlmBackend` touches every impl; mitigate
  with default methods. Confirm we want chat-messages in the `no_std` core trait
  vs an std-only extension trait.
- **Native build weight:** llama.cpp/LiteRT FFI links native libraries and
  complicates `no_std`/cross builds — keep strictly feature-gated and outside the
  core, like the Wasmtime sandbox.
- **Tool-calling parity:** local models vary wildly in tool-call reliability; the
  prompt-format fallback and per-backend capability flags are essential.
- **NVIDIA surface:** NIM (OpenAI-compatible) is easy; Triton + TensorRT-LLM
  (gRPC) is a heavier, separate sub-story — scope on demand.
- **Unsloth-on-experience:** self-fine-tuning during sleep is powerful but is the
  highest-risk item in either epic; gate behind defence evaluation and explicit
  operator opt-in.
- **HRA maturity vs. quantised merge:** HRA methods are 2025-era and vary in
  PEFT-library support and merge ergonomics; the Hadamard→NF4 dequant/requant
  path risks precision drift. Mitigate with the §S8.4.6 precision-delta check,
  keep QLoRA the default, and adopt HRA per-method only where eval shows a real
  margin on the agent's own domain.
- **Adapter mounting support varies:** vLLM has mature multi-LoRA; llama.cpp
  hot-load is improving; Ollama adapter mounting is more static (model-variant
  oriented). Treat dynamic mounting as a vLLM/llama.cpp capability flag, with a
  bake-to-variant fallback (S8.4.6) where the runtime can't hot-mount.
- **Adapter sprawl / forgetting interactions:** a growing self-authored adapter
  shelf needs the S8.4.7 retention metric in the loop and library hygiene
  (dedup, deprecate, kill-switch via E11 S11.6) so specialists don't silently
  conflict or rot.

## 9. Rough effort

| Phase | Size |
|---|---|
| S8.0 substrate + §5 trait ext | M |
| S8.1 OpenAI-compatible umbrella | M (unlocks 5 providers) |
| S8.2 Hugging Face | S–M |
| S8.3 native FFI runtimes | L |
| S8.4 Unsloth pipeline (LoRA/QLoRA) | M (+ research spike) |
| S8.4.4–.6 HRA methods + merge/quant pipeline | M (HyperAdapt/HRP first; Hadamard + TeRA research) |
| S8.4.7 adaptation eval harness (LoRA-vs-HRA decider) | M (reuses existing demo/needle idioms) |
| S8.4.8 adapter library + dynamic mounting | M (vLLM multi-LoRA first; bake-to-variant fallback) |
