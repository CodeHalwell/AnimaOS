# AnimaOS Codebase Review

**Date:** 2026-07-01
**Scope:** Full tree — 35 library crates, both kernel targets (`hosted`, `microvm`),
`llm-backends/`, `cortex/` (Python), `xtask/`, `trainer/`, CI, and build/dep hygiene.
**Method:** File-by-file review. Every finding was verified against the actual source
and (where relevant) its callers/tests. The highest-impact findings were independently
re-verified; those are marked ✔ below.

Findings are grouped by systemic theme (the useful lens for prioritization), with
per-area finding tables and test-coverage summaries following. File:line references are
current as of the date above.

> **A high-confidence subset has since been fixed on this branch** (see next section).
> The remaining findings stay as a triage backlog.

---

## Fixes applied on this branch

The following were implemented and verified (`cargo test --workspace --all-targets` and
`cargo clippy --workspace --all-targets -- -D warnings` clean; microVM UEFI target and the
`xtask` workspace both build; new regression tests added for each behavioural change):

| Finding | Fix |
|---|---|
| **AUT-1 / AUT-3** | Constitution now matches keywords on **whole-word boundaries** (`skill` no longer trips `kill`); operator bounds require all meaningful words. |
| **MEM-1** | Motor-gate filesystem screen **lexically resolves `.`/`..`** and matches critical prefixes on component boundaries (`/var/../etc/passwd` and `./etc/passwd` are caught; `/etcd` is not). |
| **MEM-2** | Injection detector **normalizes** text (lowercase, strip zero-width, collapse whitespace) before matching — defeats space/tab/newline/`U+200B` evasions. |
| **MEM-5** | Reward-hack detector uses leading word-boundary matching — `"incomplete."` / `"unfinished."` no longer read as completion claims. |
| **MEM-6** | Motor-gate network screen **parses the host**, blocks loopback/RFC1918/link-local/metadata by default, and matches the blocklist by host+subdomain (no more `10.0.0.1`⊃`10.0.0.10`). |
| **MEM-4** | WASM sandbox **caps table growth** (`table_growing` enforces a limit) — closes the unbounded-host-allocation gap. |
| **MEM-12** | `SandboxedMathEvaluator` emits `null` for non-finite results instead of invalid JSON (`{"result":inf}`). |
| **VITA-1** | Interoceptive snapshot is **rate-limited to ~1 Hz** (was ~1 kHz) — stops the audit-ring/disk flood that erased forensics. |
| **VITA-2 / CORE-6 / OPS-15 / KERN-14** | Poison-tolerant locking (`lock_recover` shim, cfg-split for std/`no_std`) on the somatic loop, `senses`, `tool-cache`, and `console` — one panic no longer permanently bricks PID 1. |
| **CORE-3** | Scheduler dispatch history is a `VecDeque` (O(1) `pop_front` eviction instead of O(n) front-drain). |
| **CORE-5** | Financial-budget pruning keys off the newest observed day, so an out-of-order older timestamp can't wipe today's spend and reset the budget. |
| **AUT-4** | Skill `linked_files` extraction rejects `..`, absolute, and Windows-drive paths (closes the latent arbitrary-file-read). |
| **AUT-7** | Motivation priority/eviction comparisons use `f32::total_cmp` (no `partial_cmp().unwrap()` panic on a stray NaN). |
| **AUT-8** | `PriorityLattice` clamps the suppression denominator (no divide-by-zero → inf/NaN in drive weights). |
| **OPS-1** | Webhook registry **validates URLs on register** — rejects non-`http(s)` schemes and loopback/private/link-local/metadata hosts (SSRF gate). |
| **INF-2** | `concurrency: cancel-in-progress` added to `ci`/`bench`/`docker` workflows. |
| **INF-7** | `cortex/transformers_worker.py` enforces the 64 MiB frame cap that `ipc.py` already had. |
| **INF-8** | `deny.toml` now sets `yanked = "deny"`. |
| **INF-16** | `trainer/sleep_phase.py` guards `__doc__` so it doesn't crash under `python -OO`. |

### Second wave (previously deferred, now done)

A follow-up pass implemented the rest of the substantive backlog:

| Finding | Fix |
|---|---|
| **AUT-2** | Constant-time HMAC compare, `CharterError::Unsealed` + `Charter::from_path_strict`, loud warning on unsealed file loads, and `ConstitutionGuard::is_sealed()` so a supervisor can refuse an unsealed charter. |
| **VITA-5 / MEM-15** | Audit ring uses amortized-O(1) batch eviction (slice API kept); kv-trace uses a `VecDeque`. |
| **KERN-2 / KERN-3** | Somatic loop runs under a `catch_unwind` + backoff **supervisor**; `LifecycleManager` gains a cooperative shutdown flag polled each iteration, with a `signal-hook` SIGTERM/SIGINT handler in the hosted kernel. |
| **VITA-3** | Watchdog / prospective memory / confidence tracker are **wired** into the somatic loop (enabled by default in `serve`, `ANIMA_COGNITION=0` to opt out). |
| **IO-2 / IO-4** | Shared jittered-backoff retry helper on the live provider paths; compat-live emits word-level token chunks so accounting isn't undercounted to 1. |
| **IO-1** | Real Anthropic Messages client + OpenAI (via the compat client); factory routes to live when the API key is set, fixtures + a loud warning otherwise. |
| **OPS-9** | `metrics-endpoint` merged into `metrics` (one Prometheus schema; CLI dump + `/metrics` share it). |
| **OPS-6 / OPS-13** | New `jsonstore` crate (`state_dir()` safe fallback + `atomic_write()`) adopted across 7 stores; `jobs::record_run_result` now persists immediately. |
| **VITA-7** | The two ~90-line sleep-maintenance methods share one `run_maintenance_and_postprocess()`. |

### Third wave — the two large structural refactors (now done)

The two high-churn, maintainability-only refactors that were previously carried as
separate follow-ups are also landed. Both are behaviour-preserving (verified against
the full 2100+-test workspace suite, clippy `-D warnings`, and both the hosted and
microVM targets):

