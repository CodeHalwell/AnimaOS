# Operator Interface

*How a human talks to — and watches — an agent that is its own user.*

---

## 1. The inversion this document respects

Every other operating system exists to put a human in control of a machine.
Anima does not (`01-architecture.md §1`). The agent runs at PID 1 and *is* the
user; the human is modelled as **a high-priority environmental signal**,
mounted alongside the agent's other senses at `/dev/anima/senses/human`
(`01-architecture.md §1.3`). And the architecture is explicit that the OS has
**no display server** — "visualisation of system state is the responsibility of
external operators connecting to the telemetry endpoint" (`02-subsystems.md`).

A human↔agent interface here is therefore **not** a control panel. Building one
as a control panel would contradict the whole design. Instead it is the
human-facing realisation of the body's existing afferent/efferent split:

| Direction      | Anatomy                          | Carries                                              | Existing substrate |
|----------------|----------------------------------|------------------------------------------------------|--------------------|
| human → agent  | **afferent** (a sense)           | guidance text + urgency                              | `senses::SensoryBridge` |
| agent → human  | **efferent + interoceptive**     | 1 Hz vitals, lifecycle state, gate rationale, audit deltas, the agent's own messages | `interoception` 1 Hz stream, `vita::AuditLog` |

Three invariants fall straight out of the paradigm (`01-architecture.md §1.3`)
and are load-bearing for everything below:

1. **The human cannot preempt the kernel.** Guidance enters the prioritisation
   queue and is weighted against the agent's current state by the Striatal
   Gate. It is never executed directly.
2. **The agent degrades gracefully when the human is absent.** There is no
   "idle waiting for the user" state — only the homeostatic loop. The console
   is an observer that may or may not be attached.
3. **Multiple operators need no architectural change.** Each is just another
   subscriber to the event stream / producer on the guidance ingress.

## 2. Architecture: one protocol, two transports

```
        ┌───────────────────── Operator (TUI / browser dashboard) ────────────────────┐
        │  vitals · state (awake/sleep+phase) · agenda · gate rationale · audit feed · │
        │  the agent's messages          +          a guidance box (text + priority)   │
        └───────────────▲───────────────────────────────────────────────┬─────────────┘
                        │ OperatorEvent  (efferent, out)                  │ OperatorInput (afferent, in)
        ════════════════╪══════════════════════════════════════════════════╪═══════════
                  console-proto  (no_std, serde-optional):  NDJSON, one object per line
        ════════════════╪══════════════════════════════════════════════════╪═══════════
   CONTAINER / HOSTED   │                            microVM (bare metal)    │
   SSE  GET /events     │                            COM1: ANIMA_TLM <line>  │
   POST /guidance       │                            COM1: ANIMA_IN  <line>  │
   (crate `console`)    ▼                            (kernel `operator_console`)
            tail vita audit log  ·  share SensoryBridge        host bridge: anima-console serial
```

The key to "one interface for both surfaces" is a single transport-agnostic
protocol crate, [`console-proto`](../crates/console-proto), with a
surface-specific carrier underneath.

### 2.1 The protocol — `console-proto`

`console-proto` is `#![no_std]` (it compiles into the UEFI kernel) and defines
exactly two message families:

```rust
// afferent — human → agent. Becomes a prioritised SensoryPacket; still gated.
pub struct OperatorInput { pub text: String, pub priority: Priority, pub force: Option<String> }
pub enum Priority { Low, Normal, High, Critical }   // mirrors senses::SensoryPriority

// efferent — agent → human. One legible, internally-tagged JSON stream.
pub enum OperatorEvent {
    Vitals { thermal_load, compute_pressure, memory_pressure,
             power_budget, financial_budget, attention_demand, aggregate_stress },
    State  { lifecycle, sleep_phase, agenda_depth },
    Gate   { invoke, cost_class, value_score, threshold, override_active, reasoning },
    Audit  { kind, detail },
    TaskStarted  { task_id, prompt },
    AgentMessage { task_id, tokens, text },   // the agent *speaking* to the operator
    Heartbeat    { uptime_secs },
}
```

Framing is **NDJSON** — one JSON object per line — chosen because it works
identically over a TCP byte stream and a serial line. Events serialise as
internally-tagged objects (`{"type":"Vitals",…}`) so a browser or TUI switches
on one field.

