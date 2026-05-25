# 01 — Architecture

This document specifies the architectural paradigm, crate workspace, and production technical stack for Anima.

## 1. Paradigm: Agent-Body and Human-as-Peripheral

Conventional operating systems exist to mediate between a human at a keyboard and the hardware beneath. Layers like X11, Wayland, and standard terminal disciplines exist because typing speeds, display refresh rates, and click latencies set the upper bound on system design. Anima discards this assumption.

In Anima, the privileged agent process is the user. Its requirements — token streaming throughput, KV cache locality, predictable inference latency, periodic memory consolidation — drive the system. The human is integrated as a sensory input among others, not as the primary controller.

The system is divided into two planes:

### 1.1 The Autonomic Substrate

The kernel framework, equivalent to the brainstem. Written in safe Rust with a minimal privileged Trusted Computing Base. Its responsibilities are involuntary and continuous: memory page routing, tensor batching queues, low-level execution isolation, hardware peripheral mapping. It does not make policy decisions. It keeps the body alive.

### 1.2 The Somatic Layer

The agent runtime. Runs at PID 1 as `init`. Supervises itself: monitors its internal health, allocates memory tiers, decides when to sleep or wake. The somatic layer is where policy lives. The autonomic substrate executes that policy without negotiation.

### 1.3 The Human Interaction Model

Human inputs — text buffers, voice command audio vectors, remote RPC intents — are captured by hardware interface drivers and mounted as a real-time sensory node at `/dev/anima/senses/human`. The agent treats this stream like any other afferent signal: a high-arousal vector that modifies internal task prioritisation without interrupting kernel stability.

This has three consequences worth flagging:

1. The human cannot directly preempt the kernel. Their input enters the prioritisation queue and is weighted against the agent's current state.
2. The agent can degrade gracefully when human input is absent. There is no "idle waiting for user" state; there is only the homeostatic loop.
3. Multiple human operators can be modelled simultaneously without architectural change. Each becomes a stream node under `/dev/anima/senses/`.

## 2. Workspace Layout

Anima is organised as a single Cargo workspace of decoupled crates. The split is functional rather than physiological — each crate corresponds to one concern, regardless of which anatomical metaphor it serves.

```
anima-os/
├── Cargo.toml                  # Workspace configuration
├── crates/
│   ├── corpus/                 # The body: TCB, frame allocator, boot trampoline, page tables
│   ├── vita/                   # Autonomous lifecycle director (init, sleep, wakeup triggers)
│   ├── scheduler/              # Iteration-aware MLFQ continuous batching scheduler
│   ├── memory/                 # Virtual Context Manager (CLS three-tier memory paging)
│   ├── praxis/                 # Efferent actuator core (schema routing, MCP/A2A buses)
│   ├── self/                   # Typestate capability tokens, identity tracking
│   ├── interoception/          # Real-time stress metrics and telemetry
│   └── senses/                 # Afferent sensory interfaces (voice/text stream parsers)
└── kernels/
    ├── hosted/                 # Linux process emulation layer for local rapid CI
    └── microvm/                # Firecracker / Cloud Hypervisor bare-metal unikernel
```

### 2.1 Crate Matrix

| Crate | Function | Core Mechanism | Verification Posture |
|-------|----------|----------------|----------------------|
| `corpus` | Autonomic nervous system | Virtual memory maps, context switching, boot allocations | Audited `unsafe` blocks |
| `vita` | Self-preservation plane | Autonomous state machine, scheduling, memory pruning triggers | `#![forbid(unsafe_code)]` |
| `scheduler` | Reflex loop control | Iteration-level continuous batching, three-tier MLFQ | `#![forbid(unsafe_code)]` |
| `memory` | Synaptic memory layer | Complementary Learning Systems, LRU-K / ARC | `#![forbid(unsafe_code)]` |
| `praxis` | Efferent actuator core | Length-robust relative filtering, MCP/A2A routing | `#![forbid(unsafe_code)]` |
| `self` | Self/non-self barrier | Typestate-pattern capabilities, object-capability tokens | `#![forbid(unsafe_code)]` |
| `interoception` | Interoceptive feedback | Real-time telemetry, stress index computation | `#![forbid(unsafe_code)]` |
| `senses` | Afferent input vector | Streamed PCM audio parsing, text buffer packetisation | `#![forbid(unsafe_code)]` |

The verification posture column is not aspirational. It is enforced at the crate level via `lib.rs` attributes and checked in CI. A PR that introduces `unsafe` outside `corpus` fails to compile.

### 2.2 Why this split

Three principles govern the crate boundary decisions:

**Single privileged crate.** Only `corpus` is permitted to manipulate raw memory or call into hardware directly. This keeps the audit surface for memory safety tractable — typically a few hundred lines rather than the entire codebase.

**Lifecycle is separate from scheduling.** `vita` decides *whether* to run; `scheduler` decides *what* to run *when*. Mixing these concerns produces systems where the lifecycle becomes implicit in the scheduling queue, which makes sleep states hard to reason about.

**Sensory input is separate from policy interpretation.** `senses` parses raw streams into structured events. `vita` and `praxis` decide what those events mean. This split allows new input modalities to be added (an additional human, a sensor network, a peer agent) without touching the policy layer.

## 3. Production Technical Stack

