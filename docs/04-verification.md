# 04 — Verification

Anima's verification strategy is built around a single principle: the audit surface for memory safety is small enough to verify thoroughly, and everything outside that surface is checked by lighter-weight means.

This document specifies the testing taxonomy, the formal verification tools, and the continuous integration setup.

## 1. The Verification Taxonomy

Anima distinguishes four levels of verification, applied at different points in the stack.

| Level | Tool | Scope | When |
|-------|------|-------|------|
| 1 — Type & lint | `rustc`, `clippy`, attribute checks | All crates | Every compile |
| 2 — Unit & integration | `cargo test` | All crates | Every PR |
| 3 — Formal | `kani`, `miri` | `corpus`, critical concurrent structures | Nightly + pre-release |
| 4 — Behavioural | Property tests, fuzzing, end-to-end harness | Subsystem integration points | Nightly + pre-release |

The pyramid is intentionally bottom-heavy: most of the codebase is checked by Level 1 and Level 2, while Levels 3 and 4 concentrate effort where bugs would be catastrophic.

## 2. Level 1: Type and Lint

### 2.1 `unsafe` Quarantine Check

Every crate except `corpus` declares at the crate root:

```rust
#![forbid(unsafe_code)]
```

This is checked by the compiler. A PR introducing `unsafe` to `vita`, `scheduler`, `memory`, `praxis`, `self`, `interoception`, or `senses` fails to build. There is no flag to override this.

The `corpus` crate may use `unsafe`, but every `unsafe` block requires:

1. A `SAFETY:` doc comment explaining the invariant being upheld.
2. Inclusion in the [`crates/corpus/unsafe_audit.md`](../crates/corpus/unsafe_audit.md) file with a brief justification.
3. Sign-off from a second reviewer at PR time.

### 2.2 Clippy Configuration

CI runs `cargo clippy --workspace --all-targets -- -D warnings`: the default
clippy lint set with all warnings treated as errors. Adopting
`clippy::pedantic` workspace-wide remains an open item — it is deliberately
not enabled until the resulting lint debt across the 35-crate workspace can
be paid down in a dedicated change rather than suppressed wholesale.

### 2.3 Format & Lint in CI

Both `cargo fmt --check` and `cargo clippy -- -D warnings` run on every PR. PRs that fail either check are blocked from merge.

## 3. Level 2: Unit and Integration Tests

### 3.1 Per-Crate Unit Tests

Each crate maintains unit tests for its public surface. Coverage targets:

- **`corpus`**: 95%+ line coverage, with all error paths exercised.
- **`vita`, `scheduler`, `memory`**: 90%+ line coverage.
- **`praxis`, `self`, `interoception`, `senses`**: 85%+ line coverage.

Coverage is measured by `cargo llvm-cov` in the nightly CI pipeline
(`nightly.yml` `coverage` job): the workspace lcov report is uploaded as an
artifact and a warning annotation is emitted if total line coverage falls
below the 80% advisory floor. Coverage never blocks a merge — the intent is
to surface regressions, not to enforce a quota by means that encourage
gaming. The per-crate targets above are review guidance, not gates.

### 3.2 Integration Tests

Cross-crate integration tests live inside the crates that own the seam —
inline `#[cfg(test)]` modules in `vita`, `kernels/hosted`, and the subsystem
crates — rather than in a workspace-root `tests/` directory, so each test
compiles against exactly the features its seam exports. They cover:

- **Memory tier transitions.** Items flow correctly through L1 → L2 → L3 and back under realistic load.
- **Lifecycle state transitions.** The state machine in `vita` reaches every state and every transition is exercised.
- **Capability enforcement.** Tasks running under restricted capabilities cannot escalate without an explicit grant.
- **Praxis circuit breakers.** Failing tools trip breakers correctly and recovery follows the expected timeline.

Integration tests run against the hosted kernel target. The microVM target is exercised separately (§5).

## 4. Level 3: Formal Verification

### 4.1 Kani: Bounded Model Checking

`kani` is used for bounded model checking of structures where correctness is hard to establish by testing alone. Initial targets:

