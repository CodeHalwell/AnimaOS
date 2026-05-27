# AnimaOS — Docker spike

Container hosting for the `anima-hosted` Linux-process kernel, intended for
local deployment on a workstation with an NVIDIA GPU (developed against an
RTX 3090).

## What this spike delivers

- `docker/Dockerfile` — multi-stage build. Stage 1 compiles the workspace
  with `cargo build --release -p hosted --bin anima-hosted` inside an
  `nvidia/cuda:12.4.1-devel-ubuntu22.04` image. Stage 2 copies the binary
  onto `nvidia/cuda:12.4.1-runtime-ubuntu22.04` and runs it as an
  unprivileged `anima` user.
- `docker-compose.yml` (at repo root) — wires the image up, mounts a named
  volume for the identity store at `~/.anima`, and reserves all attached
  NVIDIA GPUs via the `deploy.resources` block.
- `.dockerignore` — keeps `target/`, `web/node_modules/`, the static-site
  build, and the `.git` history out of the build context.

## What this spike does **not** do (yet)

The current LLM backends in `llm-backends/` are `mock`, `anthropic`, and
`openai`. None of them touch a local GPU — they replay fixtures or call
hosted APIs over HTTPS. So nothing the container runs today will actually
load a model onto the 3090.

The image is still built on the CUDA runtime base on purpose: it is the
piece that's painful to retrofit later. Once a local-inference backend
lands (e.g. a `llama.cpp` / `mistral.rs` / `candle` adaptor exposing the
`LlmBackend` trait), it can ship in the same image with no infrastructure
churn — just rebuild and the GPU is already exposed.

If you want a slimmer image for the mock/Anthropic/OpenAI path today,
swap both `FROM` lines in `docker/Dockerfile` for `debian:bookworm-slim`
and `rust:1-bookworm` respectively, and remove the `deploy.resources`
block from `docker-compose.yml`.

## Host prerequisites

- Docker 23+ (BuildKit is the default; the Dockerfile uses cache mounts).
- NVIDIA driver new enough for CUDA 12.4 (≥ 550.54).
- `nvidia-container-toolkit` installed and configured — see
  <https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/>.

Verify the toolkit is wired up before the first compose run:

```sh
docker run --rm --gpus all nvidia/cuda:12.4.1-base-ubuntu22.04 nvidia-smi
```

You should see the 3090 listed.

## Usage

From the repo root:

```sh
# Build the image and run the default two-agent mock-backend demo.
docker compose up --build

# Run an ad-hoc subcommand.
docker compose run --rm hosted why
docker compose run --rm hosted identity show
docker compose run --rm hosted identity set name "anima-prime"

# Confirm the 3090 is visible inside the container.
docker compose run --rm --entrypoint nvidia-smi hosted
```

To run against a live API backend, export the keys before bringing
compose up (or put them in a `.env` file next to `docker-compose.yml`):

```sh
export ANIMA_BACKEND=anthropic
export ANTHROPIC_API_KEY=sk-...
docker compose up --build
```

## What gets persisted

The `anima-data` named volume is mounted at `/home/anima/.anima`. The
identity-memory store (`~/.anima/anima/identity.json`) writes there, so
`identity set` survives container restarts. The volume is local to the
host's Docker engine; `docker volume rm animaos_anima-data` clears it.

## Image size

The CUDA runtime base is ~2.5 GB on disk. The `anima-hosted` binary
itself is ~1 MB. The size is dominated by the CUDA libraries, which is
the cost of being GPU-ready up front rather than later.

## Open follow-ups (out of scope for the spike)

1. Wire a local-inference `LlmBackend` (candle / mistral.rs / llama.cpp
   bindings) so the 3090 is actually exercised.
2. Add a long-running server entry point — today `anima-hosted` is a
   one-shot CLI demo. A daemon mode with an HTTP/UDS surface would make
   the container useful as a true service.
3. CI hook to build the image on push and publish to GHCR.
4. Optional second container for the Astro docs site (`web/`).