| Finding | Fix |
|---|---|
| **KERN-9** | `kernels/hosted/src/main.rs` split 7,519 → 476 lines: the 7 `print_*_audit` fns moved to an `audit_view` module, the 24 `cmd_*` handlers to a `commands` module (reached via `use super::*`, so a pure relocation), and the 25-branch `if`-ladder dispatch collapsed into one `match`. `main.rs` is now a thin entry point. |
| **VITA-6** | `LifecycleManager`'s field list regrouped into `SleepConfig` (7 sleep-phase knobs) and `Subsystems` (8 optional `Option<_>` capabilities), dropping the top-level struct from ~30 to 20 fields. External coupling was two write sites — the god-object was already well encapsulated behind its `enable_*`/`with_*` builders. |

### Fourth wave — PR review response (now done)

Automated PR review (Copilot / Codex / Gemini) surfaced a further batch, addressed here:

| Finding | Fix |
|---|---|
| **SSRF host canonicalization** | `defence` motor gate + `webhooks` registry now normalise the host (trailing DNS dot, IPv4-mapped IPv6, `inet_aton` integer/octal/hex/short forms, `0.0.0.0`) and use `std::net` range checks before the loopback/private/metadata veto; blocklist entries carrying a scheme or port are normalised too. |
| **CORE-5 (future timestamps)** | The financial-budget ledger is bounded by record count instead of a fragile max-day anchor, so a future-dated (clock-skew / replay) spend record can no longer prune away the real day's spend and reset the budget toward 1.0. |
| **Windows home dir** | `jsonstore::state_dir` restores the `USERPROFILE` fallback dropped during store unification. |
| **RUSTSEC-2026-0190** | `anyhow` bumped 1.0.102 → 1.0.103 to clear the `cargo-deny` advisories gate. |

A Gemini note about `kv-controller`'s `VecDeque` import breaking `no_std` is a
false positive: the `trace` module carrying that import is `#[cfg(feature = "std")]`,
so it is never in the `no_std` build (the microVM CI build confirms it).

**Known follow-ups** (deferred, not blocking):

- **E14 intention completion** — `inject_due_intentions` marks due intentions
  `dispatched` but nothing calls `IntentionStore::complete` after the injected
  task runs, so one-shot intentions re-fire after a restart and recurring ones
  never advance their due time. The correct fix (complete after successful
  execution) needs a task-completion hook threaded through the somatic loop;
  it's an opt-in E14 feature, so it's tracked here rather than patched with a
  semantics-changing complete-at-injection shortcut. (PR review, P2.)
- **SSRF-helper dedup** — the host-canonicalization helpers are duplicated in
  `defence` and `webhooks`; a shared home (review theme E) would remove the
  drift risk.

---

## Overall health: strong

AnimaOS is well-engineered, not a rough draft:

- `cargo build/test --workspace --all-targets` is **clean with zero warnings** under
  `RUSTFLAGS=-D warnings`; clippy is clean too.
- **Unsafe quarantine is intact** — only `corpus` (the TCB) carries `unsafe`, and it is
  minimal, sound, and accurately audited (`corpus/unsafe_audit.md`); frame/heap allocators
  and MLFQ are backed by Kani proofs + Miri.
- **Test density is high** (~1,500 `#[test]`), with genuinely strong suites in
  `console-proto` (escape-torture), `console` server (auth/lockout/bind-policy),
  `actuators` egress (SSRF matrix), `finetune` (two-stage adoption gate), and `cortex`
  (`ipc.py` framing).
- **Security seams are real**: constant-time token compares, per-IP auth lockout, a 16 MiB
  UDS frame cap enforced *before* allocation, redirect-disabled SSRF-safe egress, secrets
  kept out of logs.
- TODO/FIXME debt is negligible (6 markers in ~97k lines of Rust).

The issues below are **recurring patterns**, not scattered one-offs.

---

## Systemic themes (most impactful first)

### A. "Documented-but-not-real" drift — HIGHEST LEVERAGE

The system reports safety guarantees and capabilities it does not actually have. For a
safety-focused autonomous-agent OS this is worse than a missing feature, because operators
and the agent itself trust the claim. This is the top-priority class to resolve.

| Claim | Reality | Location |
|---|---|---|
| `anima-self` capability compile-fail test "✅ Active" — the **T-1 privilege-escalation mitigation** | No such test exists anywhere in the workspace; the barrier holds only by an undocumented private-field (`_state: PhantomData`) accident | `docs/09-threat-model.md:285,486`; `crates/self` (2 tests total) |
| Constitution `hmac_verified` tamper-evidence | Computed then **never read by any consumer**; empty HMAC silently accepted (TOFU) → anyone who can edit `constitution.toml` can weaken prohibitions and blank the MAC undetected | `constitution/src/charter.rs:224`; `defence/src/constitution.rs:25` |
| E14 cognitive suite "called by the lifecycle manager" | ~1,459 lines (watchdog/prospective/metacognition) built, tested, exported, **zero production callers**; the stuck-loop / hallucination-spiral watchdog and prospective reminders never run | `vita/src/{watchdog,prospective,metacognition}.rs`; docs at `prospective.rs:11`, `watchdog.rs:91` |
| microVM prints exit-criteria status | Panic handler **discards `info`** and unconditionally prints `E4.1 ✅ … E4.5 ✅ — kernel boot task complete` on *any* crash — the real panic message/location is thrown away | `kernels/microvm/src/main.rs:227` ✔ |
| Anthropic/OpenAI backends usable when API key set | Both are **fixture-only stubs** (no HTTP client); `default_frontier_kind()` routes to them when `ANTHROPIC_API_KEY` is set → silent `[anthropic-fixture-not-found]` sentinel for every real prompt | `llm-backends/src/anthropic.rs:52`, `openai.rs:52`, `factory.rs:287` ✔ |
| `webhook test` delivery | Delivery is **simulated** (`FixtureSender` always `Accepted`); the CLI prints "sending…" and reports success without any I/O. Webhooks never fire anywhere in the product | `webhooks/src/dispatcher.rs:113`; `kernels/hosted/src/main.rs:4291` |
| Injection red-team corpus + "published FP/TP rates" (E5.6 exit criterion) | Referenced fixture/`red_team_corpus` feature **don't exist**; only 15 inline samples, detector never evaluated against evasion | `defence/src/injection.rs:338` |
| microVM `acpi.rs` "validated against synthetic ACPI tables on the host" | No such tests exist; microVM crate has **zero tests** | `kernels/microvm/src/acpi.rs:23` |
| `corpus` unsafe audit: heap allocator alignment "verified by Kani" | The two allocators don't share the alignment math; `align_up`/aligned-alloc have **no** Kani proof (unit+Miri only) | `corpus/unsafe_audit.md:22` vs `heap_allocator.rs:54` |

