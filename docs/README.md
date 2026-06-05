# Anima

*A living substrate for autonomous agents.*

---

## What this is

Anima is a bare-metal, cloud-isolated framekernel operating system designed around a single inversion: the operating system does not manage software for a human at a screen. It serves as the body, autonomic nervous system, and reflex arcs for an LLM agent that runs as `init` and supervises itself.

The model is not "an agent running on an OS." The agent and the OS are one organism. Anima is what makes that organism alive — its breath, its lifecycle, its capacity to sleep, consolidate, and wake again.

The human operator is not the screen user. The human is modelled as a high-priority environmental signal provider, mounted alongside the agent's other senses.

## Design principles

1. **The agent owns its own body.** The agent runs at PID 1. It allocates its own memory tiers, schedules its own downtime, decides when to consolidate, and sequences its own execution loops within human-defined policy bounds.

2. **Safety by quarantine.** Unsafe Rust is permitted only inside `corpus`, the privileged Trusted Computing Base. Every other crate declares `#![forbid(unsafe_code)]`. The blast radius of memory unsafety is bounded by construction.

3. **Lifecycle is structural, not optional.** Sleep, dreaming, and memory consolidation are first-class architectural elements rather than performance optimisations to be skipped under load. An Anima system that never sleeps is a malfunctioning Anima system.

4. **The metaphor is load-bearing.** Anatomical and biological terms are used because they describe the actual structure of the system: afferent input, efferent action, interoception, homeostasis. They are not decoration. Engineers reading the codebase should be able to map a Rust function to a physiological role and back.

5. **Bare-metal compliance.** No host OS dependencies in the production target. `smoltcp` for networking, `rustls` for transport security, `wasmtime` for sandboxing untrusted code, all compiled into a unikernel image suitable for Firecracker or Cloud Hypervisor.

## Document suite

This directory contains the full design for Anima. Read in order if you are new; otherwise jump to what you need.

| File | Subject |
|------|---------|
| [`01-architecture.md`](./01-architecture.md) | System architecture, crate workspace, technical stack, and the human-as-peripheral model. |
| [`02-subsystems.md`](./02-subsystems.md) | Detailed specifications for memory, praxis, interoception, senses, and security subsystems. |
| [`03-lifecycle.md`](./03-lifecycle.md) | The homeostatic loop: waking execution, sleep state phases, transitions, and dreaming. |
| [`04-verification.md`](./04-verification.md) | Testing strategy, formal verification posture, and continuous integration. |
| [`05-roadmap.md`](./05-roadmap.md) | The 24-month phased implementation plan with milestones, exit criteria, and risks. |
| [`06-glossary.md`](./06-glossary.md) | Terminology mapping: anatomical names ↔ engineering meanings. |
| [`07-implementation-plan.md`](./07-implementation-plan.md) | Epic-by-epic delivery plan, aligned to the roadmap milestones, with stories and exit criteria. |
| [`08-cognitive-architecture.md`](./08-cognitive-architecture.md) | Cognitive layer above the somatic substrate: cortex, gate, router, learned KV-cache controller, episodic/identity memory, defence layer. Reconciled into Stage 5 of the implementation plan. |
| [`09-threat-model.md`](./09-threat-model.md) | Security posture: trust boundaries, attack surfaces, mitigations, and the defence layer's role. |
| [`10-deployment-pathways.md`](./10-deployment-pathways.md) | The two parallel deployment surfaces — containerised (Docker + Ollama + Unsloth) for iteration, bare-metal native for the production target — and how the workspace stays one codebase across both. |
| [`11-operator-interface.md`](./11-operator-interface.md) | The human↔agent interface (Epic E6): the operator console. Human-as-a-sense afferent guidance + an efferent telemetry/event stream, one `console-proto` wire protocol over two transports — HTTP/SSE in the container, COM1 serial in the microVM. |

### Forward epics (E7–E12, proposed)

Plans for the autonomous-agent layer built on top of the shipped somatic core.
These are **scoping documents**, not yet implemented. Start with the index
([`18-forward-epics.md`](./18-forward-epics.md)) for the dependency map and
build sequence.

| File | Epic | Subject |
|------|------|---------|
| [`12-real-world-tools-plan.md`](./12-real-world-tools-plan.md) | **E7 — Embodiment** | Real-world tools: web-search (SearXNG), browser (Playwright), egress/SSRF guard, semantic tool selection, live Anthropic/Ollama tool-calling. |
| [`13-local-llm-providers.md`](./13-local-llm-providers.md) | **E8 — Local Inference** | Provider ecosystem (OpenAI-compatible umbrella + native runtimes), Unsloth fine-tuning, HRA for the instinct tier, eval harness, adapter library. |
| [`14-onboarding.md`](./14-onboarding.md) | **E9 — Onboarding** | First-run wizard, `anima doctor` preflight, conversational identity bootstrap, non-NVIDIA support, per-tier router dispatch. |
| [`15-communication-multimodal.md`](./15-communication-multimodal.md) | **E10 — Presence** | Comms-app channel gateways (Telegram/Slack), text/image/voice as first-class bidirectional modalities. |
| [`16-skills-and-self-extension.md`](./16-skills-and-self-extension.md) | **E11 — Self-Extension** | Anthropic Agent Skills model; agent-registered skills and tools (sandboxed, gated); the self-improvement loop. |
| [`17-motivation-and-drives.md`](./17-motivation-and-drives.md) | **E12 — Motivation** | Six-tier drive hierarchy feeding the Striatal Gate; endogenous goals; corrigibility invariant. |
| [`18-forward-epics.md`](./18-forward-epics.md) | *(index)* | Forward-epics catalogue: dependency graph, shared-spine primitives, recommended build sequence. |

## Status

Stages 1–3 closed, Stage 5 (cognitive layer) closed, Stage 4 closed through E4.6 (no_std port, Embassy executor, smoltcp TCP/IP, TLS 1.3 in the microVM, higher-crate microVM port, Kani + Miri formal verification). Stage 6 (operator console) closed through E6.4 + E6.6 (E6.5 microVM Phase 1 gated on virtio-net). Stage 4's remaining work is Epic E4.7 — production hardening and the 30-day soak run. The workspace now holds twelve crates: the foundational eight from `01-architecture.md` plus `kv-controller` (E5.4), `defence` (E5.6), `console-proto` (E6.1), and `console` (E6.2) introduced in Stages 5–6. See `07-implementation-plan.md` for the epic-by-epic state.

The documents in this suite are the authoritative reference during implementation and are updated in lockstep with the codebase. Anatomical crate names in the documentation map one-to-one to Cargo packages: `corpus`, `vita`, `praxis`, `senses`, and `interoception` use those package names directly, while the `self` crate is published as `anima-self` (imported as `anima_self`) because `self` is a reserved Rust keyword. See the glossary for the full mapping.

## Provenance

Anima is a renaming and expansion of the earlier Axon OS specification. The technical substance is preserved; the framing has been adjusted so that the system's name reflects what is distinctive about it — the animating lifecycle — rather than a single neural component.

See [`06-glossary.md`](./06-glossary.md) for the full mapping between old and new terminology.
