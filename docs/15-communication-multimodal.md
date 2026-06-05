# 15 — Communication & Multimodal Presence

> **Status:** Proposed (scoping). Target epic: **E10 — Presence**.
> Branch: `claude/llm-tools-animaos-vuXRK`.
> Related: E7 (tools), E8 (providers), E9 (onboarding), E11 (skills).

## 0. Goal

Let a human reach their agent through the channels they already use, with
**text, images, and voice as first-class, bidirectional pathways** — not
bolt-ons. The agent should be reachable on Slack/Discord/Telegram/etc. and able
to *see* images and *speak*, while preserving the architecture's invariant that
the human is a **gated environmental signal**, never a controller.

## 1. The key insight: reuse the operator seam

`docs/11-operator-interface.md` already defines the human↔agent split and a
decoupling seam that needs **no lifecycle changes**:

| Direction | Anatomy | Substrate (exists) |
|---|---|---|
| human → agent | afferent (a sense) | `senses::SensoryBridge` → `OperatorInput` |
| agent → human | efferent / interoceptive | `vita::AuditLog` → `OperatorEvent` (NDJSON) |

A comms app is therefore **just another operator transport**: map inbound
messages → `OperatorInput` (gated, prioritised) and `OperatorEvent` →
outbound messages. The console (HTTP/SSE) and the microVM serial line are the
first two transports; chat apps are the next. **No new lifecycle code** — a new
gateway process per channel, sharing the `SensoryBridge` and tailing the audit
log, exactly as the console does.

Current modality status: `SensoryPacket` supports `Text` and `Pcm` (16-bit
audio) — voice has a substrate; **images do not exist yet**.

## 2. Workstreams — Epic E10, stories `S10.x`

### S10.1 — Channel gateway framework

- A `ChannelGateway` trait + a host process (`anima-comms`) that runs one or
  more channel adapters. Each adapter:
  inbound → `SensoryBridge::packetize_*` (priority from channel rules);
  outbound → subscribes to `OperatorEvent` and renders `AgentMessage`/`State`/
  alerts back to the channel.
- Reuses the console's decoupling: gateways never touch `vita`.
- Identity-aware addressing: which human a channel maps to (feeds E9 identity).

### S10.2 — First channel adapters

- Ship a small set behind a common adapter API. **Recommended first:**
  **Telegram** (simplest bot API, supports text + image + voice notes natively)
  and **Slack** (events API, threads, files). Then **Discord**, **WhatsApp/
  Signal**, **email**, **SMS** as follow-ons.
- Each adapter is fixture-testable offline (recorded webhook payloads); live
  mode is env-gated with the channel's token (through the E7 secret-redaction
  path).
- Outbound requests go through the **E7 egress guard** (a webhook URL is still
  an outbound action).

### S10.3 — Image modality (afferent + efferent)

- **In:** add `SensoryPacket::Image { bytes, mime, caption? }` + checked
  packetiser with policy bounds (max size/dims), mirroring the PCM path. Routed
  to a **vision-capable** model (E8: multimodal vLLM/llama.cpp, or hosted
  Claude/GPT vision on the frontier route).
- **Out:** the agent can attach images — screenshots from the E7 browser tool,
  generated diagrams/charts, or annotated results.
- Vision capability is advertised via the E8 `BackendCapabilities.vision` flag;
  routes without it degrade to "describe the image you received" only.

### S10.4 — Voice modality (STT + TTS)

- **In:** `Pcm` frames already enqueue; add a **speech-to-text** stage
  (recommend local **whisper.cpp** as the default provider, fixture transcript
  for CI) producing a `Text` packet tagged with audio provenance.
- **Out:** a **text-to-speech** stage rendering `AgentMessage` → `Pcm`/audio
  for voice-note channels (Telegram voice, phone bridges later). Recommend a
  local TTS (e.g. Piper) as the default.
- Define `SttProvider` / `TtsProvider` traits parallel to `LlmBackend`, so voice
  backends are swappable and, like everything else, **local-first** and
  CI-hermetic by default.

### S10.5 — Modality-aware routing & presence

- The router (E5.3 / E8 §4) gains modality awareness: image/voice inputs require
  vision/STT-capable routes; the gate weighs channel + modality into urgency
  (a voice message at 2am vs a batched email differ).
- "Presence": the agent proactively reaches out on the operator's preferred
  channel for things that matter (E9 sets the channel; the gate decides *when*
  to interrupt a human — respecting graceful-degradation-when-absent).

## 3. Architecture sketch

```
 Telegram / Slack / Discord / email / phone
        │  (text · image · voice)              ▲ (text · image · voice)
        ▼                                      │
   anima-comms  ── adapter ──► SensoryBridge   │   OperatorEvent (NDJSON, audit tail)
        │            (afferent, gated)         │        ▲
        │                                      └────────┘
        ├─ image → SensoryPacket::Image ─► vision route (E8)
        └─ voice → STT (whisper.cpp) ─► Text packet ;  AgentMessage ─► TTS (Piper) → voice note
```

## 4. Cross-cutting & dependencies

- **E8** supplies vision/STT/TTS providers (multimodal models, whisper.cpp,
  Piper) — local-first, same factory/health-probe discipline.
- **E7 egress guard** screens every outbound channel call; **E7 injection
  detector** screens inbound message content (a chat message is untrusted input).
- **E9** sets the operator's preferred channel during onboarding.
- **Safety invariants preserved:** all inbound stays *gated* (never preempts the
  kernel); the agent degrades gracefully when no channel is attached.

## 5. Open questions

- First two channels: confirm Telegram + Slack (recommended) vs Discord/WhatsApp.
- Default STT/TTS engines (whisper.cpp / Piper recommended) and whether voice is
  P1 or follows image.
- Proactive-outreach policy: how aggressively may the agent initiate contact
  (ties to the gate's attention-demand signal).