**Recommended cross-cutting fix:** for each row, either wire the feature or correct the
claim — and add a CI check that greps docs for asserted test/proof names and fails if they
don't exist. That makes this whole class self-preventing.

### B. Security heuristics that are trivially evadable

The `defence` and `constitution` crates are the safety boundary but rely on naive
`str::contains` substring matching. Verified bypasses:

- **`"kill"` ⊂ `"skill"`** ✔ — `constitution.toml:37` lists bare keyword `"kill"`;
  `check.rs:176` does `text.contains("kill")`. Every proposal mentioning a *skill* is
  vetoed as P1 irreversible-harm, **breaking the agent's own self-extension flow**. Live
  bug, not hypothetical.
- **Motor-gate path traversal** ✔ — `motor_gate.rs:129` does `path.starts_with("/etc")`
  on the *raw* path; `write("/var/../etc/passwd")` or `"./etc/passwd"` skips the capability
  check entirely, and `/etcd`/`/bing` over-block. This is billed as the "hard safety boundary."
- **Injection evasion** — double-space, tab, newline, zero-width (`​`), and full-width
  homoglyphs defeat every multi-word rule (`injection.rs:127`); confirmed by execution.
- **SSRF, network + webhooks** — cloud metadata (`169.254.169.254`), loopback, RFC1918 all
  allowed by default; host matching is substring-based (`10.0.0.1` blocks `10.0.0.10`;
  `evil.com` matches `notevil.com`); webhooks do **no** URL validation at all
  (`webhooks/src/registry.rs:85` ✔, `motor_gate.rs:160`).
- **Reward-hack false positives** — `"incomplete."` contains `"complete."`; statements
  saying work is *not* done trip the detector (`reward_hacking.rs:21`).
- **Skill `linked_files` accept `../`/absolute** — latent arbitrary-file-read once a caller
  joins the path; agent skills auto-promote by default (`skills/src/manifest.rs:163`,
  `proposal.rs:183`).

**Recommended fix:** one shared text-normalization + word-boundary/token matcher (NFKC-fold,
strip zero-width, collapse whitespace runs) for the detectors; a real URL parser + private-
range blocklist for the network/webhook gates; reject `..`/absolute in skill link extraction.

### C. Unbounded growth & O(n)/O(n²) hot paths (this is an always-on OS)

- **vita `InteroceptiveSnapshot` fires ~1 kHz, not the documented 1 Hz**
  (`vita/src/lib.rs:1179`; wired live in `serve` via `main.rs:6741`). The 10k audit ring
  **cycles every ~10 s**, evicting every gate/task/sleep decision (destroys `anima why`
  forensics), and with `ANIMA_AUDIT_DIR` set does ~1,000 disk flushes/s forever.
- **Ring buffers via `Vec::drain(0..1)` / `Vec::remove(0)`** — O(n) memmove per push at
  steady state: `scheduler/mlfq.rs:241`, `vita/audit.rs:2086`, `kv-controller/trace.rs:233`.
- **Full-file rewrite per mutation → O(n²)**: `sessions/store.rs:230` (per turn),
  `memory/archival.rs:327` (L3, per demotion, JSON), `feedback/store.rs:90`,
  `knowledge-graph/graph.rs:152`.
- **Maps/vecs that only grow**: `quota` per-user tracker (`lib.rs:361`),
  `vita::recent_episode_summaries` (also re-analyzed in full each sleep, `lib.rs:534`),
  `lifecycle` approval queue (`approval.rs:180`), `skills` registry (`registry.rs:81`),
  `senses` queue (unbounded → DoS, `senses/src/lib.rs:163`).

**Recommended fix:** `VecDeque` ring buffers; append-only or debounced/batched persistence;
retention/eviction sweeps + hard caps on the long-lived maps; queue-depth cap in `senses`.

### D. PID-1 availability hazards (a self-preserving init must not die)

- **Poison-fatal locks on the somatic hot loop** — `vita/src/lib.rs:640,1116,1210` use
  `.lock().expect("poisoned")`; one panic while a lock is held → every subsequent iteration
  panics → permanent PID-1 death. The crate **already** uses poison-tolerant
  `unwrap_or_else(|e| e.into_inner())` in `dispatch.rs:90`/`agent_pool.rs:279` — just
  inconsistent. Same pattern in `senses` (16 sites), `console/server.rs:114`,
  `tool-cache/lib.rs:320`.
- **No supervision in hosted `serve`** — `kernels/hosted/src/main.rs:6819` runs the loop
  under `.expect(...)` with no `catch_unwind`/restart/backoff; one dispatch error terminates
  the agent.
- **No signal handling / graceful shutdown** — only a SIGPIPE panic-hook; SIGTERM hard-kills
  mid-task with no corpus flush or clean sleep transition (`main.rs:7165`).

**Recommended fix:** a `lock_recover()` helper used everywhere; a supervised restart policy
(bounded retries + backoff) or per-iteration `catch_unwind` around the somatic loop; a
dependency-free shutdown-flag signal handler the loop polls.

### E. Cross-crate duplication & missing abstractions

- **7 hand-rolled JSON registries** (`webhooks`, `jobs`, `sessions`, `feedback`,
  `knowledge-graph`, `workspace`, `config`) each re-implement open/in-memory/`default_path`/
  atomic-flush/error-enum — and **diverge** (auto-flush vs manual-flush;
  `HOME`-missing fallback to `/tmp` vs `/root` vs `.`; `.tmp` vs `.json.tmp`). One generic
  `JsonStore<T>` collapses ~7× duplication and fixes the persistence-contract inconsistency
  (finding OPS-6) and the unsafe `/tmp` fallback (OPS-13) at once.
- **`metrics` vs `metrics-endpoint`**: two implementations emitting the *same* Prometheus
  family names with *incompatible* label schemes (`anima_tasks_total{status=…}` vs
  `{tier=…}`) — a scraper ingesting both sees conflicting series. `metrics-endpoint`
  contains **no endpoint**. One should be deleted.
- **`anthropic.rs` == `openai.rs`** (same file, 3 constants differ) → a generic
  `FixtureBackend`. Plus 4 copies of `cosine_similarity` (archival/dreaming/embedding/
  turboquant, with subtly different zero-norm handling), 3 of FNV-1a, 2 of `now_ns`, 2
  hand-rolled noop-waker `block_on` executors, smoltcp bring-up copy-pasted 3× in microVM.
