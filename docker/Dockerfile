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

# Pre-create ~/.anima (and the audit dir inside it) owned by the anima user.
# Compose mounts the named `anima-data` volume at /home/anima/.anima; a named
# volume inherits ownership from the image's directory on first use, but if
# the directory does not exist in the image the mount point is created
# root-owned and the uid-1000 process cannot write its identity store or
# audit log.
#
# curl ships in the runtime image for the container HEALTHCHECK below (and
# for exec-debugging against the console from inside the network namespace).
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /bin/bash --uid 1000 anima \
    && mkdir -p /home/anima/.anima/audit \
    && chown -R anima:anima /home/anima/.anima

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

# Liveness: /healthz is the console's dedicated probe — it is exempt from
# bearer-token auth, so the check keeps working when ANIMA_CONSOLE_TOKEN is
# set (GET / would return 401 and mark a healthy container unhealthy).  The
# console binds 0.0.0.0:8088 by default (see ANIMA_CONSOLE_ADDR above), so
# loopback inside the container always reaches it.  Irrelevant for one-shot
# subcommand containers, which exit before the start period elapses.
HEALTHCHECK --interval=15s --timeout=3s --start-period=10s --retries=3 \
  CMD curl -sf http://127.0.0.1:8088/healthz || exit 1

ENTRYPOINT ["anima-hosted"]
# Default to the operator-console server. Override for one-shot subcommands,
# e.g. `docker compose run --rm hosted why`.
CMD ["serve"]
