# AnimaOS — Docker deployment

Containerised stack for running AnimaOS against local GPU inference (and,
optionally, Unsloth-based sleep-phase fine-tuning). Four compose files cover
the main deployment targets:

| Compose file | Target | GPU |
|---|---|---|
| `docker-compose.mock.yml` *(standalone)* | Zero-dependency MVP — mock backend | none |
| `docker-compose.yml` | NVIDIA GPU (Linux + WSL 2) | CUDA via nvidia-container-toolkit |
| `docker-compose.cpu.yml` *(overlay)* | CPU-only Linux | none |
| `docker-compose.apple.yml` *(standalone)* | Apple Silicon Mac | Metal (via native Ollama) |

The default stack is designed for a single workstation with an NVIDIA GPU
(developed against an RTX 3090). For CPU-only or Apple Silicon setups, see
the sections below.

## Quickest start (no GPU, no model downloads)

```sh
docker compose -f docker-compose.mock.yml up --build
```

Runs the full organism — somatic lifecycle, sleep cycle, gate/router,
operator console — against the deterministic mock LLM backend. Open
<http://127.0.0.1:8088/> for the dashboard, then:

```sh
curl -X POST http://127.0.0.1:8088/guidance \
     -H 'Content-Type: application/json' \
     -d '{"text":"summarise system state","priority":"High"}'
```

and watch the agent wake, dispatch, answer, and return to sleep on the
event stream (`GET /events`). The same round-trip is asserted in CI by the
`docker.yml` smoke-test step on every image build. Switch to a real
backend by moving to one of the compose files below.

## Topology

```
┌─────────────────┐   HTTP    ┌─────────────────┐
│  anima-hosted   │ ────────▶ │     ollama      │ ◀── GPU
│  (Rust binary)  │  :11434   │  (llama.cpp)    │
└─────────────────┘           └────────┬────────┘
                                       │ shares ollama-models volume
                                       ▼
                              ┌─────────────────┐
                              │     trainer     │ ◀── GPU
                              │  (Unsloth)      │     profile: training
                              └─────────────────┘
```

| Service       | Role                                                                | Profile  | GPU |
|---------------|---------------------------------------------------------------------|----------|-----|
| `ollama`      | llama.cpp-backed inference daemon, serves the instinct + workhorse  | default  | yes |
| `ollama-init` | One-shot: pulls the configured GGUF tags into the models volume     | default  | no  |
| `hosted`     | The Rust `anima-hosted` agent binary; talks to ollama over HTTP     | default  | no  |
| `trainer`     | Unsloth QLoRA toolchain for sleep-phase fine-tuning                 | training | yes |

## Host prerequisites

- Docker 23+ (BuildKit is the default; the Dockerfiles use cache mounts).
- NVIDIA driver ≥ 550.54 (CUDA 12.4 ABI).
- `nvidia-container-toolkit` installed and the `nvidia` runtime registered:
  <https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/>.

Verify the toolkit before bringing compose up:

```sh
docker run --rm --gpus all nvidia/cuda:12.4.1-base-ubuntu22.04 nvidia-smi
```

## Usage

From the repo root:

```sh
# Bring up the inference stack and run the default two-agent demo.
docker compose up --build

# First time only — the ollama-init service pulls the instinct + workhorse
# models (~2 GB total for the defaults).  Subsequent runs skip the download.

# Run an ad-hoc subcommand against the live local model.
docker compose run --rm hosted why
docker compose run --rm hosted identity show

# Include the trainer (Unsloth) — first build is ~10 minutes.
docker compose --profile training up --build

# Confirm Unsloth + GPU wiring inside the trainer container.
docker compose --profile training run --rm trainer python /app/check.py
```

## Three-tier model architecture

The router in `crates/vita/src/router.rs` already classifies invocations
into three tiers; the container stack realises them like this:

| `CostClass`   | Model                                                                | Engine                    |
|---------------|----------------------------------------------------------------------|---------------------------|
| `CheapLocal`  | Gemma 4 E2B instinct (`hf.co/unsloth/gemma-4-E2B-it-GGUF:Q4_K_M`)    | local Ollama              |
| `MidTier`     | Qwen 3.5 9B MTP workhorse (`hf.co/unsloth/Qwen3.5-9B-MTP-GGUF:Q4_K_M`) | local Ollama            |
| `Frontier`    | API call (Claude / GPT)                                              | Anthropic / OpenAI live   |

Both default tags are pulled directly from Hugging Face by Ollama's
`hf.co/<owner>/<repo>:<quant>` resolver, which needs Ollama ≥ 0.21 for
Gemma 4 support and ≥ 0.23 for the MTP draft-head speculative-decoding
path used by the workhorse — the compose pin (`OLLAMA_VERSION=0.30.0`)
clears both. Bump to a newer tag in `.env` when validating Ollama
upgrades.

Today `anima-hosted` picks a *single* backend at startup via
`ANIMA_BACKEND`; wiring the router so it dispatches each invocation to the
tier-appropriate backend is the next refactor (a `LifecycleManager` change,
not a Docker change).

## VRAM budget on a 3090 (24 GB)

| Component                                | VRAM (typical) |
|------------------------------------------|----------------|
| `gemma-4-E2B-it` instinct (Q4_K_M)       | ~3.5 GB        |
| `Qwen3.5-9B-MTP` workhorse (Q4_K_M)      | ~5.5 GB        |
| KV cache (8 k ctx, two slots)            | ~1.5 GB        |
| Headroom for QLoRA (sleep phase)         | ~10 GB         |
| Spare                                    | ~3 GB          |

Drop to `Q4_K_S` or `Q3_K_M` on smaller cards, or step up to `Q5_K_M` /
`Q8_0` on cards with more headroom — Ollama resolves the suffix straight
to the matching file in the HF repo. Override
`ANIMA_WORKHORSE_MODEL` / `ANIMA_INSTINCT_MODEL` in the environment
before `compose up` to swap quants or models entirely.

## Configuration env vars

| Variable                  | Default                                              | Used by         |
|---------------------------|------------------------------------------------------|-----------------|
| `OLLAMA_VERSION`          | `0.30.0`                                             | `ollama`, `ollama-init` |
| `ANIMA_BACKEND`           | `ollama`                                             | `hosted`        |
| `ANIMA_OLLAMA_URL`        | `http://ollama:11434`                                | `hosted`        |
| `ANIMA_OLLAMA_MODEL`      | `hf.co/unsloth/Qwen3.5-9B-MTP-GGUF:Q4_K_M`           | `hosted`        |
| `ANIMA_OLLAMA_CTX`        | `8192`                                               | `hosted`        |
| `ANIMA_OLLAMA_TIMEOUT`    | `300` (seconds)                                      | `hosted`        |
| `ANIMA_INSTINCT_MODEL`    | `hf.co/unsloth/gemma-4-E2B-it-GGUF:Q4_K_M`           | `ollama-init`   |
| `ANIMA_WORKHORSE_MODEL`   | `hf.co/unsloth/Qwen3.5-9B-MTP-GGUF:Q4_K_M`           | `ollama-init`   |
| `TRAINER_BASE_MODEL`      | `unsloth/gemma-4-E2B-it-unsloth-bnb-4bit`            | `trainer`       |
| `ANTHROPIC_API_KEY`       | (empty)                                              | `hosted`        |
| `OPENAI_API_KEY`          | (empty)                                              | `hosted`        |

Persistent state:

- `anima-data` volume → `~/.anima` inside `hosted` (identity store, audit
  log artefacts).
- `ollama-models` volume → `/root/.ollama` inside Ollama, also mounted at
  `/models` inside the trainer so adapter exports land where the
  inference engine can see them.

## What "bare-metal-ish" buys you in containers

- **Ollama with `--gpus all`** uses the exact same CUDA kernels as a
  host-native install — there's no virtualisation layer between the
  container and the 3090, just the nvidia-container-toolkit shim wiring
  device nodes through. Throughput-equivalent to bare-metal Ollama.