- **`kernels/hosted/src/main.rs` is 7,438 lines** with a 1,136-line `print_audit` function
  duplicated across 7 `print_*_audit` helpers; `vita::LifecycleManager` is a ~30-field
  god-object; `scheduler` "streaming" fully materializes a `Vec<StreamingCompletion>` so
  token-budget/cancellation/backpressure are decorative on the dispatch path.

### F. CI / infra quick wins (cheap, high ROI)

- **`cortex/` Python has zero CI** — 1.9k lines + 12 tests never run/linted/type-checked; a
  protocol regression in `agent_loop.py` ships green.
- **No `concurrency: cancel-in-progress`** except `pages.yml` — every PR push runs a full
  redundant 13-job pipeline (incl. the expensive microVM build+boot) to completion. Biggest
  single CI-minute saver.
- **`cargo-audit`/`cargo-deny` scan only the root manifest** (`ci.yml:174`) — a banned/CVE
  crate in `xtask` or the *production* microVM kernel isn't caught.
- **No `timeout-minutes` on any job** (6h default); **clippy job is uncached** (`ci.yml:74`,
  cold-compiles the workspace every run); `release-sbom.yml` recompiles `cargo-cyclonedx`
  each release.
- **`deny.toml`** comments "promote yanked to error" but never sets `yanked = "deny"`;
  `unknown-git = "warn"`.
- **LLM live path**: no retry/backoff/`Retry-After`; ureq `http_status_as_error=true`
  discards 4xx/5xx bodies (real error info lost); Ollama aborts the whole stream on one
  unparseable NDJSON line; compat-live emits the whole answer as one token (corrupts budgets).

---

## Full findings by area

Severity: **H**igh / **M**edium / **L**ow.

### crates/self, corpus, scheduler, senses, interoception (core somatic)

| ID | Sev | File:line | Issue | Fix |
|---|---|---|---|---|
| CORE-1 | H | `self/src/lib.rs` | T-1 capability compile-fail test claimed active but absent (see Theme A) | Add `trybuild` compile-fail + positive typestate tests; add `trybuild` dev-dep |
| CORE-2 | M-H | `senses/src/lib.rs:163` | Sensory queue structurally unbounded (per-item bounds only) → memory-exhaustion DoS via operator/comms channel | Add `max_queue_depth`; reject/drop-oldest at cap |
| CORE-3 | M | `scheduler/src/mlfq.rs:238` | `dispatched_tasks.drain(0..1)` memmoves ~10k `Task`s per dispatch past cap | `VecDeque` + `pop_front`, or store lightweight records |
| CORE-4 | M | `scheduler/src/backend.rs:42` | "Streaming" future returns a fully-materialized `Vec`; budget/cancel/backpressure dead on dispatch path | Model as async `Stream`/channel; poll cancel per item; debit token pipe per token |
| CORE-5 | M | `interoception/src/budget.rs:141` | Ledger pruning keys off *incoming* record's day → out-of-order timestamp wipes today's spend (budget bypass) | Prune against `max(existing, current)` day or bucket by day |
| CORE-6 | M | `senses/src/lib.rs` (16 sites) | Lock-poison `.expect("poisoned")` on afferent hot path bricks all sensory input after one panic | `unwrap_or_else(\|e\| e.into_inner())` |
| CORE-7 | M | `self/src/lib.rs:24` | Capability fields `pub`; barrier rests solely on private `_state`; `CapabilityToken` alias dead | Sealed/newtype constructor; remove dead alias |
| CORE-8 | L-M | `senses/src/lib.rs:197` | `pub` unchecked `packetize_*` bypass all policy bounds (latent; only tests call today) | `pub(crate)`/`#[cfg(test)]` or route through checks |
| CORE-9 | L-M | `corpus/unsafe_audit.md:22` | Overstates Kani coverage of heap alignment math (see Theme A) | Scope the claim or add a Kani proof for `align_up` |
| CORE-10 | L | `interoception/src/lib.rs:96` | `compute_systemic_stress_index` returns unclamped >1.0 value named "index" | Document range or add `_clamped` variant |
| CORE-11 | L | `scheduler/src/backend.rs:84` | `text.len() as u32` truncates (not saturates) for >4 GiB prompts | `.min(u32::MAX as usize)` first, or return u64 |
| CORE-12 | L | `mlfq.rs:135`, `interoception/src/lib.rs:63` | `pub` mutable fields leak evict-on-cap / rolling-window invariants | Read-only accessors, private backing fields |
| CORE-13 | L | `scheduler/src/mock.rs:40` | Empty prompt skips cooperative-cancel check | Check `cancel` once before the loop |
| CORE-14 | L | `interoception/src/{signals,budget,power}.rs` | Redundant module-level `#![forbid(unsafe_code)]` (crate root already forbids) | Drop the module attributes |

Also: `mlfq.rs:8` starvation-boost doc overstates the latency bound (eventual progress holds;
strict per-dispatch bound does not).

### crates/vita

| ID | Sev | File:line | Issue | Fix |
|---|---|---|---|---|
| VITA-1 | H | `lib.rs:1179` | InteroceptiveSnapshot ~1 kHz not 1 Hz → floods 10k audit ring (~10 s turnover) + ~1k disk flush/s (see Theme C) | Rate-limit to 1 Hz via `last_snapshot_ns` |
| VITA-2 | H | `lib.rs:640,1116,1210` | Poison-fatal `.lock().expect()` on somatic loop → permanent PID-1 death (see Theme D); inconsistent with `dispatch.rs:90` | `lock_recover()` helper everywhere |
| VITA-3 | M | `watchdog.rs`/`prospective.rs`/`metacognition.rs` | ~1,459 lines built/tested/exported, never wired; docs claim integration (see Theme A) | Wire into `somatic_execution_loop` or fix docs |
| VITA-4 | M | `lib.rs:534` | `recent_episode_summaries` never cleared → unbounded + O(n²) reflection | `clear()`/drain after reflection; or bound |
| VITA-5 | M | `audit.rs:2086` | `Vec::drain(0..1)` per push at 10k cap → O(n) memmove (amplified by VITA-1) | Back with `VecDeque` |
| VITA-6 | M | `lib.rs:171` | `LifecycleManager` god-object (~30 fields, 6 optional subsystems, custom Debug to tame it) | Group into `Subsystems`/`SleepConfig`; privatize |
| VITA-7 | M | `lib.rs:665` vs `800` | `transition_to_sleep_state`/`run_sleep_cycle` duplicate ~110 lines incl. magic `outcomes.get(3)` indices | Extract `run_maintenance_and_postprocess` |
| VITA-8 | L | `lib.rs:1295` | Idle loop busy-polls at ~1 kHz (fixed 1 ms sleep) — root multiplier of VITA-1/5 | Adaptive backoff / block on sensory condvar |
| VITA-9 | L | `cortex_bridge.rs:784` | `CortexBackend::invoke` fully blocking; hazard if ever called on async thread w/o `spawn_blocking` | Document blocking contract; verify callers |
| VITA-10 | L | `cortex_bridge.rs:620` | Non-UTF8 `state_dir` silently redirects cortex to `/tmp` | Return `SpawnFailed` on `to_str()==None` |
| VITA-11 | L | `lib.rs:1160` | `if !decision.invoke { continue }` after operator-force is dead (always `invoke:true`) | Remove branch or comment |
| VITA-12 | L | `dispatch.rs:56`, `agent_pool.rs:262` | `pub` buffers expose mutable state bypassing flush/screen flow | Private fields + accessors |

