# 12 — Real-World Tools: Embodiment & Efferent World-Interaction Plan

> **Status:** Proposed (scoping). Target epic: **E6 — Embodiment**.
> Branch: `claude/llm-tools-animaos-vuXRK`.
> Companion: [13 — Local LLM Provider Ecosystem](./13-local-llm-providers.md)
> (E7) supplies the *brains* that E6's tools give *hands*. The chat/tool-calling
> trait extension (E6 S6.4 §) is shared with E7 §5.

## 0. Goal

Give the cortex (the deliberative LLM layer) genuine ability to *act on the
world*, beyond the current four inert primitives (`clock`, `echo`, `text-io`,
`wasm-math`). Concretely:

1. **`web-search`** — query the web via a self-hosted **SearXNG** instance.
2. **`browser`** — read and interact with live pages via a **Playwright**
   subprocess driver.
3. **Live LLM backends** — make **Anthropic** (frontier route) and **Ollama**
   (cheap-local route) live so the cortex actually emits tool calls instead of
   replaying a mock plan.
4. **Semantic tool selection** — wire the existing
   `praxis::routing::length_robust_filter` into the dispatch path, fed by a
   **local-embedding** scorer over each tool's description.

All of this must remain **CI-hermetic by default** (fixtures/mocks, no network)
and pass every outbound action through the existing safety boundary (the
`UnsafeMotorActionGate`, S5.6.4) before it executes.

---

## 1. Current state (grounding)

| Concern | Today |
|---|---|
| Tool inventory | `clock`, `echo`, `text-io` (`praxis/registry.rs`), `wasm-math` (`praxis/compute.rs`). No network/browser/search drivers exist. |
| Tool trait | `ToolDriver` is **synchronous**: `invoke(&[u8]) -> Result<Vec<u8>, ToolInvocationError>` (`praxis/lib.rs`). |
| Dispatch path | cortex emits `ToolCall{call_id, tool_name, args:String}` → `PythonCortexBridge` loop (`vita/cortex_bridge.rs`) → `ToolDispatcher::dispatch(name, args) -> Result<String,String>` → `ToolRegistry::dispatch(ToolEnvelope)` → `ToolDriver::invoke`. |
| Routing | `StaticRouter` maps `CostClass` → one of three routes; tool access is a **static allow-list per tier**. `SemanticClass` is accepted but **ignored**. |
| Semantic filter | `length_robust_filter(candidates, tau_rel)` exists and is unit-tested, but is **not wired** into any router, and **no scorer** produces `ToolCandidate.score`. |
| LLM backends | `llm-backends/` has Anthropic/OpenAI/Ollama, all defaulting to **fixture replay** (no network). |
| Cortex | Python subprocess; `agent_loop.py` runs a **mock** plan (`clock` then `echo`). Comments already anticipate a real-LLM path. |
| Egress safety | `UnsafeMotorActionGate` (S5.6.4) already screens **network requests by host blocklist** and filesystem writes via object-capabilities (`anima_self`). It is **not yet invoked per tool-call** in the dispatch loop. |
| Defence screening | `DefenceLayer` currently screens only the **final** `InvokeComplete` output (a `CompletionClaim`), not individual actions. |

### Two architectural facts that shape everything

- **Sync trait vs async I/O.** `ToolDriver::invoke` is synchronous; web/browser
  tools need async I/O. We resolve this by having network drivers own (or share)
  a Tokio runtime handle and `block_on` internally, keeping the trait unchanged.
  Network drivers are **std-only**, gated behind a new feature, exactly as the
  Wasmtime `compute` module already is.
- **Egress must be gated before execution.** Today defence only inspects the
  final answer. Real-world actions need **pre-execution** screening through the
  motor gate (already host-aware) plus an SSRF guard. The tool's *output* (web
  text) must also be treated as untrusted and fed to the existing injection
  detector.

---

## 2. Design principles

1. **The tier is the security boundary; semantics is relevance.** Semantic
   selection narrows tools *within* a route's permitted allow-list — it never
   widens it. A cheap-local route can never gain `browser` access via scoring.
2. **Reuse the proven plumbing.** Envelope bus, circuit breakers, `ChildGuard`
   subprocess lifecycle, length-prefixed-JSON IPC, and the motor gate already
   exist. New tools plug into them rather than inventing parallel machinery.
3. **CI stays offline.** Every new capability ships with a deterministic
   fixture/mock impl that is the default in tests; live paths are opt-in behind
   env vars and `#[ignore]`/feature gates.
4. **Untrusted-in, gated-out.** Inbound web content is untrusted (prompt
   injection); outbound actions are gated (SSRF, exfiltration, rate limits).
5. **Everything is audited.** New `AuditEntry` variants make egress, tool
   selection, and browser actions visible to `anima why`.

---

## 3. Workstreams

Phased so each phase is independently shippable and testable. **Epic E6**,
stories `S6.x`.

### Phase 0 — Foundations: async + egress safety (`S6.0`)

