# Getting Started with AnimaOS

> **Epic E9 S9.7 — Unified quickstart.** This document replaces the scattered
> first-run information previously spread across `README.md`, `docker/README.md`,
> and `docs/10-deployment-pathways.md`.  One page, one flow.

---

## What you are booting

AnimaOS is not a chat interface — it is an **agent operating system**.  The agent
is the primary actor; you, as operator, are a *sensor*: your guidance enters the
somatic queue and is arbitrated by the Striatal Gate before any task is admitted.
Nothing you send bypasses the agent's policy machinery.

The architecture lives in `docs/01-architecture.md`.  This page teaches you to
stand it up in five minutes.

---

## Prerequisites

| Tool | Minimum version | Notes |
|---|---|---|
| Rust toolchain | stable (2024 edition) | `rustup update stable` |
| Docker + Compose | v2.20+ | container path only |
| Git | any recent | — |

**Optional but recommended:**

- **Ollama** — local inference for the cheap-local tier (`https://ollama.com`)
- **ANTHROPIC_API_KEY** or **OPENAI_API_KEY** — for frontier routing
- NVIDIA GPU (≥ 4 GiB VRAM), Apple Silicon, or CPU-only (all supported)

---

## Quick path: Docker (recommended for first run)

```bash
# 1. Clone
git clone https://github.com/codehalwell/animaos.git
cd animaos

# 2. (Optional) Set your API key
export ANTHROPIC_API_KEY=sk-ant-...     # or OPENAI_API_KEY

# 3. Run preflight
cargo run --bin anima-hosted -- doctor

# 4. Run the first-run wizard (seeds providers and identity)
cargo run --bin anima-hosted -- init

# 5. Start the agent and console
docker compose -f docker-compose.mock.yml up --build   # zero-dependency MVP (mock LLM)
# …or, with an NVIDIA GPU + Ollama models:
docker compose up --build
```

The console dashboard is available at **http://127.0.0.1:8088/** once the
container is running.  Send your first guidance:

```bash
cargo run --bin anima-console -- send "hello" --url http://127.0.0.1:8088
```

---

## Quick path: native (no Docker)

```bash
# Build everything
cargo build --workspace

# Preflight
cargo run --bin anima-hosted -- doctor

# First-run wizard
cargo run --bin anima-hosted -- init

# Boot the agent with the operator console
ANIMA_BACKEND=mock cargo run --bin anima-hosted -- serve
# Replace `mock` with `ollama`, `anthropic`, or `openai` depending on your setup.
```

---

## Step-by-step

### Step 1 — Preflight: `anima doctor`

```
cargo run --bin anima-hosted -- doctor
```

`anima doctor` detects:

- **GPU** — NVIDIA (via `nvidia-smi`), Apple Silicon, or CPU-only
- **RAM** — from `/proc/meminfo` (Linux) or `sysctl` (macOS)
- **Local providers** — TCP-connects to Ollama (:11434), LM Studio (:1234),
  vLLM (:8000), and llama.cpp-server (:8080)
- **API keys** — checks `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`

It prints a tier recommendation:

```
━━━ Hardware
  GPU  : GeForce RTX 3090 (24 GiB VRAM)
  RAM  : ~32 GiB

━━━ Local providers
  ollama         ✅ REACHABLE  127.0.0.1:11434  tier=cheap-local
  lmstudio       ❌ NOT FOUND  127.0.0.1:1234   tier=mid-tier
  anthropic      ✅ REACHABLE  api.anthropic.com  tier=frontier  [env configured]

━━━ Recommendation
  cheap-local  → ollama (GGUF via Ollama; run: `ollama pull llama3.2:3b`)
  mid-tier     → ollama (or anthropic)
  frontier     → anthropic
```

### Step 2 — First-run wizard: `anima init`

```
cargo run --bin anima-hosted -- init
```

The wizard walks through three steps and is **idempotent** — re-running skips
completed steps.

1. **Preflight** — runs `doctor` and flags blocking issues
2. **Provider binding** — confirms or adjusts the tier→backend mapping
3. **Identity bootstrap** — asks your name and seeds `identity.json`

For CI or headless environments:

```bash
cargo run --bin anima-hosted -- init --non-interactive
```

