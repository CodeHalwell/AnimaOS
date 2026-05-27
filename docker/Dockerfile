# syntax=docker/dockerfile:1.7
#
# AnimaOS hosted-kernel container.
#
# Multi-stage build:
#   1. builder   — nvidia/cuda devel image + rustup, compiles `anima-hosted`
#   2. runtime   — nvidia/cuda runtime image, runs the binary
#
# Both stages share the same Ubuntu 22.04 base so the glibc the binary was
# linked against is the same one available at runtime. The runtime image
# ships CUDA 12.4 libraries so a future local-inference LLM backend can
# reach an attached NVIDIA GPU (e.g. the user's RTX 3090) without rebuilding
# the image.
#
# Build from repo root:
#   docker build -f docker/Dockerfile -t animaos/hosted:dev .
#
# Or via compose:
#   docker compose up --build

# ---- Stage 1: build ---------------------------------------------------------
FROM nvidia/cuda:12.4.1-devel-ubuntu22.04 AS builder

ENV DEBIAN_FRONTEND=noninteractive \
    CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    PATH=/usr/local/cargo/bin:$PATH

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        curl \
        pkg-config \
        libssl-dev \
    && rm -rf /var/lib/apt/lists/*

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable --profile minimal

WORKDIR /src
COPY . .

# BuildKit cache mounts keep the cargo registry and target/ warm across
# rebuilds. Requires Docker 23+ (BuildKit is the default there).
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release -p hosted --bin anima-hosted \
    && cp target/release/anima-hosted /usr/local/bin/anima-hosted

# ---- Stage 2: runtime -------------------------------------------------------
FROM nvidia/cuda:12.4.1-runtime-ubuntu22.04 AS runtime

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /bin/bash --uid 1000 anima

COPY --from=builder /usr/local/bin/anima-hosted /usr/local/bin/anima-hosted

USER anima
WORKDIR /home/anima

# Default backend is the deterministic mock; override at runtime to use a
# live provider, e.g. `-e ANIMA_BACKEND=anthropic -e ANTHROPIC_API_KEY=...`.
#
# NVIDIA_* hints let nvidia-container-toolkit expose the host GPU(s) when
# the container is launched with `--gpus all` or the compose device
# reservation block below.
ENV ANIMA_BACKEND=mock \
    NVIDIA_VISIBLE_DEVICES=all \
    NVIDIA_DRIVER_CAPABILITIES=compute,utility

ENTRYPOINT ["anima-hosted"]
CMD []