Note: the other 280+ `unwrap/expect` in vita were spot-checked and are in `#[cfg(test)]` or
guarded by preceding invariants; VITA-2 is the only genuine production panic hazard. Time
handling (`Instant` vs `SystemTime`) is correct throughout.

### crates/memory, kv-controller, praxis, defence

| ID | Sev | File:line | Issue | Fix |
|---|---|---|---|---|
| MEM-1 | H | `defence/src/motor_gate.rs:129` | Path-traversal bypass of the "hard safety boundary" (see Theme B) ✔ | Canonicalize + component-boundary match; reject non-absolute |
| MEM-2 | H | `defence/src/injection.rs:127` | Injection detector evadable by whitespace/Unicode/homoglyph (see Theme B) | NFKC-fold, strip zero-width, collapse whitespace before match |
| MEM-3 | H | `memory/src/archival.rs:327` | L3 rewrites entire archive to JSON per demotion → O(N²) write amplification | Append-only log + periodic compaction |
| MEM-4 | M | `praxis/src/compute.rs:133` | WASM `table_growing` returns `Ok(true)` unconditionally → unbounded host mem via large funcref table | Enforce table element/byte cap like `memory_growing` |
| MEM-5 | M | `defence/src/reward_hacking.rs:21` | `"incomplete."`⊃`"complete."` → negations flagged as completion claims | Whole-word/token match; drop period-suffixed patterns |
| MEM-6 | M | `defence/src/motor_gate.rs:160` | Network gate: no SSRF/private-range defense, substring host matching | Parse URL, match host component, block private/loopback/link-local |
| MEM-7 | M | `memory/src/archival.rs:403` | `MemoryNode.sigma` dropped on L3 round-trip → decay curve corrupted | Extend payload to 6 f32 incl. sigma |
| MEM-8 | M | `defence/src/injection.rs:338` | Referenced red-team corpus + feature don't exist (see Theme A) | Add fixture+feature or fix the claim |
| MEM-9 | M | `memory/src/decay.rs:62`, `kv-controller/src/controller.rs:247` | Float-math cfg keyed on `libm` not `std` → `no_std` build without libm fails to compile | Gate intrinsic on `feature="std"`, libm on `not(std)` |
| MEM-10 | M | `memory/src/turboquant.rs:994` | Archival search re-encodes full corpus per query; archives store full f32 — inverse of TurboQuant's purpose | Quantize once at demotion, persist `QuantizedVector` |
| MEM-11 | L | `turboquant.rs:800` | `score_cosine` returns nonzero for zero/degenerate vectors (unlike f32 path) | Flag zero-norm, short-circuit to 0.0 |
| MEM-12 | L | `praxis/src/compute.rs:430` | Div-by-zero/non-finite emits invalid JSON (`{"result":inf}`) | Map non-finite to null/error |
| MEM-13 | L | `praxis/src/compute.rs:327` | `write_stdout/stderr` are no-ops; `SandboxResult.output` always empty (dead capability) | Implement copy from guest memory or drop the field |
| MEM-14 | L | `memory/src/l2_cache.rs:103` | ARC L2 get/insert are O(capacity) linear scans under one Mutex | Auxiliary `HashMap<K,index>` |
| MEM-15 | L | `kv-controller/src/trace.rs:233` | Trace ring uses `Vec::remove(0)` (O(n)/push after full) | `VecDeque` |

Positives verified: WASM sandbox enables fuel, caps linear memory, links **no** WASI (no
guest fs/network/env), fresh `Store` per call; L3 `flush` is atomic vs readers (tmp+rename)
but not crash-durable (no `fsync`).

### kernels/hosted, kernels/microvm

| ID | Sev | File:line | Issue | Fix |
|---|---|---|---|---|
| KERN-1 | M-H | `microvm/src/main.rs:227` | Panic handler discards `info`, always prints "success" banner (see Theme A) ✔ | Print `info`; give boot-complete its own marker |
| KERN-2 | M-H | `hosted/src/main.rs:6819` | No supervision: one somatic-loop error/panic kills the agent (see Theme D) | Supervised restart + backoff / per-iter `catch_unwind` |
| KERN-3 | M | `hosted/src/main.rs:6697,7165` | No SIGTERM/SIGINT graceful shutdown; hard-kill mid-task, no corpus flush | Shutdown-flag signal handler the loop polls |
| KERN-4 | M | `console/src/server.rs:233,258,495` | No socket read timeout + unbounded thread-per-connection (slowloris/thread exhaustion; reachable when bound non-loopback with token) | `set_read_timeout`; connection/SSE semaphore; header deadline |
| KERN-5 | M | `microvm/src/` | Zero tests; `acpi.rs:23` claims host validation that doesn't exist (see Theme A) | Extract pure parsers to testable lib; add boundary/fuzz tests |
| KERN-6 | M | `hosted/src/main.rs:116`, `vita/src/lib.rs:1327` | `block_on` noop-waker busy-polls on `Pending` → 100% CPU during live LLM network waits | Real single-threaded executor or parking waker |
| KERN-7 | M | `microvm/src/tls.rs:977` | TLS peer auth is self-referential (verifies against embedded private key, no cert chain) — loopback demo only, but zero real peer auth | Parse peer cert + pinned anchor before any real egress |
| KERN-8 | L-M | `hosted/src/main.rs:216` | `print_audit` ~1,136-line fn duplicated across 7 helpers | Shared formatter next to `AuditEntry` |
| KERN-9 | L-M | `hosted/src/main.rs` | 7,438-line monolith; 25-branch `if` dispatch ladder | `commands/` modules + `match` |
| KERN-10 | L | `microvm/src/net.rs:304` | Busy-polls 1.92M iters then panics (→ misleading banner) | Distinct failure marker; smaller budget + delay |
| KERN-11 | L | `microvm/src/operator_console.rs:115` | Unbounded COM1 line buffer (latent; currently `dead_code`) | Bound accumulator before enabling |
| KERN-12 | L | `vita/src/cortex_bridge.rs:791` | Predictable UDS socket path in shared temp dir (local DoS on bind) | Per-process `O_EXCL` 0700 subdir |
| KERN-13 | L | `console/src/server.rs:292` | Malformed `Content-Length` silently → 0 | Reject with 400 |
| KERN-14 | L | `console/src/server.rs:114` | Poison-fatal `.expect("poisoned")` in console server | `into_inner()` recovery |
| KERN-15 | L | `vita/src/cortex_bridge.rs:659` | No protocol version negotiation on cortex UDS handshake | Add `protocol_version`, reject mismatch |

