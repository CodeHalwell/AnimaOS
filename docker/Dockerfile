# syntax=docker/dockerfile:1.7
#
# AnimaOS hosted-kernel container.
#
# Multi-stage build:
#   1. builder   — debian-slim + rustup, compiles `anima-hosted` against
#                  the workspace.
#   2. runtime   — debian-slim, ships only the binary + CA certs.
#
# The heavy GPU work happens in the sibling `ollama` compose service, which
# wraps llama.cpp's CUDA kernels.  AnimaOS itself only needs an HTTP client
# to reach it, so the agent container stays small (~120 MB) and starts
# instantly.  Switching the runtime to a CUDA-bearing base is a single-line
# change if we ever load a model directly from `anima-hosted`.
#
# Build from repo root:
#   docker build -f docker/Dockerfile -t animaos/hosted:dev .
#
# Or via compose:
#   docker compose up --build

# ---- Stage 1: build ---------------------------------------------------------
FROM debian:bookworm-slim AS builder

ENV DEBIAN_FRONTEND=noninteractive \
    CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    PATH=/usr/local/cargo/bin:$PATH

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        curl \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

# Download-then-execute rather than `curl | sh`: in a pipeline the RUN step's
# exit status is `sh`'s, so a curl failure (proxy, TLS, DNS) would silently
# produce a layer with no toolchain and only surface later as `cargo: not
# found` (exit 127).  `curl -f` + a separate execution step fails loudly at
# the point of error.
RUN curl --proto '=https' --tlsv1.2 -fsSf -o /tmp/rustup-init.sh https://sh.rustup.rs \
    && sh /tmp/rustup-init.sh -y --default-toolchain stable --profile minimal \
    && rm /tmp/rustup-init.sh

WORKDIR /src
COPY . .

# BuildKit cache mounts keep cargo registry + target/ warm across rebuilds.
# Requires Docker 23+ (BuildKit is the default there).
# Build both the agent (`anima-hosted`) and the operator client
# (`anima-console`): the latter ships in the image so `docker compose exec`
# can run the TUI/tap against the live agent from inside the container network.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p hosted --bin anima-hosted -p console --bin anima-console \
    && cp target/release/anima-hosted /usr/local/bin/anima-hosted \
    && cp target/release/anima-console /usr/local/bin/anima-console

# ---- Stage 2: runtime -------------------------------------------------------
FROM debian:bookworm-slim AS runtime

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /bin/bash --uid 1000 anima

COPY --from=builder /usr/local/bin/anima-hosted /usr/local/bin/anima-hosted
COPY --from=builder /usr/local/bin/anima-console /usr/local/bin/anima-console

USER anima
WORKDIR /home/anima

# Defaults assume the sibling `ollama` compose service is reachable as
# `ollama:11434`.  Override with `-e ANIMA_BACKEND=mock` to bypass.
#
# ANIMA_CONSOLE_ADDR binds the operator console to all interfaces *inside* the
# container; compose maps only loopback on the host (127.0.0.1:8088).  Set
# ANIMA_CONSOLE_TOKEN to require a bearer token.  ANIMA_AUDIT_DIR lives under
# the persisted ~/.anima volume so the console can tail it.
ENV ANIMA_BACKEND=ollama \
    ANIMA_OLLAMA_URL=http://ollama:11434 \
    ANIMA_OLLAMA_MODEL=hf.co/unsloth/Qwen3.5-9B-MTP-GGUF:Q4_K_M \
    ANIMA_OLLAMA_CTX=8192 \
    ANIMA_CONSOLE_ADDR=0.0.0.0:8088 \
    ANIMA_AUDIT_DIR=/home/anima/.anima/audit

# The operator console (only published to host loopback by compose).
EXPOSE 8088

ENTRYPOINT ["anima-hosted"]
# Default to the operator-console server. Override for one-shot subcommands,
# e.g. `docker compose run --rm hosted why`.
CMD ["serve"]
