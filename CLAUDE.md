# CLAUDE.md

Guidance for AI assistants (Claude Code and others) working in the AnimaOS
repository. Read this before making changes; it captures the structure,
workflows, and conventions that are easy to get wrong.

## What this project is

AnimaOS is a bare-metal, cloud-isolated **framekernel OS** that acts as the
*somatic architecture* (physical body, autonomic nervous system, and reflex
arcs) for an autonomous LLM agent. The agent runs as `init`/PID 1 and
supervises itself; the human operator is modelled as a high-priority
*environmental signal provider* (a sense), not as the screen user.

The codebase is a **Rust workspace** of ~35 library crates plus two kernel
targets, a Python cognitive layer (`cortex/`), an Astro/React docs site
(`web/`), and a fine-tuning trainer image (`trainer/`). The biological/
anatomical naming is **load-bearing**, not decorative — a function name maps to
a physiological role (afferent = input, efferent = action, interoception =
internal sensing, homeostasis = self-regulation). Preserve this metaphor when
naming things.

The authoritative design suite lives in [`docs/`](./docs/README.md). The
epic-by-epic status with exit criteria is in
[`docs/07-implementation-plan.md`](./docs/07-implementation-plan.md). When a
change touches a subsystem, the relevant `docs/NN-*.md` is usually the best
context.

## Repository layout

```
AnimaOS/
├── Cargo.toml              # Root workspace (34 crates + kernels/hosted + llm-backends)
├── crates/                 # Library crates, grouped in layers (see below)
├── kernels/
│   ├── hosted/             # `anima-hosted` Linux-process binary — DEV/CI ONLY (stable Rust)
│   └── microvm/            # `anima-microvm` x86_64-unknown-uefi framekernel — PRODUCTION (nightly)
├── llm-backends/           # Anthropic / OpenAI / Ollama / OpenAI-compat / native providers
├── cortex/                 # Python cognitive layer (plan/act/observe/revise over UDS IPC)
├── trainer/                # Unsloth sleep-phase QLoRA trainer (GPU, docker-compose profile)
├── xtask/                  # SEPARATE workspace: soak / bench-baseline / demo / finetune drivers
├── web/                    # Astro + React GitHub Pages site
├── docs/                   # Full design suite (01–23 + glossary, getting-started)
├── bench/baselines/        # Criterion regression baselines (memory/praxis/scheduler.json)
├── artifacts/              # Demo / soak artefacts (NOT for new source)
├── docker/                 # docker-compose stack docs
└── .github/workflows/      # ci, bench, nightly, soak, docker, pages, release-sbom
```

### Crate layers (`crates/`)

- **Somatic core (E1–E6):** `corpus` (the TCB — frame allocator, PCB, syscalls),
  `vita` (lifecycle director + sleep routines + router/gate), `scheduler`
  (3-tier MLFQ + token pipe + `LlmBackend`), `memory` (CLS L1/L2/L3 + decay +
  TurboQuant), `praxis` (efferent actuator + circuit breaker + WASM sandbox),
  `self`/`anima-self` (capability typestate barrier), `interoception` (stress
  index + sensors), `senses` (afferent input), `kv-controller` (learned KV
  cache), `defence` (injection/drift/reward-hack detection), `console-proto` +
  `console` (operator console).
- **Autonomy layer (E7–E17):** `actuators`, `finetune`, `comms`, `skills`,
  `motivation`, `constitution`, `lifecycle`, `users`.
- **Operational wave (E18–E30):** `quota`, `metrics` (Prometheus aggregator +
  registry; the former `metrics-endpoint` crate was merged in),
  `config`, `sessions`, `consent`, `feedback`, `analytics`, `tool-cache`,
  `knowledge-graph`, `alerts`, `webhooks`, `diagnostics`.
- **Multi-tenancy & scheduling (E31–E32):** `workspace`, `jobs`.

> **`self` crate gotcha:** the directory `crates/self/` contains the package
> **`anima-self`** (Rust import path `anima_self`). `self` is a reserved Rust
> keyword and cannot be a crate name.

## Build, test, and run

### Workspace (hosted/dev target — stable Rust)