Positives verified: hosted HTTP auth (constant-time compare, per-IP lockout, bind-exposure
policy, compile-time dashboard so no path traversal, body bounds + strict UTF-8) and the UDS
bridge (16 MiB cap before alloc, timeouts, `ChildGuard` reap+unlink) are well-hardened;
`console-proto` `write_json_str` escapes all control bytes so operator text can't break SSE
framing.

### llm-backends, console, console-proto, comms

| ID | Sev | File:line | Issue | Fix |
|---|---|---|---|---|
| IO-1 | H | `anthropic.rs:52`, `openai.rs:52`, `factory.rs:287` | Fixture-only stubs routed to when API key set → silent sentinel output (see Theme A) ✔ | Implement live client, or refuse to present stubs as usable + make fixture-miss an error |
| IO-2 | H | `ollama.rs:135`, `compat.rs:318`, `hub.rs:237` | No retry/backoff/`Retry-After` on any live path; one 429/blip aborts the task | Shared retry helper: bounded attempts, jittered backoff, retry 429/5xx, honor Retry-After |
| IO-3 | M | `compat.rs:318` | ureq `http_status_as_error=true` discards 4xx/5xx bodies; error branch dead for real errors | `.http_status_as_error(false)`, read+parse error body |
| IO-4 | M | `compat.rs:467` | Compat-live returns whole answer as one `Token` → `tokens_emitted==1`, corrupts budgets | Real SSE streaming, or set count from usage/estimate |
| IO-5 | M | `bin/anima-console.rs:107` | TUI ignores SSE status line: 401/429/404 indistinguishable from idle → silently blank console + reconnect loop | Parse status; surface auth failures; stop reconnecting on hard auth error |
| IO-6 | M | `ollama.rs:157` | One unparseable NDJSON line aborts the whole stream via `?` | `continue`+log on parse failure |
| IO-7 | M | `console/src/server.rs:263` | Unbounded `read_line` for request-line/headers *before* auth → pre-auth OOM (non-loopback+token mode) | Cap line length + header count (431) |
| IO-8 | M | `scheduler/src/backend.rs:42` | "Streaming" returns `Vec` → full buffering + blocking ureq under `block_on` blocks executor | Streaming trait surface (also fixes IO-4) |
| IO-9 | L | `compat.rs:407`, `backend.rs:83` | `usage_tokens` parsed but never consumed; no backend overrides `estimate_token_count` | Thread usage through accounting |
| IO-10 | L | `anthropic.rs`≈`openai.rs`; `native.rs` | Byte-near-identical providers; `block_on` copied in ~5 modules (see Theme E) | Generic `FixtureBackend` + shared test helper |
| IO-11 | L | `compat.rs:553`, `capabilities.rs:52` | Dead SSE parser; capability flags advertise unimplemented `streaming`/`embeddings` | Wire or delete; make flags reflect reality |
| IO-12 | L | `factory.rs:157` | Unknown `ANIMA_BACKEND` name silently falls back to Mock | Warn/return Result on unknown name |
| IO-13 | L | `bin/anima-console.rs:107` | TUI reconnect never sends `Last-Event-ID` → replays whole snapshot, duplicate feed lines | Track+send last `id:` |
| IO-14 | L | `console/src/server.rs:292` | Malformed `Content-Length` → 0 (dup of KERN-13) | Reject with 400 |

Positives verified: `console` (constant-time compare, bind policy, body/UTF-8 guards),
`console-proto` (adversarial escape suite — best-tested crate in scope), and `comms`
(redirects disabled for SSRF, EgressGuard before token read, token kept out of logs) are
solid. No secret leakage found. The suspected reqwest-0.13 `rustls` feature break was
**disproven** (feature exists in `features2`; no OpenSSL in `Cargo.lock`).

### Autonomy layer — actuators, finetune, skills, motivation, constitution, lifecycle, users

