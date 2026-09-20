# 24 — Conversation UI: Human ↔ Agent Communication

> **Status:** ✅ Delivered — epic **E33 — Conversation**.
> Branch: `claude/human-agent-communication-ui-fvgp7c`.
> §1 records the audit that motivated the work (the state *before* E33);
> §3 lists the stories as shipped.
> Related: E6 (operator console, `docs/11`), E10 (presence, `docs/15`),
> E14 (metacognition — `HelpRequest`), E15 (approval queue, `docs/21`),
> E17 (user identity), E22 (sessions), E24 (feedback).

## 0. Goal

Make the operator console a place where a human and the agent can hold an
actual conversation: replies that follow from what was said before, a visible
status for every message the human sends (accepted → gated → working →
answered / refused / failed), a way for the agent to ask the human something
and get the answer back, and a history that survives a reload or a restart.

All of it keeps the invariant from `docs/11`: the human is a **sense**, not a
controller. Nothing here adds a command surface; it makes the existing
afferent/efferent seam legible and stateful.

## 1. The audit that motivated this (state before E33, September 2026)

### 1.1 What exists

| Layer | What | Where | State |
|---|---|---|---|
| Wire protocol | `OperatorInput { text, priority, force }`; `OperatorEvent` × 7 (`Vitals`, `State`, `Gate`, `Audit`, `TaskStarted`, `AgentMessage`, `Heartbeat`); NDJSON; `no_std`; manual writer ↔ serde round-trip test | `crates/console-proto` | ✅ E6.1 |
| Server | hand-rolled HTTP on `std::net`: `GET /` dashboard, `GET /events` SSE with snapshot replay + `Last-Event-ID`, `POST /guidance`, `GET /digest`, `GET /metrics`, approval-queue / skills / adapters routes, bearer token + lockout | `crates/console/src/server.rs` | ✅ E6.2 |
| Hub | in-process fan-out; 256-event replay ring; audit-file byte offsets as SSE ids | `crates/console/src/hub.rs` | ✅ |
| Audit tailer | `AuditEntry` → `OperatorEvent`; `TaskCompleted` → `AgentMessage`; everything without a richer variant → `Audit { kind, detail }` | `crates/console/src/audit.rs` | ✅ |
| Browser dashboard | one self-contained HTML file (vanilla JS, ~800 lines): organism column (lifecycle, vitals, gate, approval / skills / adapters panels), conversation column (bubbles, typing indicator, gate/veto notices, composer with priority), telemetry feed with filters, "while you were away" digest | `crates/console/src/dashboard.html` | ✅ E6.3 |
| TUI | pure-ANSI vitals + feed; each stdin line is guidance; `!` / `!!` raise priority | `crates/console/src/bin/anima-console.rs` | ✅ E6.3 |
| Docs-site island | React mock of vitals + feed against the same SSE contract; no composer | `web/src/components/OperatorConsole.tsx`, `web/src/lib/consoleStream.ts` | demo only |
| Conversation memory | `SessionStore` / `SessionRecord` / `ConversationTurn`: atomic JSON persistence, search, export; `anima sessions` CLI | `crates/sessions` | ✅ E22 — **not connected** to the console or the serve loop |
| Feedback | `FeedbackStore`, ratings, quality report; `anima feedback` CLI | `crates/feedback` | ✅ E24 — CLI only |
| Identity | `UserRegistry`, trust tiers, consent | `crates/users` | ✅ E17 — console unaware |
| Agent-initiated questions | `HelpRequest` (low-confidence signal) | `crates/vita/src/metacognition.rs` | type only — nothing emits it to an operator |
| Channels | `ChannelGateway` + Telegram / Slack fixture adapters, modality router | `crates/comms` | ✅ E10 — demo binary |

### 1.2 What actually happens when you type in the box

Trace verified on this branch with `ANIMA_BACKEND=mock anima-hosted serve`,
three guidance lines sent and `GET /events` tapped:

1. `POST /guidance` → `packetize_text_checked` (policy bounds) → the **raw
   text** becomes a `PrioritizedPacket`. The server echoes
   `Audit { kind: "OperatorGuidance", detail: "[Normal] <text, cut at 200 chars>" }`.
   No id is attached to the echo.