The enabling layer everything else depends on.

- **S6.0.1 — Network driver substrate.** New crate **`crates/actuators`**
  (std-only, outside the `no_std` core, precedent: `llm-backends`). Provides a
  shared Tokio runtime handle and a `NetworkTool` helper so a sync
  `ToolDriver::invoke` can `block_on` async work. Keeps `praxis` lean and
  `no_std`-clean.
- **S6.0.2 — Egress guard.** `EgressGuard` module: URL/scheme allow-list
  (`https` only by default), **SSRF protection** (reject private/loopback/
  link-local ranges and cloud metadata IPs `169.254.169.254`), configurable
  domain allow/deny lists, and a per-host token-bucket rate limiter.
- **S6.0.3 — Motor-gate hook at dispatch.** Extend the vita tool-dispatch loop
  so every call carrying an outbound effect is screened by
  `UnsafeMotorActionGate` **before** `invoke`. Extend `defence::ActionKind`
  with `NetworkRequest { url }` and `BrowserNavigate { url }`. Emit
  `AuditEntry::EgressRequested` / `EgressBlocked`.
- **S6.0.4 — Config & secrets.** Env-var plumbing
  (`ANIMA_SEARXNG_URL`, `ANTHROPIC_API_KEY`, `ANIMA_OLLAMA_URL`, etc.) with
  **redaction** in audit/log output. A small typed config struct, no secrets in
  episode summaries.

**Exit criteria:** (1) a network `ToolDriver` can perform a fixture-backed async
fetch through the sync trait; (2) a request to a private IP or blocklisted host
is rejected pre-execution and audited; (3) no secret ever appears in the audit
log (asserted by test).

### Phase 1 — `web-search` tool via SearXNG (`S6.1`)

- **S6.1.1 — `SearchProvider` trait** with two impls: `SearxngProvider`
  (HTTP `GET {SEARXNG_URL}/search?q=…&format=json`) and `FixtureProvider`
  (replays recorded JSON for CI).
- **S6.1.2 — `WebSearchTool: ToolDriver`.** Schema
  `{ query: string, max_results?: int, categories?: [string] }`; returns ranked
  JSON `[{title, url, snippet}]`. Routes through the egress guard + circuit
  breaker. Output flagged untrusted.
- **S6.1.3 — Registration & routing.** Register the tool; add to the **frontier**
  (and optionally **mid-tier**) route `ToolScope`. Author a high-quality
  `ToolSpec.description` (this is what the semantic scorer embeds).
- **S6.1.4 — Ops.** Add a `searxng` service to `docker-compose.yml`; document
  setup. CI uses the fixture provider only.

**Exit criteria:** (1) mock-cortex integration test drives a search end-to-end
against the fixture provider and gets ranked results; (2) live test
(env-gated, `#[ignore]`) hits a real SearXNG; (3) egress guard + circuit breaker
exercised by unit tests.

### Phase 2 — `browser` tool via Playwright subprocess (`S6.2`)

- **S6.2.1 — Playwright driver process.** A Node (or Python) Playwright worker
  that speaks the **same length-prefixed-JSON-over-UDS** protocol as the cortex.
  Commands: `navigate`, `read_text`/`readable`, `click`, `type`, `get_links`,
  `screenshot`. One long-lived browser context; a page per task.
- **S6.2.2 — `BrowserBridge`** (in `crates/actuators`), managing the subprocess
  lifecycle with the existing `ChildGuard` RAII pattern from `cortex_bridge.rs`.
- **S6.2.3 — `Browser*` ToolDrivers.** `browse`/`navigate`/`extract`. Every
  navigation screened as `ActionKind::BrowserNavigate` (motor gate + egress
  guard on the target URL). Resource limits: max pages, per-action timeout, max
  response bytes.
- **S6.2.4 — Hermetic tests.** A `MockBrowserDriver` (in-Rust, no Chromium)
  satisfies CI; a real-Chromium smoke test is env-gated. Document the Node +
  browser install as an **optional capability** (heavier dependency; the
  fetch/readability fallback from a future `S6.2.x` can serve no-Node installs).

**Exit criteria:** (1) mock-browser integration test performs navigate→extract
end-to-end; (2) navigation to a blocked host is vetoed and audited;
(3) subprocess is reaped cleanly on success, error, and timeout.

### Phase 3 — Semantic tool selection (`S6.3`)

- **S6.3.1 — `ToolScorer` trait.** `score(query, &[ToolSpec]) -> Vec<ToolCandidate>`.
  Impls: `EmbeddingScorer` (local model via **fastembed-rs**/candle, e.g.
  `bge-small`/`all-MiniLM`, cosine similarity), plus a deterministic
  `LexicalScorer` (BM25) and `FixtureScorer` for CI.
- **S6.3.2 — Tool index.** Embed each registered tool's `ToolSpec.description`
  once at startup; cache vectors; re-embed on registry change.