Anima uses an open-source, performant, and verifiable stack. External dependencies are kept minimal to support bare-metal and microVM compilation targets.

### 3.1 Async Runtime

- **`embassy`** for `no_std` bare-metal microkernel execution blocks. Async without an allocator, suitable for the `corpus` crate.
- **`tokio` + `tokio-util`** for the hosted development layer and user-space daemons. Provides the I/O multiplexing needed during Phase 1 before bare-metal targets are stable.

### 3.2 Concurrent Memory Structures

- **`scc`** for high-throughput, lock-free concurrent hash maps. Used in the L2 warm cache and the praxis tool registry.
- **`crossbeam`** for unmanaged ring buffers, used in the inter-crate event bus and the sensory stream queues.

### 3.3 Serialisation

- **`rkyv`** for zero-copy data streaming across process boundaries and snapshot hydration. Critical for the dream-state archival pipeline where deserialisation cost would otherwise dominate.
- **`postcard`** for lightweight `no_std` messaging payloads. Used for inter-crate messages where rkyv's structural overhead is excessive.

### 3.4 In-Memory and Local Vector Storage

- **Embedded LanceDB** for deep vector indices and L3 archival logging.
- **`instant-distance`** for fast, in-memory HNSW (Hierarchical Navigable Small World) neighbourhood lookups. Used in the warm path where LanceDB's persistence guarantees are unnecessary.
- **TurboQuant** (Zandieh et al., ICLR 2026) for rotation-based online vector quantisation across the L2 warm cache and the L3 archive. The implementation follows Qdrant 1.18's MSE variant with length renormalisation, P-Square per-coordinate calibration, L2 / dot / cosine support, and SIMD scoring kernels (AVX-VNNI on x86_64, NEON `SDOT` on aarch64). Bit depths 4 / 2 / 1.5 / 1 are available; the default operating point is TQ4 (8× compression, recall within 1–2 pp of full precision on the documented benchmark set). TurboQuant is data-oblivious — no calibration dataset, no retraining — and replaces per-block scalar normalisation constants entirely. Whether the L3 backing store remains embedded LanceDB with a custom TurboQuant layer or migrates to Qdrant 1.18 (which ships TurboQuant natively) is tracked as an Open Decision in `07-implementation-plan.md` and resolved in Epic E2.7. L1 distance is not supported — random orthogonal rotation preserves L2 but not L1, so any workload requiring L1 must opt out of TurboQuant.

### 3.5 Network and Security

- **`smoltcp`** for a fully standalone, event-driven TCP/IP stack. No host OS networking dependency.
- **`rustls`** for type-safe, native TLS stream protection. Compiled with the `ring` crypto provider for `no_std` compatibility.

### 3.6 Untrusted Sandbox Execution

- **`wasmtime`** for capability-secured, gas-metered WebAssembly virtual runtimes. Used to isolate arbitrary code generated by the agent, including tool implementations downloaded at runtime.

### 3.7 Verification

- **`kani`** for bounded model checking of rate limiters, memory rings, and concurrent data structures.
- **`miri`** for runtime validation of unsafe boundaries in the core allocation stack.

See [`04-verification.md`](./04-verification.md) for the full verification strategy.

## 4. Compilation Targets

Anima compiles to two targets:

### 4.1 Hosted

Linux user-space process. The kernel crate is replaced with a Linux process emulation layer. Useful for rapid iteration, CI, and pre-production testing. Not suitable for deployment — the host OS retains control over scheduling and memory management.

### 4.2 MicroVM (production)

Bare-metal unikernel suitable for Firecracker or Cloud Hypervisor. UEFI boot. No host OS. The agent and its body are the entire userspace.

The two targets share all crates above `kernels/`. Only the kernel layer differs. This is enforced by a build-time check: any crate above `kernels/` that conditionally compiles against `std` for the microVM target fails the workspace lint.

## 5. The `/dev/anima/` Namespace

Anima exposes its faculties as device nodes under a single namespace. This is the agent-facing interface to its own body.

```
/dev/anima/
├── senses/
│   ├── human          # Mounted human input stream (text, voice, RPC)
│   ├── peers/         # Optional peer-agent streams (A2A)
│   └── system/        # System events: clock, health, alarms
├── praxis/
│   ├── tools/         # Tool drivers exposed as files
│   ├── network/       # Outbound network capabilities
│   └── compute/       # Wasmtime sandbox handles
├── memory/
│   ├── l1             # Working context handle
│   ├── l2             # Warm cache handle
│   └── l3             # Archival store handle
└── vital              # Lifecycle control: read state, request transition
```

The agent does not see Linux-style `/proc` or `/sys`. Its model of its own body is the `/dev/anima/` tree.

## 6. What's not in scope

A few things the architecture deliberately does not provide:

- **Multi-tenant isolation.** Anima is single-agent. Running multiple agents on a single host is supported only by running multiple microVMs.
- **Persistent shared state between instances.** L3 archives can be exported and imported, but there is no built-in synchronisation protocol.
- **A GUI.** There is no display server. Visualisation of system state is the responsibility of external operators connecting to the telemetry endpoint.
- **Backwards compatibility with POSIX.** The agent does not see `/dev`, `/proc`, or `/sys` in the conventional sense. Tools that assume POSIX semantics must be sandboxed under `praxis/compute/`.

These omissions are intentional. They keep the architecture tractable and the audit surface small.