- **The MLFQ scheduler queue invariants.** Items at higher priority levels are always selected over lower-priority items. Boost events fire on the configured schedule. No priority inversion.
- **The rate limiter.** Tokens are issued no faster than configured; bursts are bounded by bucket size; clock skew does not produce double-issuance.
- **The lock-free ring buffer.** No data races. No lost messages. No spurious wake-ups on empty queues.

Kani proofs are written alongside the code they verify:

```rust
#[cfg(kani)]
#[kani::proof]
fn rate_limiter_never_overissues() {
    let mut limiter = RateLimiter::new(rate: 100, burst: 10);
    let elapsed_ms: u32 = kani::any();
    kani::assume(elapsed_ms < 60_000);

    let issued = limiter.advance_and_drain(elapsed_ms);

    let max_allowed = 10 + (100 * elapsed_ms / 1000);
    assert!(issued <= max_allowed);
}
```

Kani runs on the nightly CI pipeline. A failed proof blocks the release.

### 4.2 Miri: Undefined Behaviour Detection

`miri` is used to detect undefined behaviour in `unsafe` code, particularly in `corpus`'s memory management routines. All unit tests in `corpus` and all integration tests that exercise allocator paths are run under `miri` in the nightly pipeline.

Miri is significantly slower than native execution (50–100× typical), so it is restricted to:

- The `corpus` test suite.
- Integration tests tagged `#[miri_eligible]`.
- A small set of stress tests for the L2 concurrent hashmap.

A miri failure blocks the release.

### 4.3 What's Not Formally Verified

We do not attempt to formally verify the whole system. Specifically out of scope:

- The model's behaviour itself. Anima provides a substrate; the agent's reasoning is the model's responsibility.
- The semantic correctness of the dreaming subsystem. Random graph walks are inherently stochastic; correctness here means "the walk respects the graph structure," not "the discovered associations are good."
- Tool implementations downloaded at runtime. These run in wasmtime sandboxes with bounded resources; we verify the sandbox, not its contents.

## 5. Level 4: Behavioural Testing

> **Status.** Level 4 is the least-built layer of the taxonomy. §5.1 and
> §5.2 are design intent that has **not yet been implemented** — there are
> no proptest or cargo-fuzz targets in the workspace today. They are kept
> here as the agreed scope for that work. §5.3 exists in the form described
> in its status note.

### 5.1 Property Testing (open — not yet implemented)

`proptest` is to be used at key subsystem boundaries to find inputs that break invariants. Initial targets:

- **Memory pruning preserves the semantic floor.** No matter what activation values, arousal levels, or surprise scores are generated, no entry above the floor is evicted.
- **Stress index stays in $[0, 1]$.** Across all possible (latency, token count) inputs.
- **Length-robust tool filter is monotonic in score.** A tool with a higher score than another tool in the same query is never filtered out while the other is admitted.

The named invariants are currently exercised by example-based unit tests in
`memory`, `interoception`, and `praxis`; the property-testing generalisation
remains open.

### 5.2 Fuzzing (open — not yet implemented)

`cargo-fuzz` targets are to be maintained for parsing surfaces:

- The `senses` text parser.
- The `senses` voice frame parser.
- The `praxis` MCP message decoder.
- The `praxis` A2A message decoder.
- The `console-proto` NDJSON frame parser.

### 5.3 End-to-End Harness

> **Status.** Implemented as inline integration tests rather than a separate
> `tests/harness/` tree: the somatic-loop and sleep-cycle scenarios live in
> `vita`'s and `kernels/hosted`'s test modules, and the containerised
> round-trip (console up → guidance in → `AgentMessage` out over SSE against
> the mock backend) is asserted by the Docker workflow's smoke-test step on
> every image build.

The harness boots the full system in the hosted target, drives synthetic human input, and asserts on observable outcomes. Scenarios include:

- **Baseline conversation.** Send N turns, verify responses, verify L1/L2/L3 state evolution.
- **Sleep cycle.** Force the stress index low, verify transition to sleep, verify each phase runs, verify wake on input.
- **Emergency consolidation.** Force the stress index to 0.95, verify emergency consolidation fires and recovers.
- **Capability denial.** Attempt to invoke a tool whose capability has been revoked; verify rejection and audit-log entry.
- **Circuit breaker.** Make a tool fail repeatedly; verify breaker opens; verify breaker recovers after timeout.