| ID | Sev | File:line | Issue | Fix |
|---|---|---|---|---|
| AUT-1 | H | `constitution.toml:37`, `check.rs:176` | `"kill"`⊂`"skill"` P1 false-positive breaks self-extension (see Theme B) ✔ | Word-boundary/token match; replace bare `"kill"` with phrases; regression test |
| AUT-2 | H | `charter.rs:224`, `defence/src/constitution.rs:25` | `hmac_verified` computed but never enforced; empty MAC accepted (see Theme A) | Require verified (strict mode); audit-entry on unseal; constant-time compare |
| AUT-3 | M | `check.rs:147` | Operator `additional_bounds` vetoes on any single ≥4-char word, ignores polarity ("read production logs" blocked by a delete-bound) | Structured/phrase rules; record matched span |
| AUT-4 | M | `skills/src/manifest.rs:163` | Skill `linked_files` accept `..`/absolute (latent traversal; skills auto-promote) | Reject `..`/leading-`/`/drive prefixes at extraction |
| AUT-5 | M | `check.rs:131` | Coarse substring keyword screen is the *only* mechanical charter enforcement; misses paraphrases | Tokenized phrase matching; document evals as the real gate; adversarial tests |
| AUT-6 | M-L | `lifecycle/src/approval.rs:180`, `skills/src/registry.rs:81`, `lifecycle/src/twin.rs:162` | Approval queue / skill registry / twin results grow unbounded; skill lookup O(n) | Cap/prune terminal states; index by id |
| AUT-7 | L | `motivation/src/{goal.rs:215,economics.rs:147,integrator.rs:172}` | `partial_cmp().unwrap()` on f32 — panics if an upstream NaN ever slips in | `total_cmp` / `unwrap_or(Equal)` |
| AUT-8 | L | `motivation/src/lattice.rs:97` | Divide by `(1.0 - suppression_threshold)` with unclamped public config → inf/NaN | Clamp threshold to `[0,0.99]` in ctor |
| AUT-9 | L | `lifecycle/src/replay.rs:254` | Cortex-outcome match uses `starts_with` over HashMap order → nondeterministic wrong-trace attach (`e1` vs `e12`) | Require exact `event_id` equality |
| AUT-10 | L | `actuators/src/web_search.rs:176` | SearXNG `categories` interpolated into URL unencoded (query param injection) | Percent-encode each category |
| AUT-11 | L | `charter.rs:116`, `unsloth.rs:136`, `web_search.rs:227` | Dead/non-enforcing API surface (`hmac_verified` is the load-bearing one) | Wire or drop |
| AUT-12 | L | `charter.rs:253`, `finetune/src/hash.rs` vs `lifecycle/src/skill_bridge.rs:67` | 3 FNV-1a, 2 HMAC/`now_ns` copies drift independently | Extract shared hash/time util (respect no_std split) |
| AUT-13 | L | `lifecycle/src/twin.rs:183` | Doc-comment formula sign mismatch vs code | Fix the doc sign |

Positives verified: `finetune` (digest-bound two-stage adoption gate) and `actuators/egress`
(SSRF hardening incl. decimal/hex/octal/IPv4-mapped + post-redirect re-screen) are the
best-designed parts of the layer. Concentrated risk is in `constitution`.

### Operational wave — quota, metrics(-endpoint), config, sessions, consent, feedback, analytics, tool-cache, knowledge-graph, alerts, webhooks, diagnostics, workspace, jobs

| ID | Sev | File:line | Issue | Fix |
|---|---|---|---|---|
| OPS-1 | H | `webhooks/src/registry.rs:85`, `endpoint.rs:55` | No SSRF gate — any URL stored (loopback/RFC1918/metadata/`file://`) ✔ | `validate_webhook_url()` on register; re-validate resolved IP at connect |
| OPS-2 | M | `webhooks/src/dispatcher.rs:113`, `hosted main.rs:4291` | Delivery simulated; `webhook test` always "succeeds" (see Theme A) | Real rustls sender behind OPS-1 gate |
| OPS-3 | M-H | `quota/src/lib.rs:361,290` | Per-user tracker map grows unbounded (`drain_stale` only trims deques) | Evict empty/idle users; periodic gc + hard cap |
| OPS-4 | M-H | `sessions/src/store.rs:230,262,183` | Only grows (`delete` sets flag) + full-file rewrite per turn → O(n²) | Retention/purge; append-only or debounced flush |
| OPS-5 | M | `feedback/src/store.rs:90`, `knowledge-graph/src/graph.rs:152` | O(n) dedup scans + full rewrites, no decay → O(n²), unbounded | `HashSet` dedup; retention/decay; true append |
| OPS-6 | M | `jobs/runner.rs:126`, `workspace/registry.rs:238`, `kg/graph.rs:112` vs `sessions`/`webhooks` | Split flush contract: `record_run_result` never flushes → job re-fires / loses retry on crash | Standardize contract; flush after mutation |
| OPS-7 | M | `jobs/src/runner.rs:67,126` | Double-fire race: job stays "due" until `record_run_result`; no claimed/in-flight state | Mark claimed (set `fired_at_ns`)+flush before run |
| OPS-8 | M | `alerts/src/rule.rs:209`, `evaluator.rs:92` | No debounce/hysteresis → flapping alert storms; `consecutive_firing` tracked but unused | Add `for_consecutive`/cooldown gating |
| OPS-9 | M | `metrics/src/prometheus.rs:22` vs `metrics-endpoint/src/lib.rs:190` | Duplicate, conflicting Prometheus impls; `metrics-endpoint` has no endpoint (see Theme E) | Collapse to one; delete the other |
| OPS-10 | M | `workspace/src/quota.rs:55`, `registry.rs:339` | 3 of 4 quota dimensions defined but never enforced (token/storage/task inert) | Wire real usage or document as advisory |
| OPS-11 | M | `tool-cache/src/lib.rs:246` | O(n) eviction scan per insert; evicts by insertion not access time | `VecDeque`/`BTreeMap` order or real LRU |
| OPS-12 | L-M | `tool-cache/src/lib.rs:87,439` | Caches everything except hardcoded `clock` → stale/side-effect risk; "dedup" has no single-flight (thundering herd) | Opt-in allowlist; single-flight |
| OPS-13 | L-M | `sessions`/`kg`/`webhooks` (`/tmp`) vs `jobs`/`workspace` (`/root`) vs `feedback` (`.`) | Inconsistent `default_path` HOME fallback; `/tmp` for durable state | One shared path helper, fail-closed |
| OPS-14 | L | `tool-cache/src/lib.rs:47`, `sessions/record.rs:77`, … | Injected `now_ns` vs direct `SystemTime::now()` — TTL misbehaves on clock jump; not hermetically testable | Thread a `Clock`/`now_ns` consistently |
| OPS-15 | L | `tool-cache/src/lib.rs:320…` | `.lock().unwrap()` panics on poisoning (cascading in PID-1) | `into_inner()` recovery |
| OPS-16 | L | `webhooks/src/dispatcher.rs:201` | Backoff uses blocking `thread::sleep` (blocks executor if driven async later) | Async retry or document sync-only |
| OPS-17 | L | `jobs/src/runner.rs:87,151` | `RetryPolicy` ignored for cron; a failing cron job can be permanently retired | Document or apply uniformly |
| OPS-18 | L | `jobs/src/finetune_trigger.rs:112` | `.expect()` on serialization (panic on production trigger path) | Propagate/`unwrap_or_default` |

**Top cross-crate observations:** (1) 7 hand-rolled JSON registries → one `JsonStore<T>`
(fixes OPS-6+OPS-13); (2) `metrics`/`metrics-endpoint` duplication (OPS-9); (3) inconsistent
time source + `check_and_consume` (quota) vs after-the-fact `check` (workspace).

