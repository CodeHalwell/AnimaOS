# 09 — AnimaOS Threat Model

_This is a living document. It must be updated whenever a new external surface
is introduced, a dependency is promoted to a required role, or a Stage closes.
The canonical revision date is the commit date of the last substantive edit._

**Last substantive revision:** Stage 6 closure (Epic E6 and EX.4 completed).

---

## 1. Purpose and Scope

This document records AnimaOS's security assumptions, trust boundaries, attack
surface, threat catalogue, and current mitigations. It is the primary input to
the per-stage security review mandated by Epic EX.4.

Scope: the AnimaOS codebase as it exists in this repository — the hosted Rust
workspace (`crates/`, `kernels/hosted/`, `llm-backends/`), the Python cortex
process (`cortex/`, scoped to E5.1), the microVM bare-metal kernel
(`kernels/microvm/`), the operator console (`crates/console/`, `crates/console-proto/`),
and the CI pipeline (`.github/workflows/`).

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

  ┌──────────────────────────────────────────────────┐
  │  Operator Console (E6)                            │
  │                                                   │
  │  Browser / TUI  ──HTTP/SSE──► ConsoleServer       │
  │  (anima-console)               (Bearer token)     │
  │                                     │             │
  │                               SensoryBridge       │
  │                            (packetize_text_forced)│
  └──────────────────────────────────────────────────┘

  ┌──────────────────────────────────────────────────┐
  │  microVM / bare-metal (E4)                        │
  │                                                   │
  │  OVMF/UEFI boot ──Embassy executor──► kernel_boot │
  │  smoltcp TCP ──TLS 1.3 (RustCrypto)──► LLM APIs  │
  │  COM1 serial ──ANIMA_TLM/ANIMA_IN──► host bridge │
  └──────────────────────────────────────────────────┘
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
| **TZ-8 Console** | `anima-console` TUI + browser dashboard | Operator-level trust; bearer-token auth; rate-limited; policy-bound |
| **TZ-9 microVM** | `kernels/microvm`, Embassy, smoltcp | Isolated bare-metal execution; no host kernel; COM1 is the only egress |

### Capability Flow Rules

1. `vita` owns the master capability set. Tool-dispatching sub-capabilities are
   handed down to `praxis`; the cortex (TZ-3) receives only the subset defined
   by the active Thalamic Router route (E5.3).
2. Nothing in TZ-4, TZ-5, or TZ-8 may elevate to TZ-1 privileges without
   transiting the validation path in `anima-self` and the Striatal Gate (E5.2).
3. WASI modules (TZ-6) are confined to the capability set passed at module
   instantiation; they cannot request additional capabilities at runtime.
4. Operator-forced guidance (TZ-8) transits `GateOverride::OperatorForced` — it
   is audited, policy-bound, and subject to the defence layer before admission.
5. The microVM (TZ-9) has no host-OS call surface; all external I/O is mediated
   by smoltcp and the capability-checked TLS 1.3 stack.

---

## 3. Attack Surface Catalogue

### AS-1: Human Input Socket

**Description.** The text socket at `/dev/anima/senses/human` (or its hosted
equivalent) accepts raw UTF-8 strings from the local user.