- **S6.3.3 — Wire into dispatch.** New stage in `build_routed_request` (or a
  pre-step in vita dispatch): **start from the route's tier allow-list** →
  score against the task description → `length_robust_filter(tau_rel)` →
  final `ToolSpec` list handed to the cortex. Selection only ever *narrows* the
  tier set.
- **S6.3.4 — Config & audit.** Per-route `tau_rel` and max-tools cap. New
  `AuditEntry::ToolSelection { candidates_scored, kept, tau_rel }`.
- **S6.3.5 — Benchmarks.** Reuse the documented benchmark set already in
  `registry.rs` tests; assert correct selection, determinism, and `tau_rel`
  sweeps.

**Exit criteria:** (1) given a task + a tool library larger than the route cap,
the cortex receives exactly the relevance-filtered subset; (2) selection is
deterministic for fixed inputs; (3) the tier boundary is never widened by
scoring (asserted).

### Phase 4 — Live LLM backends & real cortex tool-calling (`S6.4`)

- **S6.4.1 — Anthropic live mode.** Real HTTPS + streaming + tool-use blocks;
  map `ToolSpec` → Anthropic tool schema; parse `tool_use` → `ToolCall`, feed
  `tool_result` back. Egress + key handling via Phase 0.
- **S6.4.2 — Ollama live mode.** Local HTTP tool-calling (native tool API, with
  a prompt-format fallback for models lacking it). Maps to the cheap-local tier.
- **S6.4.3 — Real `agent_loop.py`.** Replace the mock plan with an LLM-driven
  Plan/Act/Observe/Revise loop that consumes `InvokeRequest.tools`, emits real
  `ToolCall`s, consumes `ToolResponse`, and honours `max_turns`/`max_tool_calls`.
  Backend chosen via the existing `--backend` flag (`anthropic|ollama|mock`).
- **S6.4.4 — Route → backend wiring.** `ModelSelector::Frontier → anthropic`,
  `CheapLocal → ollama`, `MidTier → ` (Ollama-large or Claude Haiku — TBD).
  Pass the backend hint through `InvokeRequest`/spawn.
- **S6.4.5 — Tests.** Fixture-mode end-to-end stays the CI default; live smoke
  tests are env-gated.

**Exit criteria:** (1) with fixtures, a routed task runs a multi-step
tool-calling loop with no network; (2) env-gated live smoke test completes a
real search-and-summarise task; (3) tier→backend mapping is asserted.

---

## 4. Cross-cutting concerns

- **Threat model (`docs/09-threat-model.md` update):** SSRF, prompt-injection
  via fetched web content (route tool output through the existing injection
  detector; never auto-execute fetched instructions), data exfiltration via
  egress, secret handling/redaction.
- **Homeostatics:** network/browser/LLM calls have real financial + power cost;
  feed per-tool cost into the gate's budget signals so resource pressure can
  throttle world-interaction (ties into the existing E5.7 modulation rules).
- **Observability:** new audit variants surfaced through `anima why` and the
  operator console; web/ diagrams + glossary + roadmap updated.

---

## 5. Recommended first PR (vertical slice)

**Phase 0 + Phase 1 + a minimal slice of Phase 3**, proving the whole path
end-to-end in CI without network:

```
cortex ──ToolCall──► vita dispatch
                      │  ├─ semantic select (LexicalScorer → length_robust_filter)
                      │  ├─ motor-gate / egress screen (pre-exec)
                      │  └─ WebSearchTool ─► SearchProvider (FixtureProvider in CI)
                      ◄──ToolResponse── ranked results, audited
```

This lands the egress substrate, the first real tool, and the long-dormant
semantic filter wired in (BM25 first, embeddings as fast-follow in S6.3.1).
**Playwright (Phase 2)** and **live Anthropic/Ollama (Phase 4)** follow as
separate PRs to keep review surface manageable.

---

## 6. Risks & open questions

- **Sync-trait bridging:** shared injected Tokio runtime vs per-driver runtime
  — recommend a single shared handle owned by `crates/actuators`.
- **Crate placement:** new `crates/actuators` keeps `praxis` `no_std`-clean
  (precedent: `llm-backends` lives outside the core). Confirm naming.
- **Embedding model distribution:** fastembed downloads ONNX models at runtime
  — size/license/offline implications for an agent OS; may need vendoring.
- **Playwright footprint:** Node + browser binaries conflict with the
  minimal-footprint ethos; ship it **feature-gated/optional**, with a
  fetch+readability fallback for no-Node installs.
- **Live cortex is the biggest unknown:** the agent loop is mock today; making
  it genuinely LLM-driven (Phase 4) warrants its own design pass and may reveal
  protocol gaps (streaming, partial tool calls, retries).

---

## 7. Rough effort

| Phase | Size |
|---|---|
| P0 Foundations | M |
| P1 web-search | M |
| P2 browser/Playwright | L |
| P3 semantic selection | M |
| P4 live LLM + cortex | L |
