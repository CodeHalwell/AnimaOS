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

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable --profile minimal

WORKDIR /src
COPY . .

# BuildKit cache mounts keep cargo registry + target/ warm across rebuilds.
# Requires Docker 23+ (BuildKit is the default there).
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p hosted --bin anima-hosted \
    && cp target/release/anima-hosted /usr/local/bin/anima-hosted

# ---- Stage 2: runtime -------------------------------------------------------
FROM debian:bookworm-slim AS runtime

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /bin/bash --uid 1000 anima

COPY --from=builder /usr/local/bin/anima-hosted /usr/local/bin/anima-hosted

USER anima
WORKDIR /home/anima

# Defaults assume the sibling `ollama` compose service is reachable as
# `ollama:11434`.  Override with `-e ANIMA_BACKEND=mock` to bypass.
ENV ANIMA_BACKEND=ollama \
    ANIMA_OLLAMA_URL=http://ollama:11434 \
    ANIMA_OLLAMA_MODEL=llama3.2:3b \
    ANIMA_OLLAMA_CTX=8192

ENTRYPOINT ["anima-hosted"]
CMD []