2. The somatic loop (`vita::somatic_execution_loop`, step 2) turns the packet
   into an agenda `Task` whose `prompt` **is the raw text** — no system
   framing, no identity, no previous turns, no tools. Sensory task ids are
   minted from `1 << 63`.
3. Only a **forced** packet is evaluated by the Striatal Gate
   (`GateDecision` → `Gate` event). Normal / High / Critical guidance goes
   straight onto the agenda. The dashboard's hint that guidance "is
   arbitrated by the Striatal Gate" is true for `force` only.
4. `Scheduler::dispatch_task` calls `backend.stream_completion(&task.prompt)`
   and collects the whole `Vec<StreamingCompletion>` before returning. There
   is no live token stream anywhere in the stack; the Anthropic client also
   does a non-streaming POST and chunks the answer by word afterwards.
5. `TaskCompleted { response }` → `AgentMessage { task_id, tokens, text }`.
   The reply is linked to the guidance only by arrival order: the UI keys
   the typing indicator on `TaskStarted.task_id`, and nothing carries the
   operator's message id through.
6. The agent sleeps and runs all four sleep phases within about a second of
   every reply, so the feed fills with `SleepPhaseCompleted` rows.
7. With the mock backend the reply is the prompt echoed word by word. Fine
   for CI; useless for judging the conversation.
8. On reload the dashboard gets the hub's last 256 feed events; after a
   restart the tailer re-reads the audit file from offset 0 so the same 256
   come back. Anything older is gone from the UI (still in the JSONL).

### 1.3 Gaps, in order of how much they hurt — all now closed

Each is annotated with the story that fixed it.


1. **Replies are not conversational.** *(fixed: S33.1.)* The model never sees the previous
   turn, so "and the second one?" cannot work. (`vita` loop step 2 →
   `scheduler::mlfq::dispatch_task`.)
2. **No message correlation or status.** *(fixed: S33.2.)* The `OperatorGuidance` echo has no
   id; `TaskStarted` carries the prompt only. The UI cannot say "this reply
   answers that message" or "this one was refused".
3. **The agent cannot ask.** *(fixed: S33.3.)* No event variant; `HelpRequest` is never
   emitted; approval proposals reach the side panel (when wired) but never
   the conversation.
4. **History does not persist for the UI.** *(fixed: S33.1.)* `crates/sessions` is unused on
   the serve path; the hub ring is the only memory.
5. **Three panels are dead under `serve`.** *(fixed: S33.0.)* Commit `ae6b71a` says it wired
   `with_approval_queue` / `with_skill_registry` / `with_adapter_library`
   into `cmd_serve`; the diff only added the Cargo dependency.
   `GET /approval-queue`, `/skills`, `/adapters` return 404 in `serve`, so
   the panels stay hidden. (Verified on this branch.)
6. **Small UI defects.** *(fixed: S33.0 and S33.4.)* Forced guidance renders as a raw
   `[FORCED:Critical] (Reason: …)` bubble because the meta regex only
   matches `[Word]`; the echo truncates at 200 chars so long guidance is cut
   in the conversation; `force` has no affordance in the composer; the uptime
   chip never fills because the `Heartbeat` is only sent after 15 s of
   silence and the 1 Hz vitals never leave the stream silent; task ids
   ≥ 2^63 need the raw-frame regex the dashboard already has, and every new
   client must copy it.
7. **Feedback, identity, attachments.** *(feedback and identity fixed:
   S33.4 and S33.5; attachments remain open — see §5.)* The crates exist; there is no route
   and no UI. Images can be packetised (`packetize_image_checked`) but the
   loop reduces them to `[Image N B mime]` text, so without a vision route a
   UI attachment would be theatre.

## 2. Design

### 2.1 Keep the seam

The UI stays a client of `GET /events` + `POST /guidance`. Two additions to
the seam, nothing else:

- a **conversation memory** the loop consults when it builds a prompt and
  updates when a reply lands (fixes gaps 1 and 4);
- **three protocol additions** so clients can thread messages and the agent
  can ask (fixes gaps 2 and 3).

### 2.2 Protocol (`console-proto`, stays `no_std`)