- **HTTP loopback** between `hosted` and `ollama` adds < 100 µs per
  request — negligible compared to token generation latency (10–50 ms
  per token).
- **Flash Attention** is enabled by default (`OLLAMA_FLASH_ATTENTION=1`);
  Ampere supports it, and it halves KV-cache memory at full speed.
- **`OLLAMA_KEEP_ALIVE=24h`** stops the workhorse from being unloaded
  between requests, eliminating cold-start cost during interactive use.

The bare-metal install path (no containers) is documented in
[`../docs/10-deployment-pathways.md`](../docs/10-deployment-pathways.md).

## CPU-only Linux

Use the `docker-compose.cpu.yml` overlay to run without an NVIDIA GPU:

```sh
# First time — brings up ollama (CPU mode) + hosted agent.
docker compose -f docker-compose.yml -f docker-compose.cpu.yml up --build

# One-shot subcommand:
docker compose -f docker-compose.yml -f docker-compose.cpu.yml run --rm hosted why
```

The overlay:
- Removes the NVIDIA device reservation from the `ollama` service.
- Disables Flash Attention (requires CUDA) and caps Ollama to one loaded model.
- Defaults to CPU-friendly 3.8B models (~3 GB VRAM-equivalent RAM each).

Set `ANIMA_INSTINCT_MODEL` / `ANIMA_WORKHORSE_MODEL` / `ANIMA_OLLAMA_MODEL` in
`.env` to swap models or quants.  On hosts with < 8 GB RAM, set
`ANIMA_BACKEND=mock` or supply an API key (`ANTHROPIC_API_KEY`) and set
`ANIMA_BACKEND=anthropic` to bypass local inference entirely.

## Apple Silicon (macOS)

Docker Desktop on macOS does not expose Metal to containers.  Run Ollama
natively on the host (full Metal acceleration) and use the standalone
`docker-compose.apple.yml` to run only the agent in Docker:

```sh
# 1. Install and start Ollama natively.
brew install ollama
ollama serve                                           # stays running in background

# 2. Pull the model you want (any GGUF from Hugging Face via the hf.co resolver).
ollama pull hf.co/bartowski/Phi-3.5-mini-instruct-GGUF:Q4_K_M

# 3. Build and start the agent container.
docker compose -f docker-compose.apple.yml up --build

# 4. Open the console.
open http://127.0.0.1:8088/
```

To use a different model, set `ANIMA_OLLAMA_MODEL` before running compose:

```sh
ANIMA_OLLAMA_MODEL=llama3:8b docker compose -f docker-compose.apple.yml up --build
```

## Pre-built images (GHCR)

The hosted agent image is built and pushed to GHCR by the `docker.yml` CI
workflow on every merge to `main` and on each GitHub Release:

```sh
# Latest main-branch build:
docker pull ghcr.io/codehalwell/animaos/hosted:main

# Specific release:
docker pull ghcr.io/codehalwell/animaos/hosted:v1.0.0
```

Replace the `build:` block in any compose file with `image: ghcr.io/codehalwell/animaos/hosted:<tag>`
to skip the local build step.

The trainer image is **not** built in CI (10–12 GB; GPU-dependent).  Build it
locally when you need sleep-phase fine-tuning:

```sh
docker build -f docker/Dockerfile.trainer -t animaos/trainer:dev .
```

## Known limitations / follow-ups

1. **Router wiring** — the three-tier router doesn't yet dispatch to
   tier-specific backends per invocation; the `TierBackends` map exists
   (`crates/vita`) but the compose stack still binds one Ollama model.
   Setting different `ANIMA_OLLAMA_MODEL` values per service is the
   current workaround until the router wiring lands.
2. **Sleep-phase training loop** — `trainer/check.py` proves Unsloth +
   GPU + bitsandbytes import cleanly, but the replay-buffer →
   QLoRA-step → GGUF-export → Ollama-reload cycle is not yet written
   (tracked in E8 S8.4).
3. **Streaming cancellation** — the Ollama backend checks the
   cancellation token between received lines, not mid-line. A truly
   instantaneous cancel needs an HTTP client that supports request abort
   (currently using `ureq`).