### Infrastructure — cortex, xtask, trainer, CI, build

| ID | Sev | File:line | Issue | Fix |
|---|---|---|---|---|
| INF-1 | H | `.github/workflows/*` | cortex Python has zero CI (see Theme F) | `cortex` job: pytest+ruff+mypy, path-filtered |
| INF-2 | H | `ci.yml`,`bench.yml`,`docker.yml` | No `concurrency` cancel-in-progress (only pages) → redundant full pipelines | Add cancel group to ci/bench/docker |
| INF-3 | M-H | `ci.yml:174` | audit/deny scan only root manifest, skip xtask + microVM | Add `--manifest-path` for both |
| INF-4 | M | all workflows | No `timeout-minutes` (6h default) | Per-job timeouts (30–60) |
| INF-5 | M | `ci.yml:74` | clippy job uncached → cold workspace compile every run | Add cache block (distinct key) |
| INF-6 | M | `cortex/`,`trainer/` | No Python dep pinning/packaging; `pytest` listed nowhere | `requirements-dev.txt` + `trainer/requirements.txt` |
| INF-7 | M | `cortex/transformers_worker.py:59` | `_recv_frame` has no frame-size cap (unlike `ipc.py:94`) → memory DoS | Mirror the 64 MiB cap |
| INF-8 | L-M | `deny.toml:16,91` | `yanked` not set to deny despite comment; `unknown-git="warn"` | `yanked="deny"`; consider `unknown-git="deny"` |
| INF-9 | L | `pages.yml:35` | web/ no lint/typecheck (`astro check` never run) | Add `npx astro check` before build |
| INF-10 | L | `release-sbom.yml:47` | Recompiles `cargo-cyclonedx` per release (ci.yml caches it) | Reuse binary-cache pattern |
| INF-11 | L | `xtask/src/soak.rs:146` | `REQUIRED_MARKERS` stale subset vs ci.yml's 8 | Add E4.5_SOAK_DONE + E4.5B_VITA_DONE |
| INF-12 | L | `xtask/src/main.rs:20` | Doc-comment cites wrong bench-baseline flags | Delete stale block |
| INF-13 | L | Dockerfiles, compose | Base images pinned by tag not digest | Pin `@sha256:` |
| INF-14 | L | `cortex/agent_loop.py:159` | Error handler can mask original exception on send failure | try/except + `raise from` |
| INF-15 | L | `cortex/identity_memory.py:110` | `get()` conflates "key absent" with "value==default" | Private unique sentinel |
| INF-16 | L | `trainer/sleep_phase.py:311` | `__doc__.splitlines()[0]` crashes under `python -OO` | `(__doc__ or "")` |
| INF-17 | L | `xtask/src/finetune.rs:337,432` | Byte-slice `[..16]` panics on non-ASCII/short input | char-safe truncation |

Positives verified: crate count (35) and CI-job list in CLAUDE.md are accurate; actions are
SHA-pinned; permissions are least-privilege; `cortex/ipc.py` and `agent_loop.py` are
high-quality (64 MiB cap, correct partial-read/clean-EOF handling, monotonic deadlines,
thorough type hints, hermetic tests); no `eval`/`exec`/`pickle`/`subprocess` in the Python.

---

## Test-coverage summary

| Area | Rating | Notes |
|---|---|---|
| `corpus` | Good | pcb/allocators/syscall well covered; heap alignment math Kani-uncovered |
| `scheduler` | Good | mlfq/token_pipe units+Kani; backend trait defaults + post-cap eviction untested |
| **`anima-self`** | **Thin (critical)** | 2 tests; the barrier that is the crate's raison d'être has no compile-fail test |
| `senses` | Good | 33 tests; no queue-depth/concurrency test |
| `interoception` | OK–Good | budget lacks an out-of-order-timestamp test (the CORE-5 bug) |
| `vita` | Good core | ~311 tests; watchdog/prospective/metacognition unit-only (unwired); `invoke.rs` none |
| `memory` | Good | 128 tests; no L3 write-amp/sigma-round-trip/zero-vector tests |
| `kv-controller` | OK–Good | 41 tests; no libm-only compile-path or trace-eviction test |
| `praxis` | OK | 38 tests incl. strong WASM adversarial; no table-growth/div-zero/stdout test |
| `defence` | OK happy-path, **thin adversarial** | 86 tests but zero evasion/traversal/SSRF tests; referenced corpus absent |
| Autonomy | Good, `constitution` **ok w/ blind spots** | no benign-"skill" test, operator-bounds path untested, `hmac_verified` enforcement unasserted |
| Operational wave | Good unit-level | 16–82 tests/crate; **no crate has an integration `tests/` dir**; no growth/retention/concurrency coverage |
| `llm-backends` | OK fixtures, **none live** | every live network path untested; Ollama NDJSON parser has no test |
| `console` lib | Good | strong auth/route/SSE-cursor suite |
| `console` TUI binary | None | `stream_events`/`parse_tui_input` untested |
| `console-proto` | Excellent | best-tested crate in scope |
| `comms` | Good | live `post_message` only tested up to pre-socket guards |
| **`microvm`** | **None** | zero tests; pure ACPI/TLS parsers are host-testable but untested |
| `cortex` (Python) | Good but **ungated** | hermetic tests exist; never run in CI |

---

## Suggested triage order

1. **Theme A (documented-but-not-real)** — resolve each row (wire or correct), add the
   doc-claim CI grep. Highest trust impact for a safety-oriented system.
2. **Theme B (evadable safety heuristics)** — the shared normalizer + URL parser fixes
   MEM-1/2/6, AUT-1/3/4/5, OPS-1 together. AUT-1 (`kill`⊂`skill`) is a live functional bug.
3. **Theme D (PID-1 availability)** — `lock_recover()` + supervised somatic loop + signal
   handling; small, high-value, mostly mechanical.
4. **Theme C (unbounded growth)** — VITA-1 first (it silently defeats the audit log), then
   the `VecDeque` conversions and persistence/retention work.
5. **Theme F (CI)** — cheap YAML/config edits with immediate ROI.
6. **Theme E (dedup/factoring)** — `JsonStore<T>`, metrics consolidation, `FixtureBackend`,
   splitting `hosted/main.rs` and `LifecycleManager`; larger but reduces future drift.