Non-interactive mode prints the suggested config and exits without prompting.

State is persisted in `~/.anima/anima/onboarding.json`.

### Step 3 — Boot the agent: `anima serve`

```bash
# With a local Ollama backend:
ANIMA_BACKEND=ollama cargo run --bin anima-hosted -- serve

# With Anthropic (requires ANTHROPIC_API_KEY):
ANIMA_BACKEND=anthropic cargo run --bin anima-hosted -- serve

# With the built-in deterministic mock (CI default):
ANIMA_BACKEND=mock cargo run --bin anima-hosted -- serve
```

The console binds on **http://127.0.0.1:8088** by default
(`ANIMA_CONSOLE_PORT` overrides the port; `ANIMA_CONSOLE_TOKEN` sets a bearer
token for the `/guidance` endpoint).

### Step 4 — Interact via the console

**Browser dashboard** — open http://127.0.0.1:8088/

**CLI client:**

```bash
# Stream the event feed (SSE):
cargo run --bin anima-console -- tap --url http://127.0.0.1:8088

# Send guidance:
cargo run --bin anima-console -- send "summarise the overnight logs" \
  --url http://127.0.0.1:8088 --priority High

# Full TUI:
cargo run --bin anima-console -- tui --url http://127.0.0.1:8088
```

**Forced operator guidance** (audited override):

```bash
cargo run --bin anima-console -- send "emergency shutdown" \
  --force --reason "system maintenance" --url http://127.0.0.1:8088
```

---

## Identity memory

The agent stores stable facts about you and its own configuration in
`~/.anima/anima/identity.json`.

```bash
# View the full identity document:
cargo run --bin anima-hosted -- identity show

# View a single fact:
cargo run --bin anima-hosted -- identity show operator_name

# Set a fact (audited):
cargo run --bin anima-hosted -- identity set operator_name "Alice"
cargo run --bin anima-hosted -- identity set working_hours "09:00-18:00 UTC"
```

Every `set` is written to the audit log; inspect the trail with `anima why`:

```bash
cargo run --bin anima-hosted -- why
```

---

## Hardware paths

### NVIDIA GPU

Install Ollama and pull a model:

```bash
ollama pull llama3.2:3b      # cheap-local tier  (~2 GiB VRAM)
ollama pull llama3.1:8b      # mid-tier          (~5 GiB VRAM)
```

Then: `ANIMA_BACKEND=ollama cargo run --bin anima-hosted -- serve`

### Apple Silicon (M-series)

Ollama runs natively with Metal acceleration — same commands as NVIDIA above.
`anima doctor` will report `Apple Silicon (unified memory)`.

### CPU-only

Ollama also runs on CPU.  Expect slower inference; the agent is otherwise
fully functional.  `anima doctor` will report `CPU-only` and the recommendation
will still suggest Ollama.

### Hosted-API-only (no local GPU)

Set `ANTHROPIC_API_KEY` or `OPENAI_API_KEY` and use:

```bash
ANIMA_BACKEND=anthropic cargo run --bin anima-hosted -- serve
```

Only the frontier tier will be available; cheap-local falls back to `mock`.

---

## Troubleshooting

### `anima doctor` shows all providers as NOT FOUND

- Ollama is not running — start it with `ollama serve`
- LM Studio's API server is not enabled (Settings → Local Server → Start)
- vLLM is not running on the expected port

### Console says `port already in use`

Another process is using port 8088.  Override with:

```bash
ANIMA_CONSOLE_PORT=9000 cargo run --bin anima-hosted -- serve
```

### Builds fail with `missing serde_json`

Run `cargo build --workspace` from the repo root (not from a subdirectory) to
ensure workspace-level dependencies are resolved.

---

## What's next

| Capability | Command / Link |
|---|---|
| Understand a gate decision | `anima-hosted why` |
| Inspect the audit log | `~/.anima/audit/anima.jsonl` |
| Full architecture | `docs/01-architecture.md` |
| Operator interface | `docs/11-operator-interface.md` |
| Deployment paths | `docs/10-deployment-pathways.md` |
| Real-world tools (E7) | `docs/12-real-world-tools-plan.md` |
| Local LLM ecosystem (E8) | `docs/13-local-llm-providers.md` |