```rust
pub struct OperatorInput {
    pub text: String,
    pub priority: Priority,
    pub force: Option<String>,
    pub message_id: Option<String>,   // NEW — client-minted, echoed back
    pub reply_to: Option<String>,     // NEW — answers an AgentQuestion
}

pub enum OperatorEvent {
    // existing variants unchanged …
    TaskStarted   { task_id, prompt, message_id: Option<String> },        // + correlation
    AgentMessage  { task_id, tokens, text, message_id: Option<String> },
    Accepted      { message_id, priority, forced: bool, text },           // NEW — typed echo
    AgentQuestion { task_id, question_id, text, options: Vec<String>, reason }, // NEW
}
```

Rules: new fields are optional and default on the wire, so the kernel's
`parse_input_line` keeps accepting old input; the manual `to_ndjson` writer
gains the two variants; `manual_ndjson_round_trips_through_serde` is
extended. The microVM emits nothing new.

`Accepted` replaces the free-text `OperatorGuidance` echo for consoles that
understand it; the `Audit` echo stays one release for the TUI and the
docs-site island.

Correlation path: `message_id` rides the `PrioritizedPacket` (new optional
field in `senses`), `vita` keeps a `task_id → message_id` map from intake to
dispatch, and the `TaskStarted` / `TaskCompleted` audit entries carry it.
`scheduler::Task` is untouched.

### 2.3 Conversation memory — S33.1

A trait in `vita` behind the `std` feature, following the
`Subsystems::watchdog` pattern, called at the two points the loop already
touches:

```rust
pub trait ConversationMemory: Send {
    /// Wrap freshly-accepted guidance in the context the model needs:
    /// identity framing, the last N turns, the open question if `reply_to`.
    fn compose(&mut self, task_id: u64, guidance: &str, reply_to: Option<&str>) -> String;
    /// A reply landed; persist it as the assistant turn.
    fn record_reply(&mut self, task_id: u64, response: &str);
}
```

The host implementation (`kernels/hosted`) sits over `sessions::SessionStore`:
one active session per operator, the `IdentityMemory` document as the system
framing, a bounded window of recent turns, trimmed by
`LlmBackend::estimate_token_count`. Because it composes a **raw prompt** it
works with every existing `LlmBackend` (mock, Ollama, OpenAI-compatible,
Anthropic) without touching the scheduler, and `anima sessions show` then
prints exactly what the console shows.