```sh
cargo build --workspace --all-targets
cargo test  --workspace --all-targets
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings

# Development convenience binary (the hosted kernel):
cargo run -p hosted --bin anima-hosted -- serve        # boots one agent + operator console
cargo run -p hosted --bin anima-hosted -- doctor       # GPU/preflight check
cargo run -p hosted --bin anima-hosted -- init         # first-run onboarding wizard
cargo run -p hosted --bin anima-hosted -- identity show # inspect agent identity memory
```

`anima-hosted serve` publishes the operator dashboard at
**http://127.0.0.1:8088/**. Attach the terminal UI with
`anima-console tui --url http://127.0.0.1:8088` (the `anima-console` binary is
in `crates/console`).

### xtask (SEPARATE workspace — manifest `xtask/Cargo.toml`)

`xtask` is **not** a member of the root workspace, so root `cargo build/test`
does **not** cover it. Use the `cargo xtask` alias (defined in
`.cargo/config.toml`) or `--manifest-path xtask/Cargo.toml`. CI gates it with
dedicated `xtask-{fmt,build,clippy,test}` jobs — keep them green independently.

```sh
cargo xtask demo --kind graceful     # graceful-degradation demo (writes artifacts/)
cargo xtask demo --kind retention    # long-horizon retention demo
cargo xtask soak --hours 1 --efi <path-to>/anima-microvm.efi
```

### microVM kernel (PRODUCTION — nightly + bare-metal)

`kernels/microvm` is its **own standalone workspace** (`[workspace]` in its
`Cargo.toml`) and pins nightly via its own `rust-toolchain.toml`. It targets
`x86_64-unknown-uefi` with `-Z build-std`.

```sh
cd kernels/microvm
cargo +nightly build           # debug  (≤ 6 MiB EFI budget)
cargo +nightly build --release # release (≤ 1 MiB EFI budget, enforced in CI)
cargo +nightly fmt -- --check
cargo +nightly clippy -- -D warnings
```

> **TLS crypto build flags:** the bare-metal TLS stack needs force-soft CFG
> flags so LLVM does not try to lower AES-NI/CLMUL intrinsics for the UEFI
> target. CI sets `RUSTFLAGS` with `--cfg aes_force_soft --cfg
> polyval_force_soft --cfg sha2_force_soft --cfg sha2_backend="soft"`. If a
> local microVM build fails in the crypto crates, this is why.

> **Both targets must build.** New subsystem features must compile for **both**
> the hosted target and the microVM target before landing on `main`. Many
> crates are `no_std`-aware: they gate std-only modules behind a `std` feature
> and the microVM depends on them with `default-features = false` (often
> `features = ["libm"]` for float math). When adding code to a core crate,
> respect the `#[cfg(feature = "std")]` / `#![cfg_attr(not(feature = "std"),
> no_std)]` split — see `crates/memory/src/lib.rs` for the canonical pattern.

### Containerised dev stack (Docker)

```sh
docker compose -f docker-compose.mock.yml up --build   # zero-dep MVP, deterministic mock backend
docker compose up --build                              # NVIDIA GPU + Ollama
docker compose -f docker-compose.yml -f docker-compose.cpu.yml up --build  # CPU-only
docker compose -f docker-compose.apple.yml up --build  # Apple Silicon (host Ollama)
docker compose --profile training up --build           # + Unsloth trainer
```

Operational details (model defaults, env vars, VRAM budget) are in
[`docker/README.md`](./docker/README.md); copy `.env.example` to `.env`.

### Web docs site

```sh
cd web && npm install && npm run dev   # astro dev server; npm run build for prod
```

### Python cortex

`cortex/` is the cognitive service (LangGraph-style plan/act/observe/revise
loop) connected over length-prefixed JSON-over-UDS IPC. Tests live alongside
(`cortex/test_agent_loop_real.py`). The hosted kernel bridges to it via
`PythonCortexBridge`.

## Conventions and invariants (do not break these)

1. **Unsafe quarantine.** Every workspace **library** crate **must** declare
   `#![forbid(unsafe_code)]` at the crate root — the **only** exception is
   `crates/corpus` (the TCB), where unsafe is permitted but **audited** in
   `crates/corpus/unsafe_audit.md`. CI's `unsafe-quarantine` job greps for the
   attribute and fails the build if it is missing. If you add a new crate, add
   the forbid attribute; if you add audited unsafe to `corpus`, update
   `unsafe_audit.md`.

