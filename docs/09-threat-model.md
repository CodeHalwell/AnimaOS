# 09 — AnimaOS Threat Model

_This is a living document. It must be updated whenever a new external surface
is introduced, a dependency is promoted to a required role, or a Stage closes.
The canonical revision date is the commit date of the last substantive edit._

**Last substantive revision:** Stage 3 closure (Epic E3.6 merged).

---

## 1. Purpose and Scope

This document records AnimaOS's security assumptions, trust boundaries, attack
surface, threat catalogue, and current mitigations. It is the primary input to
the per-stage security review mandated by Epic EX.4.

Scope: the AnimaOS codebase as it exists in this repository — the hosted Rust
workspace (`crates/`, `kernels/hosted/`, `llm-backends/`), the Python cortex
process (`cortex/`, scoped to E5.1), and the CI pipeline (`.github/workflows/`).
The bare-metal microVM target (Stage 4) is out of scope until Stage 4 opens.

---

## 2. System Overview and Trust Boundaries

```
                          ┌──────────────────────────────────────┐
                          │           Host OS / Container         │
                          │                                       │
  ┌──────────┐  UDS/text  │  ┌─────────────┐    Rust IPC / UDS   │
  │  Human   │──────────► │  │    senses   │◄──────────────────  │
  │  Input   │            │  └──────┬──────┘                     │
  └──────────┘            │         │ SensoryPacket              │
                          │  ┌──────▼──────┐                     │
                          │  │    vita     │ (somatic runtime)   │
                          │  └──┬──────┬───┘                     │
                          │     │      │                         │
                          │  ┌──▼──┐ ┌─▼──────┐                 │
                          │  │sch- │ │memory  │                 │
                          │  │edu- │ │ (L1/2/ │                 │
                          │  │ler  │ │  L3)   │                 │
                          │  └──┬──┘ └────────┘                 │
                          │     │                                │
                          │  ┌──▼──────┐     ┌─────────────────┐│
                          │  │ praxis  │     │ cortex (Python) ││
                          │  │(tools)  │◄────│ (Epic E5.1+)    ││
                          │  └──┬──────┘     └─────────────────┘│
                          │     │                                │
                          └─────┼────────────────────────────────┘
                                │ HTTPS / TLS
                          ┌─────▼──────────────┐
                          │  LLM Provider APIs  │
                          │ (Anthropic / OpenAI)│
                          └─────────────────────┘
```

### Trust Zones

| Zone | Components | Trust Level |
|------|-----------|-------------|
| **TZ-1 Kernel** | `vita`, `corpus`, `scheduler`, `memory` | Fully trusted; Rust safety invariants hold |
| **TZ-2 Tools** | `praxis`, built-in tool drivers | Trusted; circuit-breaker–supervised |
| **TZ-3 Cortex** | Python `cortex/` subprocess (E5.1+) | Partially trusted; isolated by UDS + capability gating |
| **TZ-4 Sensorium** | `senses`, human text socket, PCM pipeline | Untrusted input boundary; all input validated before ingestion |
| **TZ-5 LLM APIs** | Anthropic, OpenAI provider endpoints | Untrusted remote; TLS enforced, responses treated as attacker-controlled |
| **TZ-6 Sandbox** | Wasmtime WASI modules (E2.5+) | Untrusted user code; gas-metered and capability-gated |
| **TZ-7 CI/CD** | GitHub Actions, crates.io | Semi-trusted; supply-chain controls documented in §6 |

### Capability Flow Rules

1. `vita` owns the master capability set. Tool-dispatching sub-capabilities are
   handed down to `praxis`; the cortex (TZ-3) receives only the subset defined
   by the active Thalamic Router route (E5.3).
2. Nothing in TZ-4 or TZ-5 may elevate to TZ-1 privileges without transiting
   the validation path in `anima-self`.
3. WASI modules (TZ-6) are confined to the capability set passed at module
   instantiation; they cannot request additional capabilities at runtime.

---

## 3. Attack Surface Catalogue

### AS-1: Human Input Socket

**Description.** The text socket at `/dev/anima/senses/human` (or its hosted
equivalent) accepts raw UTF-8 strings from the local user.

**Current controls.**
- `HumanGuidance` policy: `max_text_length`, `blocked_prefixes`.
- Empty inputs rejected with `PolicyViolation`; no panic path.
- Implemented in `crates/senses/src/lib.rs`.

**Residual risk.** Prompt injection via crafted user text that causes the cortex
to take unintended actions (see T-4 below).