Two (de)serialisers, one shape:

- **std surfaces** use `serde_json` (the crate's `json` feature) for robust
  parsing in both directions.
- **the kernel** links *no* `serde_json` (image-size budget): it uses the
  dependency-free `OperatorEvent::to_ndjson()` writer and the hand-rolled
  `parse_input_line()` scanner. A `console-proto` test asserts the manual
  writer round-trips through serde, so the two surfaces interoperate
  byte-for-byte.

### 2.2 The decoupling seam (no changes to `vita`)

The console never reaches into the lifecycle. It observes the agent through the
**durable audit log** `vita` already writes (`$ANIMA_AUDIT_DIR/<agent_id>.jsonl`,
EX.2). The [`console`](../crates/console) crate's `AuditTailer` follows that
file and republishes each `AuditEntry` as an `OperatorEvent`:

- `TaskCompleted` → `AgentMessage` (the agent's reply)
- `GateDecision` → `Gate` (why it did/didn't act)
- `InteroceptiveSnapshot` → `Vitals` (down-sampled to the documented 1 Hz)
- `SleepEntered` / `WakeEntered` / `SleepPhaseStarted` → `State`
- `DefenceVeto`, `MemoryPressureEvent`, … → `Audit`

The only shared *mutable* handle is the `SensoryBridge`, which is `Clone` and
thread-safe by construction — so a POSTed guidance line lands in the very queue
the somatic loop drains. This means the console works against **any** agent
process (the `serve` command, the legacy two-agent demo, the container
`hosted` service) with zero lifecycle modifications.

## 3. Container / hosted surface (HTTP + SSE)

`anima-hosted serve` boots a single long-lived agent and the operator console:

```sh
ANIMA_BACKEND=mock anima-hosted serve
#   dashboard : http://127.0.0.1:8088/
#   events    : GET  http://127.0.0.1:8088/events   (Server-Sent Events)
#   guidance  : POST http://127.0.0.1:8088/guidance
```

The HTTP server (in the `console` crate) is hand-rolled on `std::net` so the
crate pulls in **no** third-party HTTP stack — keeping the workspace's
supply-chain audit (`deny.toml`) and build times unchanged.

| Method + path     | Role                                                            |
|-------------------|-----------------------------------------------------------------|
| `GET /`           | The self-contained browser dashboard (HTML + vanilla JS).       |
| `GET /events`     | Server-Sent Events: the live `OperatorEvent` stream + a snapshot replay so a newly-opened dashboard paints immediately. Every event carries an SSE `id:` (the audit-file byte offset of its source line), and `Last-Event-ID` on reconnect skips already-rendered replay — stable across server restarts, so a network blip or agent restart never duplicates the conversation. |
| `POST /guidance`  | Afferent ingress: an `OperatorInput` → validated → sensory packet. |
| `GET /healthz`    | Liveness probe (always open).                                   |

The agent **starts idle** and sleeps until a sense wakes it — so the demo
*is* the paradigm: send guidance, watch the agent wake (`State: Awake`), gate
the request (`Gate`), work it (`TaskStarted`), reply (`AgentMessage`), and
return to sleep through its four sleep phases.

### Clients (`anima-console`)

```sh
anima-console tui    --url http://127.0.0.1:8088     # pure-ANSI dashboard (default; SSH-friendly)
anima-console tap    --url http://127.0.0.1:8088     # print the raw event stream (scripting)
anima-console send   "summarise the overnight logs" --priority High
```

Both UIs requested — a terminal TUI (works over SSH, no browser, zero deps) and
the browser dashboard (richer gauges) — are available; they consume the same
SSE stream and POST to the same ingress.

### In Docker

`docker-compose.yml` runs `hosted` with `command: ["serve"]` and publishes the
console on host loopback only (`127.0.0.1:8088:8088`), mirroring how the Ollama
daemon is exposed. Audit logs live under the persisted `~/.anima` volume.

```sh
docker compose up --build      # then open http://127.0.0.1:8088/
```

## 4. microVM surface (serial today, TLS tomorrow)

The bare-metal kernel has no external network listener yet — `smoltcp` runs a
loopback demo and `virtio-net` is future work. The console therefore rides the
channel the kernel *already* has to the host: the **COM1 serial line**
(`kernels/microvm/src/operator_console.rs`, E6.4).

**Phase 0 (implemented).** Telemetry is framed as `ANIMA_TLM <ndjson>` lines and
guidance is read as `ANIMA_IN <ndjson>` lines, using the same `console-proto`
types — without linking `serde_json` into the kernel. A host-side bridge
re-serves them over the identical HTTP surface, so the *same* dashboard/TUI work
against a microVM:

```sh
# QEMU/Firecracker already wire COM1 to a host device or pty.
anima-console serial --device /dev/ttyS0 --http 127.0.0.1:8088
#   reads ANIMA_TLM telemetry → serves it at http://127.0.0.1:8088/
#   POSTed guidance → written back to COM1 as ANIMA_IN lines
```

The kernel boot task drives a Phase-0 demonstration and writes the
`E6.4_CONSOLE_DONE` marker the `microvm-boot` CI job can assert, alongside the
existing `E4.x` markers. The added `console-proto` dependency costs ~36 KiB,
leaving the release EFI image (~200 KiB) well inside its ≤1 MiB budget.

**Phase 1 (future).** Once `virtio-net` lands, the identical `console-proto`
messages run over the existing `smoltcp` + TLS 1.3 stack (`tls.rs` already
implements P-256 ECDHE / AES-128-GCM) and the operator connects straight to the
microVM — no host bridge. Only the transport changes; the protocol does not.

## 5. Security posture (ties into `09-threat-model.md`)

1. **No preemption.** Guidance is a sensory event arbitrated by the Striatal
   Gate. An `OperatorInput.force` requests an *audited*
   `GateOverride::OperatorForced` (escalating urgency to `Critical`) — never a
   kernel bypass. *(Wiring `force` through to a true gate override on the vita
   side is the documented follow-up; the gate already supports the override.)*
2. **Policy + defence on ingress.** Every guidance line is validated against
   the agent's `HumanGuidance` bounds (max length, blocked prefixes) by
   `packetize_text_checked` before it enters the queue, and is then subject to
   the `defence` layer's prompt-injection screening. The operator is treated as
   a potentially-compromised channel, consistent with the threat model.
3. **Transport auth.** The container server binds loopback by default and
   accepts an optional bearer token (`ANIMA_CONSOLE_TOKEN`); browsers using
   `EventSource` pass it as `?token=`. The microVM Phase-1 path inherits the
   kernel's TLS 1.3.
4. **Full auditability.** Every accepted guidance line is echoed into the event
   feed and recorded in the durable audit trail, which is persisted to L3 at the
   highest emotional weight — the canonical record of operator action.

## 6. Delivery — Epic E6 (Operator Console)

| Story | Scope | State |
|-------|-------|-------|
| **E6.1** | `console-proto`: shared `no_std` wire types + NDJSON framing; manual↔serde round-trip test. | ✅ |
| **E6.2** | Container console: hand-rolled HTTP/SSE server + `POST /guidance` in the `console` crate; `anima-hosted serve`. | ✅ |
| **E6.3** | Operator UIs: `anima-console` TUI (pure ANSI) + the embedded browser dashboard. | ✅ |
| **E6.4** | microVM Phase 0: `ANIMA_TLM`/`ANIMA_IN` serial framing + `anima-console serial` host bridge; `E6.4_CONSOLE_DONE` boot marker. | ✅ |
| **E6.5** | microVM Phase 1: `console-proto` over `smoltcp` + TLS (gated on virtio-net). | ☐ future |
| **E6.6** | Wire `OperatorInput.force` to a true audited `GateOverride::OperatorForced` on the vita side. | ☐ future |

## 7. What this deliberately is **not**

- **Not a GUI inside the OS.** Consistent with `02-subsystems.md`, the OS ships
  no display server. The dashboard is an *external* client of the telemetry
  endpoint.
- **Not a remote shell.** There is no command execution surface. The only thing
  a human can inject is a sensory signal, and the gate decides what it means.
- **Not a coupling point.** The console reads the audit log and shares the
  sensory bridge; it holds no reference to the lifecycle, scheduler, or memory
  tiers. Removing it changes nothing about how the agent runs.