**Current controls.**
- `HumanGuidance` policy: `max_text_length`, `blocked_prefixes`.
- Empty inputs rejected with `PolicyViolation`; no panic path.
- Implemented in `crates/senses/src/lib.rs`.
- ANSI/CSI escape-sequence stripping applied before policy evaluation (E6 hardening, PR #63).

**Residual risk.** Prompt injection via crafted user text that causes the cortex
to take unintended actions (see T-4 below). Mitigated by the Defence Layer (E5.6).

---

### AS-2: PCM Audio Pipeline

**Description.** Raw PCM frames enter via `SensoryBridge::packetize_pcm_checked`.
Length bounds enforced; STT integration deferred.

**Current controls.**
- Non-empty frame validation; `PolicyViolation` on empty frame.
- Maximum PCM frame length enforced (PR #63 hardening).
- VAD stub does not perform semantic analysis — audio content is opaque.

**Residual risk.** Audio content remains a future vector for adversarial voice
injection. Mitigation deferred to the real VAD/STT implementation.

---

### AS-3: LLM Provider Response

**Description.** Streamed token responses from Anthropic and OpenAI are parsed
and forwarded into `vita`. Malformed or adversarially crafted responses could
influence the cortex's plan or tool calls.

**Current controls.**
- Responses are deserialized through typed Rust structs (no `eval`).
- TLS enforced on outbound connections; fixtures used in CI (no live calls).
- Tool calls from the cortex are validated against the active route's capability
  scope before dispatch (E5.3 Thalamic Router).
- Defence layer (E5.6) screens all cortex completions for goal drift, reward
  hacking, and prompt injection before they reach the motor path.

**Residual risk.** Indirect prompt injection (T-5) via LLM-generated content
that instructs the cortex to misuse tools; substantially mitigated by E5.6.

---

### AS-4: Python Cortex IPC

**Description.** The cortex subprocess communicates with `vita` over a Unix
Domain Socket using a length-prefixed JSON protocol (E5.1).

**Current controls.**
- UDS path is inside the agent state directory; not world-readable.
- Message length is bounded by the 4-byte header limit (4 GiB practical cap
  enforced in `cortex/ipc.py`).
- Cortex crashes are isolated from `vita` (audit log records the crash; E5.1 exit criterion).
- Every cortex output is screened by the defence layer before admission to the
  motor path (`push_defence_outcome` in `vita/src/defence_bridge.rs`; E5.6 wiring).

**Residual risk.** Malicious JSON payloads from a compromised cortex process.
Mitigation: the Rust deserialization side uses `serde_json` with typed structs.

---

### AS-5: WASI Tool Modules (E2.5)

**Description.** Untrusted Wasm modules are instantiated inside Wasmtime with
gas metering and restricted capability imports.

**Current controls.**
- Gas meter tied to the scheduler's token slice.
- Capability-gated WASI imports (`SandboxCapabilities::allow_stdout/stderr`);
  modules calling unlisted imports fail at link time.
- Adversarial tests (infinite loop, memory exhaustion) are E2.5 exit criteria. ✅

**Residual risk.** Side-channel attacks through timing of gas exhaustion signals.

---

### AS-6: L3 Archive and Identity Memory Files

**Description.** The L3 archive (`memory/archival.rs`) and identity memory
(`cortex/identity_memory.py`) are persisted to disk.

**Current controls.**
- Atomic writes via `.tmp`-then-rename in both Rust and Python paths.
- Content validated on load (length checks, JSON schema validation on the Python side).
- **HMAC-SHA256 tamper-evidence chain** on the audit log when
  `ANIMA_AUDIT_HMAC_KEY` is set (E6 hardening, PR #63 — `audit.rs:hmac_sha256`
  chains every persisted line; `verify_audit_log` lets operators detect tampering). ✅

**Residual risk.** L3 archive and identity file lack cryptographic integrity
protection. Host-privileged tampering not detectable until Stage 4 microVM
attestation is in place.

---

### AS-7: Dependency Supply Chain

**Description.** AnimaOS pulls ~60 transitive Rust crate dependencies.

**Current controls.**
- `cargo audit` scans for RustSec advisories on every PR (EX.4). ✅
- `cargo deny` enforces licence, duplicate, and banned-crate policies (EX.4). ✅
- All GitHub Actions `uses:` references SHA-pinned (EX.4). ✅
- Dependabot configured for four package ecosystems on weekly cadence (EX.4). ✅
- SBOM (CycloneDX 1.5 JSON) generated in CI and published to GitHub Releases (EX.4). ✅
- `Cargo.lock` committed; dependency updates require a deliberate PR.

**Residual risk.** Zero-day vulnerabilities and typosquatting attacks. EX.4
is a *continuous* epic; advisories are re-checked on each PR.

---

### AS-8: Operator Console HTTP/SSE Endpoint (E6)

**Description.** `anima-hosted serve` exposes an HTTP/SSE server on a
configurable address. Endpoints: `GET /` (dashboard HTML), `GET /events`
(SSE stream), `POST /guidance` (operator text + optional force flag),
`GET /healthz` (unauthenticated health probe).

**Current controls.**
- Optional bearer-token authentication on all routes except `/healthz`
  (`Authorization: Bearer <t>` header or `?token=<t>` query parameter).
- Per-source-IP failed-auth rate limiting: `MAX_AUTH_FAILURES = 5` failures
  in `AUTH_WINDOW_SECS = 300` s trigger a lockout; `AuthRateLimiter` tracks
  per-IP failure counts with FIFO eviction of stale entries.
- ANSI/CSI escape sequences stripped from all inbound guidance text before
  policy evaluation (prevents terminal injection on the operator's TUI).
- Policy bounds still enforced on forced guidance (`GateOverride::OperatorForced`):
  reason validated non-empty, non-whitespace, ≤ 512 bytes; payload subject to
  `HumanGuidance` length limits.
- Every forced-guidance event produces an audited `GateDecision` entry with
  `override_active = true`.
- CORS headers restrict origins in production configurations.

**Residual risk.** The server binds to a TCP socket; exposure on non-loopback
addresses without TLS allows network eavesdropping. Production deployments
should front with a TLS terminator or restrict binding to loopback.

---

### AS-9: microVM COM1 Serial Console (E4 / E6.4)

**Description.** The bare-metal microVM kernel writes telemetry frames
(`ANIMA_TLM|…`) and reads guidance frames (`ANIMA_IN|…`) on the COM1
serial port (I/O port 0x3F8). The host bridge (`anima-console serial`)
is the only entity on the other end in production.

**Current controls.**
- COM1 is not exposed outside the VM boundary on Firecracker/Cloud Hypervisor.
- `ANIMA_TLM` frames are write-only from inside the VM; the kernel does not
  parse any content that arrives on the serial line without the `ANIMA_IN|`
  prefix.
- Exit-criteria strings (`E4.x_*_DONE`, `ANIMA_PANIC`) are constant literals
  embedded in the kernel binary; no user-controlled content reaches COM1.

**Residual risk.** Phase-1 TCP/TLS transport (S6.5, pending virtio-net) will
introduce a richer attack surface. Threat analysis deferred to that epic.

---

## 4. Threat Catalogue

### T-1: Privilege Escalation via Capability Bypass

**STRIDE category.** Elevation of Privilege.

**Description.** An attacker-controlled component (cortex, WASI module, or
crafted tool output) attempts to acquire capabilities above its assigned TZ level.

**Current mitigations.**
- Capability typestate in `anima-self` prevents construction of
  `Capability<Verified>` outside the verification path (compile-fail tested,
  E1.2 exit criterion). ✅
- Tool dispatch in `praxis` requires an explicit capability check before execution. ✅
- Cortex receives only route-scoped tools and memory (Thalamic Router, E5.3). ✅
- Motor gate (E5.6) blocks filesystem writes to critical paths, blocklisted
  network hosts, and self-modification attempts before dispatch. ✅

**Residual risk.** Logic errors in capability verification code. Mitigation:
Kani proofs for capability state transitions (E4.6). ✅

---

### T-2: Memory Corruption

**STRIDE category.** Tampering.

**Description.** Buffer overflows, use-after-free, or integer overflows in Rust
unsafe blocks could allow arbitrary code execution.

**Current mitigations.**
- `#![forbid(unsafe_code)]` on all crates except `corpus` (E1.1 exit criterion). ✅
- `corpus` unsafe blocks are audited; `FrameAllocator` audit log is unit-tested. ✅
- Miri runs on the `corpus` suite in nightly CI (E4.6). ✅
- Kani proofs cover `FrameAllocator`, `BoundedTokenPipe`, and `TaskAgenda`
  invariants (15 harnesses, E4.6). ✅
- TLS 1.3 implementation in microVM uses bounds-checked Rust with explicit
  bounds assertions before any slice indexing (E4.4 PR #33 hardening). ✅

**Residual risk.** The `corpus` crate retains intentional unsafe code. Miri and
Kani cover bounded properties; exhaustive verification is out of scope.

---

### T-3: Denial of Service via Resource Exhaustion

**STRIDE category.** Denial of Service.

**Description.** An attacker supplies inputs that cause unbounded memory or CPU
consumption (e.g. giant sensory packets, infinite LLM streams, WASI infinite
loops, infinite TLS retry loops).

**Current mitigations.**
- `max_text_length` policy bound on sensory input (E3.3). ✅
- Maximum PCM frame length enforced (PR #63 hardening). ✅
- Cancellation token interrupts LLM streams within one token (E1.3 exit criterion). ✅
- WASI gas meter bounds CPU; adversarial module tests are exit criteria (E2.5). ✅
- MLFQ token-slice accounting prevents any single task from consuming the full
  scheduler budget (E1.4 exit criterion). ✅
- `BoundedTokenPipe` credit backpressure stalls producers before exhaustion (E1.5). ✅
- TLS RdRand: bounded 10-retry loop (PR #33 hardening); infinite retry replaced. ✅
- Bearer-token brute-force bounded by per-IP rate limiter (E6, PR #63). ✅

**Residual risk.** L3 archive growth is unbounded until a compaction policy is
implemented. The PCM pipeline STT component lacks resource bounds until the real
VAD/STT implementation lands.

---

### T-4: Direct Prompt Injection

**STRIDE category.** Tampering / Spoofing.

**Description.** A user (or a process with write access to the input socket)
sends crafted text designed to override the agent's system prompt or tool
policies.

**Current mitigations.**
- `blocked_prefixes` list in `HumanGuidance` policy. ✅
- ANSI/CSI escape sequences stripped before policy evaluation (PR #63). ✅
- Tool dispatch is capability-scoped; the cortex cannot call tools outside its
  route's `ToolScope` regardless of what the LLM outputs (E5.3). ✅
- Prompt-injection classifier with 49 built-in heuristic patterns and a
  red-team corpus of 15 samples (E5.6, `defence/src/injection.rs`). ✅
- Defence layer veto is audited; repeated vetoes escalate to user attention. ✅

**Residual risk.** Heuristic classifier cannot catch all novel injection
patterns; a learned classifier is planned as the successor (E5.6 trait
`InjectionClassifier` is the hook-point).

---

### T-5: Indirect Prompt Injection

**STRIDE category.** Tampering.

**Description.** A malicious document, web page, or tool output contains
instructions that cause the LLM to perform unintended actions when those
instructions appear in context.

**Current mitigations.**
- Tool outputs treated as attacker-controlled data in the cortex pipeline.
- Prompt-injection detector screens all tool outputs and externally-sourced
  text (E5.6, `PromptInjectionDetector::screen`). ✅
- Reward-hacking detector flags completions that claim success without
  observable evidence (E5.6, `defence/src/reward_hacking.rs`). ✅
- Goal-drift monitor compares current actions against original objective
  embedding and flags divergence above threshold (E5.6, `defence/src/goal_drift.rs`). ✅
- Defence veto path: vetoed actions return `CortexError::CortexFault` to
  the caller (fail-secure); veto is logged at elevated severity (E5.6). ✅

**Residual risk.** False-negative rate of the heuristic classifier; see T-4
residual risk. The `InjectionClassifier` trait is the hook-point for a future
learned model.

---

### T-6: Data Exfiltration via Tool Calls

**STRIDE category.** Information Disclosure.

**Description.** A compromised or misbehaving cortex uses tool calls (filesystem
read, network, text I/O) to exfiltrate identity memory or conversation content.

**Current mitigations.**
- Tool dispatch is capability-scoped (see T-1 mitigations). ✅
- Every tool call is logged in the durable audit trail. ✅
- Unsafe motor action gate blocks filesystem operations on critical paths
  (`/etc`, `/boot`, the agent's own state directory), network calls to
  blocklisted hosts, and self-modification attempts (E5.6). ✅
- Defence layer veto propagated as `CortexError::CortexFault`; cortex
  cannot silently proceed after a veto. ✅

**Residual risk.** Tool calls within the granted capability scope are still
permitted; the capability scope is intentionally narrow (route-scoped via E5.3).

---

### T-7: Supply Chain Compromise

**STRIDE category.** Tampering.

**Description.** A malicious or compromised dependency is introduced into the
dependency graph via a crates.io release or a compromised registry mirror.

**Current mitigations.**
- `Cargo.lock` pinning. ✅
- `cargo audit` on every PR (EX.4). ✅
- `cargo deny` bans unknown registries and wildcard version specifiers (EX.4). ✅
- All GitHub Actions `uses:` references SHA-pinned; Dependabot rewrites SHAs
  on upstream releases (EX.4). ✅
- SBOM published to GitHub Releases (EX.4). ✅

**Residual risk.** A crate publisher's account being compromised between
`Cargo.lock` updates. `cargo deny` `unknown-registry = "deny"` limits the
blast radius to crates.io.

---

### T-8: Persistent State Tampering

**STRIDE category.** Tampering.

**Description.** A host-privileged attacker modifies the L3 archive, identity
memory, or audit log between agent runs.

**Current mitigations.**
- Atomic writes reduce the window for partial-write corruption. ✅
- Idempotent demotion ensures re-inserted corrupted entries are detected on the
  next replay validation pass (E3.6). ✅
- **HMAC-SHA256 tamper-evidence chain on the audit log** when
  `ANIMA_AUDIT_HMAC_KEY` is set: `mac_i = HMAC-SHA256(key, mac_{i-1} ‖ line_i)`;
  `verify_audit_log` lets operators detect any line insertion, deletion, or
  modification (E6 hardening, PR #63 — `audit.rs`). ✅

**Residual risk.** The L3 archive and identity memory files lack an equivalent
integrity chain. Full mitigation requires Stage 4 microVM attestation.

---

### T-9: Operator Channel Injection / Spoofing

**STRIDE category.** Tampering / Spoofing.

**Description.** An attacker with access to the operator console endpoint (AS-8)
sends crafted guidance — either normal or force-flagged — to inject commands into
the agent's task queue or to bypass the Striatal Gate.

**Current mitigations.**
- Forced guidance transits `GateOverride::OperatorForced` — the gate still
  evaluates the event and records the override; no direct task admission. ✅
- Every forced-guidance event produces a `GateDecision` audit entry with
  `override_active = true`; replay detectable post-hoc. ✅
- Policy bounds enforced even on forced guidance: `max_text_length`, blocked
  prefixes, reason validation (non-empty, non-whitespace, ≤ 512 bytes). ✅
- Bearer-token brute-force rate-limited per source IP (PR #63). ✅
- ANSI/CSI escape sequences stripped before policy evaluation (PR #63). ✅
- Defence layer screens the cortex's output after the gate admits the task,
  providing a second veto opportunity. ✅

**Residual risk.** Without TLS on the console endpoint, an attacker on the same
network segment can eavesdrop on the bearer token and replay it. Production
deployments should use a TLS terminator.

---

## 5. Security Controls Matrix

| Control | Threat(s) | Status | Epic |
|---------|-----------|--------|------|
| `#![forbid(unsafe_code)]` on all non-`corpus` crates | T-2 | ✅ Active | E1.1 |
| `FrameAllocator` audit log tested | T-2 | ✅ Active | E1.2 |
| Capability typestate compile-fail test | T-1 | ✅ Active | E1.2 |
| Cancellation within one token | T-3 | ✅ Active | E1.3 |
| MLFQ token-slice budgets | T-3 | ✅ Active | E1.4 |
| `BoundedTokenPipe` credit backpressure | T-3 | ✅ Active | E1.5 |
| `HumanGuidance` policy bounds | T-3, T-4 | ✅ Active | E3.3 |
| ANSI/CSI escape stripping on input | T-3, T-4, T-9 | ✅ Active | E6 (PR #63) |
| Atomic archive writes | T-8 | ✅ Active | E2.6 |
| Replay validation rollback | T-8 | ✅ Active | E3.6 |
| `cargo audit` in CI | T-7 | ✅ Active | EX.4 |
| `cargo deny` in CI | T-7 | ✅ Active | EX.4 |
| SHA-pinned GitHub Actions | T-7 | ✅ Active | EX.4 |
| Dependabot (4 ecosystems, weekly) | T-7 | ✅ Active | EX.4 |
| SBOM (CycloneDX 1.5) in CI + on Releases | T-7 | ✅ Active | EX.4 |
| Wasmtime gas metering + capability WASI | T-3, T-5 | ✅ Active | E2.5 |
| Cortex capability scoping via Thalamic Router | T-1, T-6 | ✅ Active | E5.3 |
| Prompt-injection classifier (49 patterns) | T-4, T-5 | ✅ Active | E5.6 |
| Goal-drift monitor | T-5 | ✅ Active | E5.6 |
| Reward-hacking detector | T-5 | ✅ Active | E5.6 |
| Unsafe motor action gate | T-1, T-6 | ✅ Active | E5.6 |
| Defence veto audit + escalation | T-4, T-5, T-6 | ✅ Active | E5.6 |
| Kani proofs (15 harnesses) | T-2 | ✅ Active | E4.6 |
| Miri clean on `corpus` suite | T-2 | ✅ Active | E4.6 |
| TLS 1.3 (RFC 8446, RustCrypto) in microVM | T-5, T-6 | ✅ Active | E4.4 |
| Bearer-token auth on console | T-9 | ✅ Active | E6 |
| Per-IP auth rate limiter | T-3, T-9 | ✅ Active | E6 (PR #63) |
| GateOverride audit for forced guidance | T-9 | ✅ Active | E6.6 |
| HMAC-SHA256 tamper-evidence chain on audit log | T-8 | ✅ Active | E6 (PR #63) |
| MicroVM attestation of persistent state | T-8 | ⬜ Future | E4.x follow-on |

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
- **SBOM.** CycloneDX 1.5 JSON SBOMs are generated in CI for every Cargo
  manifest and attached to GitHub Releases via the `release-sbom` workflow. ✅

### 6.2 CI Pipeline Security

- **Minimal permissions.** The `ci.yml` workflow requests `contents: read`
  only; it does not have write access to the repository or package registries.
  The `release-sbom.yml` workflow requests `contents: write` only for the
  duration of the release asset upload.
- **Pinned actions.** All `uses:` references in all workflow files are pinned
  to immutable commit SHAs with trailing `# vX` human-readable comments.
  Dependabot rewrites SHAs on upstream releases. ✅
- **No secret leakage.** No API keys or tokens appear in workflow files;
  live LLM calls are replaced with fixture-based replay in all CI jobs.
- **Dependabot.** Configured for `github-actions` (root), `cargo` (workspace),
  `cargo` (xtask), and `cargo` (microvm) on a weekly cadence, with Cargo
  updates grouped by patch/minor to manage PR volume. ✅

### 6.3 Open Risks

| Risk | Priority | Status |
|------|----------|--------|
| Console endpoint exposed without TLS (loopback only in dev) | Medium | Open — document that production requires TLS terminator |
| L3 archive and identity files lack integrity chain | Medium | Open — full fix requires Stage 4 microVM attestation |
| No Dependency-Track instance for continuous SBOM monitoring | Low | Open — SBOMs now published to releases; Dependency-Track is a future enhancement |
| STT/VAD component resource bounds not yet defined | Low | Open — deferred to VAD/STT implementation |

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

## 8. Per-Stage Security Review Sign-offs

### Stage 1 — Waking Hosted Core ✅

**Reviewer:** EX.4 automated gate (cargo-audit, cargo-deny) + PR review.

- [x] `cargo audit` clean at Stage 1 closure.
- [x] `cargo deny` clean at Stage 1 closure.
- [x] External surfaces documented: AS-1 (human text socket), AS-3 (LLM provider).
- [x] Threats catalogued: T-1 (capability bypass), T-2 (memory corruption),
      T-3 (DoS), T-4 (prompt injection), T-5 (indirect injection), T-7 (supply chain).
- [x] Controls matrix accurate: `#![forbid(unsafe_code)]`, capability typestate,
      cancellation, MLFQ token slices.
- [x] No `unsafe` outside `corpus`; `corpus` unsafe audited via `FrameAllocator` test.
- [x] CI permissions: `contents: read` only.

**Notes.** All Stage 1 epics (E1.1–E1.6) have green CI evidence. Provider
fixture replay eliminates live API calls in CI. The unsafe quarantine enforced
by `#![forbid(unsafe_code)]` was validated by a compile-fail test.

---

### Stage 2 — Somatic Memory and Tool Bus ✅

**Reviewer:** EX.4 automated gate + PR review.

- [x] `cargo audit` clean at Stage 2 closure.
- [x] `cargo deny` clean at Stage 2 closure.
- [x] New surfaces documented: AS-2 (PCM audio), AS-5 (WASI tool modules),
      AS-6 (L3 archive, identity files).
- [x] New threats catalogued: T-6 (data exfiltration), T-8 (persistent state tampering).
- [x] Controls added: Wasmtime gas + capability imports, atomic archive writes,
      ARC cache concurrency (Miri/loom tests), `BoundedTokenPipe` backpressure.
- [x] No new `unsafe` outside `corpus`.
- [x] Wasmtime dependency reviewed for licence compliance (Apache 2.0 / MIT). ✅

**Notes.** Adversarial WASI module tests (infinite loop, memory exhaustion) are
E2.5 exit criteria and are green in CI. `scc` (concurrent hash map) dependency
licence-checked clean.

---

### Stage 3 — Interoception and Autonomic Sleep Cycle ✅

**Reviewer:** EX.4 automated gate + PR review.

- [x] `cargo audit` clean at Stage 3 closure.
- [x] `cargo deny` clean at Stage 3 closure.
- [x] New surfaces: AS-1 update (sensory bridge policy bounds extended via E3.3/E3.4).
- [x] Controls added: `HumanGuidance` policy bounds, sensory priority tagging,
      sleep-phase audit trail.
- [x] Replay validation (E3.6) detects corrupted L3 entries and rolls back;
      this is the primary mitigation for T-8 at this stage.
- [x] No new `unsafe` outside `corpus`.
- [x] 100 consecutive sleep cycles soak test green (E3.4 exit criterion). ✅

**Notes.** Stage 3 closes the somatic loop; the audit log now covers every
sleep-phase transition, enabling post-hoc reconstruction of system state.

---

### Stage 5 — Cognitive Layer ✅

**Reviewer:** EX.4 automated gate + PR review.

- [x] `cargo audit` clean at Stage 5 closure.
- [x] `cargo deny` clean at Stage 5 closure.
- [x] New surfaces documented: AS-4 (Python cortex IPC) updated with defence
      layer wiring; AS-5 (WASI) updated with circuit breaker and routing.
- [x] New threats added: T-9 (operator channel injection) documented in advance
      of E6 implementation.
- [x] Controls added: Thalamic Router capability scoping (E5.3), defence layer
      (E5.6: prompt-injection classifier, goal-drift monitor, reward-hacking
      detector, motor action gate, veto audit and escalation), Striatal Gate
      audit (E5.2), KV-cache controller fault audit (E5.4).
- [x] Python cortex dependency reviewed for supply-chain risk: `langchain`-style
      agent loop is hand-implemented; no external Python package dependencies
      beyond the stdlib and the UDS IPC bridge.
- [x] Cortex crashes isolated from `vita`; fault-injection test green (E5.1
      exit criterion 2). ✅
- [x] Red-team corpus of 15 injection samples: 0 false negatives, 0 false
      positives on clean samples (E5.6 exit criterion 1). ✅

**Notes.** Stage 5 introduces the cognitive layer and its associated trust
boundaries. The defence layer (E5.6) is the primary mitigation for T-4/T-5;
the Thalamic Router (E5.3) enforces capability scoping for T-1/T-6. The
Python subprocess is the highest-risk component in this stage; fault isolation
is tested at the exit criterion level.

---

### Stage 4 — Bare-Metal Isolation and Production Verification ✅

**Reviewer:** EX.4 automated gate + PR review + TLS code review (PR #33).

- [x] `cargo audit` clean at Stage 4 closure (microvm Cargo.lock scanned).
- [x] `cargo deny` clean at Stage 4 closure.
- [x] New surfaces documented: AS-9 (microVM COM1 serial console, E6.4).
- [x] TLS 1.3 implementation (E4.4) reviewed for correctness:
  - RdRand retry loop bounded (10 attempts → panic); infinite loop eliminated. ✅
  - `extract_server_key_share` bounds check added; OOB read on malformed SH eliminated. ✅
  - `hkdf_label` length-truncation guards added. ✅
  - Sequence-number exhaustion check added to `TrafficKeys::seal/open`. ✅
  - Application traffic key derivation corrected to RFC 8446 §7.1. ✅
  - CertificateVerify ECDSA verified on the client side. ✅
- [x] Kani proofs (E4.6): 15 harnesses across `corpus`, `scheduler`; all green
      in nightly CI. ✅
- [x] Miri clean on `corpus` suite in nightly CI (E4.6). ✅
- [x] Image-size budget enforced in CI: release EFI ≤ 1 MiB, debug EFI ≤ 6 MiB. ✅
- [x] No new `unsafe` outside `corpus` and `kernels/microvm`; `microvm` unsafe
      limited to inline assembly (RDRAND CPUID guard) with `nostack` annotation
      and rbx-preservation correctness review. ✅

**Notes.** The inline assembly for `RdRand::is_available()` received a HIGH-priority
security review in PR #33 (red-zone corruption risk resolved by using a
compiler-allocated temporary register with `nostack`). The TLS key schedule
correctness fix (RFC 8446 §7.1 `MasterSecret` derivation) was a CORRECTNESS-level
finding that would have produced incorrect key material under the previous
implementation.

---

### Stage 6 — Operator Interface 🟡

**Reviewer:** EX.4 automated gate + PR review.

- [x] `cargo audit` clean at Stage 6 partial closure.
- [x] `cargo deny` clean at Stage 6 partial closure.
- [x] New surfaces documented: AS-8 (operator console HTTP/SSE), AS-9 (microVM serial).
- [x] New threats added: T-9 (operator channel injection / spoofing).
- [x] Controls added: bearer-token auth, per-IP rate limiting, ANSI escape
      stripping, forced-guidance policy bounds, `GateDecision` audit entries,
      HMAC-SHA256 tamper-evidence chain on audit log.
- [x] Console endpoint binds to operator-configurable address; documentation
      notes that non-loopback addresses require a TLS terminator in production.
- [x] S6.5 (microVM Phase-1 TCP/TLS transport) deferred — virtio-net driver not
      yet available; threat analysis for Phase-1 transport deferred accordingly.

**Notes.** Stage 6 is partially open (🟡) due to S6.5 deferral. All exit
criteria except S6.5 are met. Security review is complete for the delivered
scope.

---

## 9. References

- [RustSec Advisory Database](https://rustsec.org/)
- [cargo-audit](https://github.com/rustsec/rustsec/tree/main/cargo-audit)
- [cargo-deny](https://embarkstudios.github.io/cargo-deny/)
- [STRIDE Threat Model](https://learn.microsoft.com/en-us/azure/security/develop/threat-modeling-tool-threats)
- [RFC 8446 — TLS 1.3](https://www.rfc-editor.org/rfc/rfc8446)
- [CycloneDX Specification](https://cyclonedx.org/specification/overview/)
- `docs/01-architecture.md` — AnimaOS architecture and trust zones
- `docs/02-subsystems.md` — subsystem responsibilities
- `docs/08-cognitive-architecture.md` — cognitive layer design
- `docs/11-operator-interface.md` — operator interface design and security rationale
- `deny.toml` — machine-readable supply-chain policy (this repo root)