---

### AS-2: PCM Audio Pipeline

**Description.** Raw PCM frames enter via `SensoryBridge::packetize_pcm_checked`.
The VAD stub accepts any non-empty frame; STT is deferred to E4.x.

**Current controls.**
- Non-empty frame validation; `PolicyViolation` on empty frame.
- VAD stub does not perform semantic analysis — audio content is opaque.

**Residual risk.** Audio content is a future vector for adversarial voice
injection. Mitigation deferred to the real VAD/STT implementation in E4.x.

---

### AS-3: LLM Provider Response

**Description.** Streamed token responses from Anthropic and OpenAI are parsed
and forwarded into `vita`. Malformed or adversarially crafted responses could
influence the cortex's plan or tool calls.

**Current controls.**
- Responses are deserialized through typed Rust structs (no `eval`).
- TLS enforced on outbound connections; fixtures used in CI (no live calls).
- Tool calls from the cortex are validated against the active route's capability
  scope before dispatch.

**Residual risk.** Indirect prompt injection (T-5) via LLM-generated content
that instructs the cortex to misuse tools (e.g. exfiltrate identity memory).

---

### AS-4: Python Cortex IPC

**Description.** The cortex subprocess communicates with `vita` over a Unix
Domain Socket using a length-prefixed JSON protocol (E5.1).

**Current controls.**
- UDS path is inside the agent state directory; not world-readable.
- Message length is bounded by the 4-byte header limit (4 GiB practical cap
  is enforced in `cortex/ipc.py`).
- Cortex crashes are isolated from `vita` (audit log records the crash).

**Residual risk.** Malicious JSON payloads from a compromised cortex. Mitigation:
the Rust deserialization side uses `serde_json` with typed structs.

---

### AS-5: WASI Tool Modules (E2.5+)

**Description.** Untrusted Wasm modules are instantiated inside Wasmtime with
gas metering and restricted capability imports.

**Current controls.**
- Gas meter tied to the scheduler's token slice.
- Capability-gated WASI imports (defined in E2.5 stories).
- Adversarial module tests (infinite loop, memory exhaustion) are an E2.5 exit
  criterion.

**Residual risk.** Side-channel attacks through timing of gas exhaustion signals.
Deferred until E2.5 closes.

---

### AS-6: L3 Archive and Identity Memory Files

**Description.** The L3 archive (`memory/archival.rs`) and identity memory
(`cortex/identity_memory.py`) are persisted to disk. A compromised host could
modify these files.

**Current controls.**
- Atomic writes via `.tmp`-then-rename in both Rust and Python paths.
- Content is validated on load (length checks, JSON schema validation in the
  Python side).

**Residual risk.** No cryptographic integrity protection. File-level tampering
by a host-privileged attacker would not be detected. Full mitigation requires
Stage 4 microVM attestation.

---

### AS-7: Dependency Supply Chain

**Description.** AnimaOS pulls ~60 transitive Rust crate dependencies. A
compromised crate release or a crate with a known CVE could introduce
vulnerabilities.

**Current controls.**
- `cargo audit` scans for RustSec advisories on every PR (this epic).
- `cargo deny` enforces licence, duplicate, and banned-crate policies (this
  epic).
- `Cargo.lock` is committed; dependency updates require a deliberate `cargo
  update` command and a passing CI run.

**Residual risk.** Zero-day vulnerabilities and typosquatting attacks (a
malicious crate published with a name similar to a legitimate dependency).
Mitigation: EX.4 is a *continuous* epic; advisories are re-checked on each PR.

---

## 4. Threat Catalogue

### T-1: Privilege Escalation via Capability Bypass

**STRIDE category.** Elevation of Privilege.

**Description.** An attacker-controlled component (cortex, WASI module, or
crafted tool output) attempts to acquire capabilities above its assigned TZ level.

**Current mitigations.**
- Capability typestate in `anima-self` prevents construction of
  `Capability<Verified>` outside the verification path (compile-fail tested,
  E1.2 exit criterion).
- Tool dispatch in `praxis` requires an explicit capability check before
  execution.

**Residual risk.** Logic errors in capability verification code. Mitigation:
Kani proofs for capability state transitions (E4.6).

---

### T-2: Memory Corruption

**STRIDE category.** Tampering.

**Description.** Buffer overflows, use-after-free, or integer overflows in Rust
unsafe blocks could allow arbitrary code execution.