The harness runs on every PR. New scenarios are added with each significant feature.

## 6. MicroVM Target Verification

The microVM target is verified separately from the hosted target because it exercises code paths (`smoltcp`, bare-metal allocator, UEFI boot) that the hosted target doesn't touch.

### 6.1 Boot Smoke Test

A microVM image is built on every PR (with the ≤ 1 MiB release-EFI budget
enforced) and booted under QEMU/OVMF in the `microvm-boot` CI job. The boot
smoke test asserts the full marker sequence on COM1 serial: `E4.1_*` (boot +
panic handler), `E4.2_TASK_DONE` (Embassy executor), `E4.3_TCP_DONE`
(smoltcp), `E4.4_TLS_DONE` (TLS 1.3), `E4.5_SOAK_DONE` (sleep-cycle soak),
and `E6.4_CONSOLE_DONE` (operator-console Phase 0). Boot latency is recorded
for information only — the 2 s budget applies to Firecracker / Cloud
Hypervisor and is asserted on real hardware as part of the soak run
(`docs/22` §1). Firecracker-in-CI remains open: GitHub-hosted runners do not
expose KVM reliably.

### 6.2 Long-Running Soak Test

The soak harness is `cargo xtask soak`: it drives a QEMU boot loop, records
per-iteration boot latency and outcome (`ok` / `timeout` /
`unscheduled_exit`), and writes a resumable JSON manifest plus a JSONL log.
`.github/workflows/soak.yml` runs a short live soak plus a dry-run
schema self-test on manual dispatch. The 30-day production run
(720 hours, on Firecracker / Cloud Hypervisor hardware) has **not yet been
executed**; its manifest is to be committed under `artifacts/soak/` when it
completes (`docs/22` §1). A recurring 6-hour nightly soak with
synthetic-load assertions (memory envelope, consolidation behaviour, L3
growth bounds) remains open design intent.

## 7. CI Pipeline Overview

```
PR opened (ci.yml, bench.yml, docker.yml on relevant paths)
   │
   ├─► Level 1: fmt, clippy -D warnings, unsafe quarantine (compiler-enforced)
   │   (must pass to proceed)
   │
   ├─► Level 2: cargo build + test (hosted target, all crates)
   │
   ├─► Supply chain: RustSec audit + cargo-deny (licences, bans, sources)
   │
   ├─► MicroVM: UEFI build (≤ 1 MiB release-EFI budget) +
   │   QEMU/OVMF boot with serial-marker assertions (E4.2–E4.5, E6.4)
   │
   ├─► Benchmarks: criterion vs bench/baselines/ (warning-only on PRs)
   │
   └─► Docker: hosted image build + mock-backend console smoke test
       (on Dockerfile / manifest changes)

Nightly (nightly.yml; bench gate hard on schedule)
   │
   ├─► Level 3: kani proofs (15), miri on corpus
   │
   ├─► Level 2½: cargo-llvm-cov workspace coverage (advisory floor 80%,
   │   lcov artifact uploaded)
   │
   └─► Benchmarks: regression gate is hard (continue-on-error: false)

Manual / hardware-gated
   │
   ├─► soak.yml: short live soak + manifest schema self-test (dispatch)
   │
   ├─► 30-day production soak on Firecracker / Cloud Hypervisor
   │   (operator-driven; manifest committed to artifacts/soak/)
   │
   └─► Release: audit of crates/corpus/unsafe_audit.md changes +
       benchmark comparison against the last release
```

Level 4 (property tests, fuzzing) is not yet wired into any pipeline — see
the status note at the top of §5.

## 8. What "Verified" Means in Anima

Anima does not claim to be a formally-verified system in the sense of seL4 or CompCert. The agent's behaviour is the model's responsibility and is fundamentally not amenable to formal proof.

What Anima does claim:

1. The Trusted Computing Base (`corpus`) is small enough to audit by hand and is verified against undefined behaviour by miri.
2. The concurrent data structures and rate limiters in the critical path are proven correct by kani against the invariants that matter.
3. The crate-level `unsafe` quarantine is enforced by the compiler, not by convention.
4. The behavioural test surface covers all major subsystem interactions and is exercised continuously.

These are the claims the verification strategy is designed to support. Anything stronger would be marketing.
