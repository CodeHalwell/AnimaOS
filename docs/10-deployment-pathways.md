# 10 — Deployment pathways

AnimaOS is designed to terminate, eventually, as a bare-metal framekernel
(see [`05-roadmap.md`](./05-roadmap.md) phase 4 + the `kernels/microvm/`
work). That endpoint is far from where iteration happens today, so the
project supports **two parallel deployment surfaces** that share the same
workspace code:

1. **Containerised** — fast iteration on a developer workstation, Ollama
   as the inference engine, Unsloth for fine-tuning, nvidia-container-toolkit
   for GPU passthrough.
2. **Bare-metal native** — the eventual production surface: AnimaOS as
   PID 1 (or close to it), inference linked directly into the address
   space, no Docker daemon between the agent and the hardware.

This document captures what's shared, what diverges, and where the seams
are so the bare-metal cut-over doesn't require rewriting AnimaOS itself.

## Why two paths

> "I'll probably try and prove this works in docker and then move to the
> bare metal version." — workspace owner, 2026-05-27.

The container path makes the cognitive architecture provable: it lets us
verify that the wake/sleep cycle, the three-tier router, and the
sense → instinct → workhorse → API hierarchy actually behave the way the
design documents promise. Containers cost ~5 % overhead vs. bare-metal
on a 3090 (mostly cold-start and HTTP loopback) — well under the noise
floor of "is this design correct."

The bare-metal path is then a transport optimisation, not a design pivot.
Same Rust workspace, same crates, same `LlmBackend` trait — different
deployment surface.

## What's shared between the two paths

Everything in the Rust workspace:

- The `anima-hosted` binary (Linux-process kernel).
- All ten core crates (`corpus`, `vita`, `scheduler`, `memory`, `praxis`,
  `self`, `interoception`, `senses`, `defence`, `kv-controller`).
- The `LlmBackend` trait surface in `crates/scheduler/src/backend.rs`.
- The router + gate (`crates/vita/src/router.rs`, `gate.rs`).
- All audit and identity-memory infrastructure.

Backends are **swappable** at the trait level. The container path uses
`OllamaBackend`; the bare-metal path will substitute a Rust-native
backend (candle / mistralrs / llama-cpp-2) that fulfils the same
contract. No code outside `llm-backends/` knows the difference.

## Containerised path (today)

```
host: docker + nvidia-container-toolkit
  ├── compose service: ollama          (GPU passthrough, GGUF inference)
  ├── compose service: ollama-init     (one-shot model pull)
  ├── compose service: hosted          (anima-hosted talking HTTP to ollama)
  └── compose service: trainer         (Unsloth QLoRA, profile-gated)
                                       (shares ollama-models volume)
```

- **Inference**: Ollama wraps llama.cpp's CUDA kernels — Ampere-class
  throughput, identical to a host-native Ollama install.
- **Training**: Unsloth runs in a separate container, mounts the same
  models volume Ollama uses, performs QLoRA passes during sleep phases,
  exports GGUF, and Ollama hot-reloads.
- **Iteration speed**: `docker compose up --build` and you're running.
  Model swaps are `docker compose run --rm ollama-init ollama pull <tag>`.
- **GPU sharing**: both `ollama` and `trainer` reserve the same device;
  the wake/sleep cycle naturally serialises them, so contention never
  occurs in practice.

Full operational details live in [`../docker/README.md`](../docker/README.md).

## Bare-metal native path (target)

```
host: Linux (or eventually the microVM kernel)
  ├── ollama (host daemon, or omitted entirely)
  └── anima-hosted (or anima-microvm) — agent process, links inference
                                         crate in-process if Ollama is gone
```

Three flavours, in increasing order of how much you're committing to
the framekernel vision:

### Flavour A — host-native sidecar (lowest friction)

Same architecture as the container path, minus Docker:

- Install Ollama on the host (`curl -fsSL https://ollama.com/install.sh | sh`).
- Install Unsloth in a venv (or conda env) on the host.
- Build and run `anima-hosted` directly: `cargo run --release -p hosted`.
- `ANIMA_OLLAMA_URL=http://127.0.0.1:11434` (loopback instead of compose DNS).

Throughput: identical to containerised. The only thing you lose is the
isolation Docker provides — and the only thing you gain is no container
runtime overhead, which is already negligible. Useful when you want to
debug `anima-hosted` with native gdb / rr without `docker exec`.

### Flavour B — in-process inference (Rust-native)

Replace the HTTP boundary with a direct Rust binding. New backend lives
under `llm-backends/src/local/` and implements the existing trait:

```rust
pub struct LocalLlamaBackend {
    model: llama_cpp_2::model::LlamaModel,
    ctx: llama_cpp_2::context::LlamaContext,
    ...
}

impl LlmBackend for LocalLlamaBackend { ... }
```

Candidates: `llama-cpp-2` (C FFI to llama.cpp, fastest), `mistralrs`
(pure-Rust with CUDA via cudarc), `candle` (pure-Rust, slowest but
cleanest with the workspace's `#![forbid(unsafe_code)]` posture
outside `corpus`).

Saves the HTTP round-trip and the second process. Cost: the build
matrix gets heavier (CMake, CUDA toolkit at compile time) and the
`anima-hosted` binary grows from ~1 MB to ~100 MB plus model weights
in memory.

### Flavour C — microVM framekernel (the long-arc target)

`kernels/microvm/` is the bare-metal UEFI kernel under construction.
Phase 4 of the roadmap (E4.1–E4.4, in progress) brings it up to the
point where it can boot, run Embassy's async executor, drive smoltcp
for the network, and terminate TLS — enough to call an external
inference service.

At that point AnimaOS becomes the OS, with no Linux underneath, no
Docker, no Python venv, and inference is the only thing on the wire.
This is the inversion the project name implies: the agent isn't on
top of an OS, it *is* the OS.

## Migration order

Recommended sequence (each step is independently shippable):

1. **Today** — containerised path, prove the wake/sleep cycle and
   three-tier router work against live local models.
2. **Next** — wire the router so it actually dispatches to tier-specific
   backends (currently `anima-hosted` picks one backend at startup).
   Doesn't change deployment, just makes the existing infrastructure
   meaningful.
3. **Next** — sleep-phase training loop in `trainer/`. Replay buffer →
   Unsloth QLoRA → GGUF export → Ollama reload. Closes the cognitive
   architecture's outer loop.
4. **Then** — flavour A on a host without Docker, to remove a layer
   while still iterating on the same Linux substrate.
5. **Then** — flavour B (in-process Rust inference) when the deployment
   target is a known single user. Removes Ollama as a dep.
6. **Eventually** — flavour C, once `kernels/microvm/` reaches the
   point where it can host the cognitive stack.

Each step preserves the trait surface, so the cognitive code never gets
rewritten.

## Risks to track

- **GGUF availability for the 270 M instinct tier.** Ollama's catalogue
  starts at ~0.5 B (qwen2.5:0.5b) and ~1 B (llama3.2:1b). A genuine
  270 M instinct model probably needs custom training + `ollama create`
  from a GGUF you produce in the trainer.
- **CUDA-toolkit drift.** Container path is pinned to CUDA 12.4; the
  bare-metal flavour B build picks up whatever toolkit is installed
  host-side. Track a single supported pair (driver ≥ 550.54, CUDA 12.4).
- **`LifecycleManager` is single-backend.** The router knows about
  three tiers, but `LifecycleManager::new` takes one `Arc<dyn LlmBackend>`.
  Multi-tier dispatch wants a router-aware wrapper; this is the same
  change required for both deployment paths and should be done once.