**Current mitigations.**
- `#![forbid(unsafe_code)]` on all crates except `corpus` (E1.1 exit criterion).
- `corpus` unsafe blocks are audited; `FrameAllocator` audit log is unit-tested.
- Miri runs on the `corpus` suite in nightly CI (E4.6).

**Residual risk.** The `corpus` crate retains intentional unsafe code. Miri does
not cover all execution paths; Kani proofs (E4.6) close this gap for bounded
properties.

---

### T-3: Denial of Service via Resource Exhaustion

**STRIDE category.** Denial of Service.

**Description.** An attacker supplies inputs that cause unbounded memory or CPU
consumption (e.g. giant sensory packets, infinite LLM streams, WASI infinite
loops).

**Current mitigations.**
- `max_text_length` policy bound on sensory input (E3.3).
- Cancellation token interrupts LLM streams within one token (E1.3 exit
  criterion).
- WASI gas meter bounds CPU consumption per module (E2.5).
- MLFQ token-slice accounting prevents any single task from consuming the full
  scheduler budget (E1.4 exit criterion).

**Residual risk.** The PCM pipeline and STT component lack resource bounds until
E4.x. L3 archive growth is unbounded until a compaction policy is implemented.

---

### T-4: Direct Prompt Injection

**STRIDE category.** Tampering / Spoofing.

**Description.** A user (or a process with write access to the input socket)
sends crafted text designed to override the agent's system prompt or tool
policies.

**Current mitigations.**
- `blocked_prefixes` list in `HumanGuidance` policy.
- Tool dispatch is capability-scoped; the cortex cannot call tools outside its
  route's `ToolScope` regardless of what the LLM outputs.

**Residual risk.** Prefix matching is not a complete injection defence. A
prompt-injection classifier is part of the Defence Layer (E5.6).

---

### T-5: Indirect Prompt Injection

**STRIDE category.** Tampering.

**Description.** A malicious document, web page, or tool output contains
instructions that cause the LLM to perform unintended actions when those
instructions appear in context.

**Current mitigations.**
- Tool outputs are treated as attacker-controlled data in the cortex pipeline.
- Reward-hacking and goal-drift detectors planned in E5.6.

**Residual risk.** No current classifier; E5.6 is the primary mitigation path.

---

### T-6: Data Exfiltration via Tool Calls

**STRIDE category.** Information Disclosure.

**Description.** A compromised or misbehaving cortex uses tool calls (filesystem
read, network, text I/O) to exfiltrate identity memory or conversation content.

**Current mitigations.**
- Tool dispatch is capability-scoped (see T-1 mitigations).
- Every tool call is logged in the audit trail.
- Unsafe motor action gate (E5.6) will block reads of sensitive paths.

**Residual risk.** Until E5.6, the cortex can read any file within its capability
scope. The capability scope is intentionally narrow (E5.1 route scope).

---

### T-7: Supply Chain Compromise

**STRIDE category.** Tampering.

**Description.** A malicious or compromised dependency is introduced into the
dependency graph via a crates.io release or a compromised registry mirror.

**Current mitigations.**
- `Cargo.lock` pinning.
- `cargo audit` on every PR (this epic).
- `cargo deny` bans unknown registries and wildcard version specifiers (this
  epic).

**Residual risk.** A crate publisher's account being compromised between
`Cargo.lock` updates. Mitigation: `cargo deny` `unknown-registry = "deny"`
prevents pulling from unrecognised registries.

---

### T-8: Persistent State Tampering

**STRIDE category.** Tampering.

**Description.** A host-privileged attacker modifies the L3 archive, identity
memory, or audit log between agent runs.

**Current mitigations.**
- Atomic writes reduce the window for partial-write corruption.
- Idempotent demotion ensures re-inserted corrupted entries are detected on the
  next replay validation pass (E3.6).

**Residual risk.** No cryptographic integrity (hash-and-sign of the archive).
Full mitigation deferred to Stage 4 microVM attestation.

---

## 5. Security Controls Matrix

