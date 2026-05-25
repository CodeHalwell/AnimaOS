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
2. Inclusion in the `corpus/unsafe_audit.md` file with a brief justification.
3. Sign-off from a second reviewer at PR time.

### 2.2 Clippy Configuration

The workspace `.clippy.toml` enables the full set of warnings under `clippy::pedantic`, with a small number of explicit exceptions documented in the file. The CI build treats all clippy warnings as errors.

### 2.3 Format & Lint in CI

Both `cargo fmt --check` and `cargo clippy -- -D warnings` run on every PR. PRs that fail either check are blocked from merge.

## 3. Level 2: Unit and Integration Tests

### 3.1 Per-Crate Unit Tests

Each crate maintains unit tests for its public surface. Coverage targets:

- **`corpus`**: 95%+ line coverage, with all error paths exercised.
- **`vita`, `scheduler`, `memory`**: 90%+ line coverage.
- **`praxis`, `self`, `interoception`, `senses`**: 85%+ line coverage.

Coverage is measured by `cargo llvm-cov` and reported in CI. Falling below the target on a PR produces a warning but does not block merge — the intent is to surface regressions, not to enforce a quota by means that encourage gaming.

### 3.2 Integration Tests

Cross-crate integration tests live in `tests/` at the workspace root. They cover:

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

### 5.1 Property Testing

`proptest` is used at key subsystem boundaries to find inputs that break invariants. Initial targets:

- **Memory pruning preserves the semantic floor.** No matter what activation values, arousal levels, or surprise scores are generated, no entry above the floor is evicted.
- **Stress index stays in $[0, 1]$.** Across all possible (latency, token count) inputs.
- **Length-robust tool filter is monotonic in score.** A tool with a higher score than another tool in the same query is never filtered out while the other is admitted.

Property tests run on every PR with a small default iteration count (256) and on nightly with a larger count (10,000).

### 5.2 Fuzzing

`cargo-fuzz` targets are maintained for parsing surfaces:

- The `senses` text parser.
- The `senses` voice frame parser.
- The `praxis` MCP message decoder.
- The `praxis` A2A message decoder.
- The `postcard` and `rkyv` deserialisers for inter-crate messages.

Fuzz targets run continuously in a dedicated CI job and report crashes to the issue tracker automatically.

### 5.3 End-to-End Harness

A test harness in `tests/harness/` boots the full system in the hosted target, drives synthetic human input, and asserts on observable outcomes. Scenarios include:

- **Baseline conversation.** Send N turns, verify responses, verify L1/L2/L3 state evolution.
- **Sleep cycle.** Force the stress index low, verify transition to sleep, verify each phase runs, verify wake on input.
- **Emergency consolidation.** Force the stress index to 0.95, verify emergency consolidation fires and recovers.
- **Capability denial.** Attempt to invoke a tool whose capability has been revoked; verify rejection and audit-log entry.
- **Circuit breaker.** Make a tool fail repeatedly; verify breaker opens; verify breaker recovers after timeout.

The harness runs on every PR. New scenarios are added with each significant feature.

## 6. MicroVM Target Verification

The microVM target is verified separately from the hosted target because it exercises code paths (`smoltcp`, bare-metal allocator, UEFI boot) that the hosted target doesn't touch.

### 6.1 Boot Smoke Test

A microVM image is built on every PR. The image is booted under Firecracker in a CI runner. The boot smoke test asserts:

- The image boots to the agent's main loop within 5 seconds.
- The agent emits a "ready" event on its telemetry stream.
- A single test request is handled correctly.
- A graceful shutdown completes within 2 seconds.

### 6.2 Long-Running Soak Test

Nightly, a microVM image is booted and driven with synthetic load for 6 hours. The soak test asserts:

- No memory growth beyond the initial steady-state envelope.
- No descent into emergency consolidation under sustained nominal load.
- Sleep cycles trigger appropriately and complete cleanly.
- L3 grows by a bounded amount per hour (catches dream-walk runaway).

## 7. CI Pipeline Overview

```
PR opened
   │
   ├─► Level 1: fmt, clippy, unsafe quarantine
   │   (must pass to proceed)
   │
   ├─► Level 2: cargo test (hosted target, all crates)
   │   coverage report
   │
   ├─► Level 4 (sampled): proptest, e2e harness scenarios
   │
   └─► MicroVM boot smoke test

Nightly (default branch)
   │
   ├─► Everything above
   │
   ├─► Level 3: kani proofs, miri on corpus
   │
   ├─► Level 4 (full): proptest 10k iters, full fuzz run
   │
   └─► MicroVM soak test (6h)

Release candidate
   │
   ├─► Everything above
   │
   ├─► Manual audit of unsafe_audit.md changes since last release
   │
   └─► Performance regression benchmark against last release
```

## 8. What "Verified" Means in Anima

Anima does not claim to be a formally-verified system in the sense of seL4 or CompCert. The agent's behaviour is the model's responsibility and is fundamentally not amenable to formal proof.

What Anima does claim:

1. The Trusted Computing Base (`corpus`) is small enough to audit by hand and is verified against undefined behaviour by miri.
2. The concurrent data structures and rate limiters in the critical path are proven correct by kani against the invariants that matter.
3. The crate-level `unsafe` quarantine is enforced by the compiler, not by convention.
4. The behavioural test surface covers all major subsystem interactions and is exercised continuously.

These are the claims the verification strategy is designed to support. Anything stronger would be marketing.
