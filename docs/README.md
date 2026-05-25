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

## Status

Early implementation. The Cargo workspace, the eight crates named in `01-architecture.md`, and the hosted kernel target are in place; the MLFQ scheduler, bounded token pipe, three-tier memory shells, circuit breaker, capability typestate, stress-index monitor, and sensory bridge primitives are all merged and tested. Phase 1's exit criteria are partially met. See `05-roadmap.md` for the per-milestone state.

The documents in this suite are the authoritative reference during implementation and are updated in lockstep with the codebase. Anatomical crate names in the documentation map one-to-one to Cargo packages: `corpus`, `vita`, `praxis`, `senses`, and `interoception` use those package names directly, while the `self` crate is published as `anima-self` (imported as `anima_self`) because `self` is a reserved Rust keyword. See the glossary for the full mapping.

## Provenance

Anima is a renaming and expansion of the earlier Axon OS specification. The technical substance is preserved; the framing has been adjusted so that the system's name reflects what is distinctive about it — the animating lifecycle — rather than a single neural component.

See [`06-glossary.md`](./06-glossary.md) for the full mapping between old and new terminology.