| Control | Threat(s) | Status | Epic |
|---------|-----------|--------|------|
| `#![forbid(unsafe_code)]` on all non-`corpus` crates | T-2 | ✅ Active | E1.1 |
| `FrameAllocator` audit log tested | T-2 | ✅ Active | E1.2 |
| Cancellation within one token | T-3 | ✅ Active | E1.3 |
| MLFQ token-slice budgets | T-3 | ✅ Active | E1.4 |
| `HumanGuidance` policy bounds | T-3, T-4 | ✅ Active | E3.3 |
| Capability typestate compile-fail test | T-1 | ✅ Active | E1.2 |
| Atomic archive writes | T-8 | ✅ Active | E2.6 |
| Replay validation rollback | T-8 | ✅ Active | E3.6 |
| `cargo audit` in CI | T-7 | ✅ Active | **EX.4** |
| `cargo deny` in CI | T-7 | ✅ Active | **EX.4** |
| Wasmtime gas metering + capability WASI | T-3, T-5 | 🟡 In PR | E2.5 |
| Cortex capability scoping via route | T-1, T-6 | 🟡 In PR (E5.1) | E5.1/E5.3 |
| Prompt-injection classifier | T-4, T-5 | ⬜ Planned | E5.6 |
| Goal-drift / reward-hacking detector | T-5 | ⬜ Planned | E5.6 |
| Unsafe motor action gate | T-6 | ⬜ Planned | E5.6 |
| Kani proofs for scheduler invariants | T-2 | ⬜ Planned | E4.6 |
| Miri clean on `corpus` suite | T-2 | ⬜ Planned | E4.6 |
| MicroVM attestation of persistent state | T-8 | ⬜ Planned | E4.x |

---

## 6. Supply-Chain Security

### 6.1 Dependency Management

- **Lock file.** `Cargo.lock` is committed and must not diverge from the
  workspace without a deliberate `cargo update` invocation.
- **Registry.** Only `https://github.com/rust-lang/crates.io-index` is
  allowed; `deny.toml` (`[sources]`) rejects unknown registries.
- **Wildcard versions.** `deny.toml` (`[bans]`) rejects any `"*"` version
  specification.
- **Banned crates.** `openssl` and `git2` are explicitly denied because
  they introduce C dependencies incompatible with the microVM target.
- **Advisory scanning.** `cargo audit` is run on every PR against the
  [RustSec Advisory Database](https://rustsec.org/advisories/). Findings
  at `error` severity block merge.
- **Licence compliance.** `cargo deny` enforces the licence allow-list;
  any new dependency must carry an approved licence or receive an explicit
  exception documented in `deny.toml`.

### 6.2 CI Pipeline Security

- **Minimal permissions.** The `ci.yml` workflow requests `contents: read`
  only; it does not have write access to the repository or package registries.
- **Pinned actions.** All `uses:` references should be pinned to a specific
  SHA in addition to the version tag. This is tracked as a future hardening
  item under EX.4.
- **No secret leakage.** No API keys or tokens appear in workflow files;
  live LLM calls are replaced with fixture-based replay in all CI jobs.

### 6.3 Open Risks

| Risk | Priority | Mitigation Path |
|------|----------|-----------------|
| GitHub Actions `uses:` pinned to tag not SHA | Medium | Pin to SHAs in a follow-up EX.4 iteration |
| No SBOM (Software Bill of Materials) generation | Low | `cargo cyclonedx` or `cargo spdx` in a future EX.4 iteration |
| No automated update bot (Dependabot/Renovate) | Low | Enable Dependabot once EX.4 is stable |

---

## 7. Security Review Checklist (per Stage)

At the closure of each stage, the following checklist must be signed off:

- [ ] `cargo audit` passes with zero `error`-level findings.
- [ ] `cargo deny` passes with zero `deny`-level findings.
- [ ] All new external surfaces are documented in §3 of this file.
- [ ] All new threats are catalogued in §4 and mitigations are current.
- [ ] The Security Controls Matrix in §5 is up to date.
- [ ] No new `unsafe` blocks have been introduced outside `corpus` without a
      documented justification in the PR.
- [ ] CI workflow permissions have not been widened beyond `contents: read`
      without documented justification.
- [ ] Any new crate dependency has an approved licence in `deny.toml` and is
      free of known advisories at merge time.

---

## 8. References

- [RustSec Advisory Database](https://rustsec.org/)
- [cargo-audit](https://github.com/rustsec/rustsec/tree/main/cargo-audit)
- [cargo-deny](https://embarkstudios.github.io/cargo-deny/)
- [STRIDE Threat Model](https://learn.microsoft.com/en-us/azure/security/develop/threat-modeling-tool-threats)
- `docs/01-architecture.md` — AnimaOS architecture and trust zones
- `docs/02-subsystems.md` — subsystem responsibilities
- `docs/08-cognitive-architecture.md` — cognitive layer design
- `deny.toml` — machine-readable policy (this repo root)
