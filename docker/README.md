# AnimaOS — Docker deployment

Containerised stack for running AnimaOS against local GPU inference (and,
optionally, Unsloth-based sleep-phase fine-tuning). Designed for a single
workstation with an NVIDIA GPU; developed against an RTX 3090.

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

| `CostClass`   | Model                              | Engine                    |
|---------------|------------------------------------|---------------------------|
| `CheapLocal`  | 270 M – 1 B instinct (e.g. `llama3.2:1b`) | local Ollama       |
| `MidTier`     | 3 – 13 B workhorse (e.g. `llama3.2:3b`)   | local Ollama       |
| `Frontier`    | API call (Claude / GPT)            | Anthropic / OpenAI live   |

Today `anima-hosted` picks a *single* backend at startup via
`ANIMA_BACKEND`; wiring the router so it dispatches each invocation to the
tier-appropriate backend is the next refactor (a `LifecycleManager` change,
not a Docker change).

## VRAM budget on a 3090 (24 GB)

| Component                        | VRAM (typical)        |
|----------------------------------|-----------------------|
| `llama3.2:1b` instinct (Q4)      | ~0.7 GB               |
| `llama3.2:3b` workhorse (Q4)     | ~2.5 GB               |
| KV cache (8 k ctx, two slots)    | ~1.0 GB               |
| Headroom for QLoRA (sleep phase) | ~12 GB                |
| Spare                            | ~7 GB                 |

Bigger workhorses (`llama3.1:8b` ≈ 5.5 GB, `llama3.1:13b` ≈ 8 GB at Q4_K_M)
fit comfortably alongside the instinct; override
`ANIMA_WORKHORSE_MODEL=llama3.1:8b` in the environment before
`compose up` to pull a different default.

## Configuration env vars

| Variable                  | Default                       | Used by         |
|---------------------------|-------------------------------|-----------------|
| `ANIMA_BACKEND`           | `ollama`                      | `hosted`        |
| `ANIMA_OLLAMA_URL`        | `http://ollama:11434`         | `hosted`        |
| `ANIMA_OLLAMA_MODEL`      | `llama3.2:3b`                 | `hosted`        |
| `ANIMA_OLLAMA_CTX`        | `8192`                        | `hosted`        |
| `ANIMA_OLLAMA_TIMEOUT`    | `300` (seconds)               | `hosted`        |
| `ANIMA_INSTINCT_MODEL`    | `llama3.2:1b`                 | `ollama-init`   |
| `ANIMA_WORKHORSE_MODEL`   | `llama3.2:3b`                 | `ollama-init`   |
| `ANTHROPIC_API_KEY`       | (empty)                       | `hosted`        |
| `OPENAI_API_KEY`          | (empty)                       | `hosted`        |

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

## Known limitations / follow-ups

1. **Router wiring** — the three-tier router doesn't yet dispatch to
   tier-specific backends. The current spike uses one backend per
   `anima-hosted` invocation.
2. **Sleep-phase training loop** — `trainer/check.py` proves Unsloth +
   GPU + bitsandbytes import cleanly, but the actual replay-buffer →
   QLoRA-step → GGUF-export → Ollama-reload cycle is not yet written.
3. **Streaming cancellation** — the Ollama backend checks the
   cancellation token between received lines, not mid-line. A truly
   instantaneous cancel needs an HTTP client that supports request abort
   (currently using `ureq`).
4. **CI build** — no GitHub Actions step yet builds and pushes the
   `animaos/hosted` and `animaos/trainer` images to GHCR.