Two rules: composition happens **after** the policy check (bounds and blocked
prefixes apply to the human's words, not to the context); and
`TaskStarted.prompt` keeps recording the human's text, with the composed
length recorded separately, so the audit log and the UI do not fill with
repeated context.

Later, not in this epic: route operator tasks through `ChatCortexBridge` for
tool use; the memory then becomes the message list rather than a string.

### 2.4 Agent-initiated questions — S33.3

Two sources, one event:

1. `ConfidenceTracker` below the help-request floor → `HelpRequest` → new
   `AuditEntry::HelpRequested { task_id, question, confidence, reason }` →
   tailer → `AgentQuestion`. The operator answers with
   `OperatorInput { reply_to }`; the composer puts the question back in front
   of the model.
2. `ApprovalProposalQueued` → `AgentQuestion` with `options = ["approve",
   "reject"]` and the proposal id. The answer stays
   `POST /approval-queue/{id}/…`, so no new authority is introduced.

Questions are efferent; answers are afferent and still policy-checked. The
invariant holds.

### 2.5 HTTP additions

| Route | Purpose |
|---|---|
| `GET /conversation?limit=N&before=<turn>` | turns from the session store, for page load and scroll-back; replaces reliance on the 256-event ring |
| `POST /guidance` | accepts `message_id` and `reply_to`; the 202 body returns the `message_id` (server-minted when absent) |
| `POST /feedback` | `{ task_id, rating, comment? }` → `FeedbackStore` + `FeedbackReceived` audit |
| `GET /whoami` | operator identity (S33.5) |

### 2.6 The dashboard and the TUI — S33.2 / S33.4

Keep the embedded single-file dashboard as the shipped surface: zero
dependencies, served by the binary, reachable through the serial bridge.
Changes:

- **Threaded conversation.** One operator bubble per `message_id`; a status
  line under it (accepted · gated invoke / block with the reason · working ·
  answered / failed / vetoed); the reply nests under it. Gate and veto
  notices move from free-floating to under the message they belong to.
- **History.** Load `GET /conversation` on open; "load earlier" on scroll-up;
  the SSE stream only appends.
- **Composer.** Multi-line (`Shift+Enter`), `Ctrl+Enter` to send, priority as
  a segmented control, a `force` toggle that reveals a reason field
  (audited), the draft kept in `localStorage`.
- **Rendering.** Markdown-lite (paragraphs, fenced code, lists, inline code)
  on top of the existing escaping; long replies collapsible; copy button.
- **Questions.** `AgentQuestion` as a card with quick-reply buttons;
  approval proposals as cards inline in the conversation as well as the side
  panel.
- **Feedback.** Thumbs up / down on agent bubbles → `POST /feedback`.
- **Noise.** Collapse the four `SleepPhaseCompleted` rows per cycle into one
  feed row; the conversation column stays clean.
- **Defects** from §1.3 item 6 fixed; the `task_id` precision note added to
  `web/src/lib/consoleStream.ts`.
- **TUI parity.** Message ids and status glyphs in the feed so SSH users see
  the same thread.

### 2.7 Decisions (recommendation first)

1. **Surface: vanilla embedded dashboard or a React app?** Keep vanilla for
   the console that ships in the binary: no build step, no bundle in the
   image, no CI change, serial-bridge parity. A richer React operator app
   can be built later in `web/` against the same HTTP contract
   (`consoleStream.ts` already mirrors it). Good TypeScript / React practice,
   but not on the critical path.
2. **Streaming.** Real token streaming needs `LlmBackend` to expose a sink
   (or the scheduler to forward tokens as they arrive) plus an `AgentToken`
   SSE event. Defer: with today's backends it would be fake anyway.
3. **Gate every operator packet.** Today only forced guidance is gated.
   Recommend gating all operator packets: it costs one gate evaluation per
   message, matches what the docs already claim, and gives the per-message
   status something true to show. (S33.6.)
4. **Sessions.** One active session per operator identity, archived by
   `anima sessions archive` or after a configurable idle period. Simple and
   matches E22.

## 3. Workstreams — Epic E33, stories `S33.x`

All shipped. Each row names where the behaviour now lives.

| Story | Scope | State |
|---|---|---|
| **S33.0 Unblock** | Wire the approval queue, skill registry (shared with `LifecycleManager` so the panel reflects live reflection output) and adapter library into `cmd_serve`; periodic heartbeat on a 5 s wall-clock cadence; guidance echo raised from 200 to 4000 chars; forced-bubble renderer parses the colon-bearing label and the reason clause. | ✅ (`kernels/hosted/src/commands.rs`; `crates/console/src/server.rs`; `dashboard.html`; 4 tests) |
| **S33.1 Conversation memory** | `vita::ConversationMemory` trait + `Subsystems::conversation`; `compose` on dispatch, `record_reply` on completion; host `SessionConversation` over `sessions::SessionStore` with identity framing and a 6000-char window trimmed from the oldest end; `GET /conversation`; the dashboard loads history and reconciles it against the replay ring. | ✅ (`crates/vita/src/conversation.rs`; `kernels/hosted/src/conversation.rs`; `crates/console/src/server.rs`; 11 tests) |
| **S33.2 Correlation** | `message_id` and `reply_to` on `OperatorInput`; optional `message_id` on `Gate`/`TaskStarted`/`AgentMessage`; typed `Accepted` event; `AuditEntry::OperatorMessageLinked` emitted at intake; `console::CorrelationTracker` as the reader half; per-message status in the dashboard and id tags in the TUI. | ✅ (`console-proto`, `senses`, `vita`, `console`; 13 tests) |
| **S33.3 Agent questions** | `AuditEntry::HelpRequested` from a sub-floor confidence score; `ApprovalProposalQueued` read the same way; both surface as `AgentQuestion`; the question is recorded as the agent's own turn so an answer arrives with it in context; question cards with quick replies routed by kind. | ✅ (`crates/vita/src/lib.rs`, `crates/console/src/audit.rs`, `dashboard.html`; 4 tests) |
| **S33.4 Conversation view** | Markdown-lite rendering (escape-first), collapsible long replies, copy and rating controls, `POST /feedback` into the E24 store, multi-line composer with Enter-to-send and a force toggle that requires a reason, draft persistence, sleep-phase feed collapse; `consoleStream.ts` contract updated. | ✅ (`dashboard.html`, `crates/console/src/server.rs`, `web/src/lib/consoleStream.ts`; 4 tests) |
| **S33.5 Identity** | `GET /whoami` from the E17 `UserRegistry`: the profile conversations and feedback are attributed to, and its trust tier. Trust is never inferred from reaching the console. | ✅ (`crates/console/src/server.rs`, `kernels/hosted/src/commands.rs`; 5 tests) |
| **S33.6 Gate every packet** | The Striatal Gate arbitrates *all* operator guidance, not only forced packets, against the last real interoceptive reading. A stressed agent defers ordinary chatter and still takes Critical. `ANIMA_GATE_OPERATOR=0` restores unconditional admission. | ✅ (`crates/vita/src/lib.rs`; 3 tests) |

**Epic exit criteria.**
1. A reload shows the full conversation from the session store, not the ring. ✅
   (`GET /conversation` + the dashboard's history load; verified across a
   restart — the same session continues.)
2. Every operator message shows a status that reaches a terminal state. ✅
   (`Accepted` → `Gate` → `TaskStarted` → `AgentMessage`, all carrying the
   same `message_id`; the dashboard hangs the status under the bubble.)
3. With a real backend the agent answers a follow-up that only makes sense
   given the previous turn. ✅ for the composition path — the composed prompt
   demonstrably carries the prior exchange. **Still to do by hand:** run it
   against Ollama or a frontier key and judge the answers. The mock backend
   echoes its prompt, so it cannot show this.
4. A low-confidence completion produces a question card; answering it produces
   a reply that carries the question context. ✅
5. `cargo test --workspace --all-targets` (2172 tests), `cargo clippy
   --workspace --all-targets -- -D warnings` and `cargo fmt --check` clean;
   the `console-proto` manual/serde round trip covers both new variants
   including a `None` correlation; `vita` still builds
   `--no-default-features --features libm` for the microVM. ✅

### 3.1 What is still open

- **Judging it against a real model** (exit criterion 3) — the one thing a
  mock cannot verify.
- **Token streaming.** Still absent, deliberately: `LlmBackend` collects the
  whole completion before returning, so an `AgentToken` event would be
  theatre. It needs a sink on the backend trait first.
- **Attachments.** `packetize_image_checked` exists, but the somatic loop
  reduces an image to `[Image N B mime]` text, so a UI attachment would be
  theatre too until a vision route is bound.
- **Tool use on operator tasks.** Operator guidance still dispatches through
  `LlmBackend`, not `ChatCortexBridge`. `ConversationMemory` is the seam that
  would change — `compose` would return a message list rather than a string —
  and it does not need to move for it.
- **Session growth.** `SessionRecord` grows without bound; archival is E22's
  `anima sessions archive`, not automatic.

## 4. What you need to try it

Stable Rust only (the hosted target); no Node for the embedded dashboard.
Chromium is handy for screenshots but optional.

```sh
# Zero-dependency parrot — plumbing and protocol only; replies echo the prompt.
ANIMA_BACKEND=mock cargo run -p hosted --bin anima-hosted -- serve

# Real replies — local Ollama (the docker-compose default) …
ANIMA_BACKEND=ollama cargo run -p hosted --bin anima-hosted -- serve
# … or a frontier key.
ANIMA_BACKEND=anthropic ANTHROPIC_API_KEY=… cargo run -p hosted --bin anima-hosted -- serve

# Then:
open http://127.0.0.1:8088/
cargo run -p console --bin anima-console -- tui --url http://127.0.0.1:8088
curl -sN http://127.0.0.1:8088/events          # raw stream, for debugging the UI
```

Judge the conversation against a real backend. The mock backend proves the
plumbing, not the experience — it echoes whatever prompt it is given, so with
conversation memory on it replies with the composed context rather than an
answer. `serve` says so on boot.

Useful switches:

| Variable | Effect |
|---|---|
| `ANIMA_CONVERSATION=0` | bare, stateless loop — no memory, no history |
| `ANIMA_GATE_OPERATOR=0` | admit operator guidance unconditionally (pre-E33 behaviour) |
| `ANIMA_OPERATOR_ID` | the identity conversations and feedback are recorded against |

Routes the console serves: `/`, `/events`, `/guidance`, `/conversation`,
`/whoami`, `/feedback`, `/digest`, `/metrics`, `/healthz`, `/approval-queue`
(+ approve/reject), `/skills`, `/adapters`.