2. **`-D warnings` everywhere.** CI sets `RUSTFLAGS: -D warnings`; clippy runs
   with `-D warnings`. Warnings fail the build. There are no `rustfmt.toml` /
   `clippy.toml` overrides — use default rustfmt; run `cargo fmt --all` before
   committing.

3. **Kani proof harnesses** are gated behind `#[cfg(kani)]` and run in nightly
   CI (15 proofs across `corpus` + `scheduler`). Crates that carry proofs
   declare `cfg(kani)` as a known cfg in `[lints.rust]` to avoid the
   `unexpected_cfgs` lint (see `crates/corpus/Cargo.toml`). Don't remove these.

4. **Supply-chain policy.** `deny.toml` governs licences, banned crates
   (`openssl`, `git2`), duplicate versions, and allowed sources. CI runs
   `cargo audit` and `cargo deny check`. Prefer pure-Rust / rustls crates over
   anything pulling OpenSSL. SBOMs are generated per manifest.

5. **Workspace dependency hygiene.** Shared deps (`anyhow`, `serde`,
   `serde_json`, `criterion`, `wasmtime`) are pinned in the root
   `[workspace.dependencies]`; reference them with `{ workspace = true }`.
   Package metadata (`version`/`edition`/`license`) uses `.workspace = true`.

6. **microVM serial markers gate CI.** The microVM boot test (in `ci.yml`)
   greps the QEMU COM1 serial log for exit-criteria markers
   (`E4.2_TASK_DONE`, `E4.3_TCP_DONE`, `E4.4_TLS_DONE`, `E4.5_SOAK_DONE`,
   `E4.5B_VITA_DONE`, `E6.5_NET_DONE`, `E6.5_GUIDANCE_OK`, `ANIMA_PANIC`). If
   you change kernel boot behaviour, keep these markers emitting.

7. **Benchmark regression gate.** `bench.yml` runs Criterion benches and
   compares against `bench/baselines/{memory,praxis,scheduler}.json`. A
   performance regression fails CI; intentional baseline updates are a
   deliberate, reviewed change.

8. **Keep the metaphor consistent.** Use the anatomical vocabulary already in
   the codebase (see `docs/06-glossary.md`). Don't rename `vita`, `praxis`,
   `corpus`, etc. to generic terms.

## CI workflows (`.github/workflows/`)

| Workflow            | Gates                                                                 |
|---------------------|----------------------------------------------------------------------|
| `ci.yml`            | fmt, unsafe-quarantine, build+test, clippy, xtask-{fmt,build,clippy,test}, cargo-audit, cargo-deny, SBOM, microVM UEFI build (≤1 MiB) + QEMU boot-marker verification |
| `bench.yml`         | Criterion benches + regression gate vs `bench/baselines/`            |
| `nightly.yml`       | Kani bounded model checking (15 proofs) + Miri                       |
| `soak.yml`          | microVM soak harness smoke test (manual dispatch)                   |
| `docker.yml`        | Hosted image build + mock-backend smoke test                        |
| `pages.yml`         | Build/deploy the `web/` docs site                                   |
| `release-sbom.yml`  | Release SBOM publication                                            |

CI triggers on PRs to `main` and pushes to `main`.

## Workflow expectations for assistants

- **Before finishing a Rust change:** run `cargo fmt --all`, `cargo build
  --workspace --all-targets`, `cargo test --workspace --all-targets`, and
  `cargo clippy --workspace --all-targets -- -D warnings`. If you touched
  `xtask/`, run the same against `xtask/Cargo.toml`. If you touched a core
  somatic crate, sanity-check the microVM build too.
- **Match surrounding style** — comment density, naming, and the
  documentation-comment idiom (crates carry rich `//!` module docs and often
  reference the epic/story IDs like `E4.5` / `S9.3`). Cite the relevant epic in
  comments when it adds context, consistent with existing code.
- **Don't add new source under `artifacts/`** — it holds generated demo/soak
  outputs and is intentionally not the place for code.
- **Reach for the docs.** For non-trivial subsystem work, read the matching
  `docs/NN-*.md` and `docs/07-implementation-plan.md` first.

## Git / contribution notes

- Develop on the feature branch you were assigned; create it locally if
  needed. Do **not** push to `main` without explicit permission, and do **not**
  open a pull request unless explicitly asked.
- Push with `git push -u origin <branch-name>`; retry transient network
  failures with exponential backoff.
- Write clear, descriptive commit messages.
