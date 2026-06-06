#![forbid(unsafe_code)]

//! Channel gateway framework for AnimaOS — Epic E10 (Presence).
//!
//! # Architecture
//!
//! The human is already modelled as a *sense* in AnimaOS: inbound signals
//! flow through [`senses::SensoryBridge`] and the somatic loop drains them.
//! A comms channel is therefore **just another operator transport**:
//!
//! ```text
//!  Telegram / Slack / Discord / email
//!         │  (text · image · voice)         ▲ (text · image · voice)
//!         ▼                                 │
//!  anima-comms ── adapter ──► SensoryBridge  │  OperatorEvent (audit tail)
//! ```
//!
//! This crate provides:
//! - [`ChannelAdapter`] — the per-channel driver trait.
//! - [`ChannelGateway`] — orchestrates adapters ↔ bridge.
//! - [`adapters`] — fixture-backed Telegram and Slack adapters (CI-safe).
//! - [`voice`] — [`voice::SttProvider`] and [`voice::TtsProvider`] traits +
//!   fixture impls.
//! - [`routing`] — [`routing::ModalityRouter`] for S10.5 modality-aware routing
//!   and presence.
//!
//! Inbound channel messages are packetised via the existing
//! [`senses::SensoryBridge`] policy checks before entering the agent; no
//! lifetime or priority bypass is possible.  Outbound messages are produced
//! from the agent's `OperatorEvent` stream — the channel never touches `vita`
//! directly.

pub mod adapters;
pub mod routing;
pub mod voice;

use routing::{
    DeliveryPreference, ModalityCapability, ModalityRouter, OutboundContext, OutboundPlan,
    RouteAudit, RouteDecision,
};
use senses::{SensoryBridge, SensoryBridgeError, SensoryPriority};
use voice::SttProvider;

// ── Modality ──────────────────────────────────────────────────────────────────

/// The content type of a channel message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modality {
    /// Plain or rich text.
    Text,
    /// A raster image (JPEG, PNG, WebP, …).
    Image,
    /// PCM or compressed audio.
    Voice,
}

impl Modality {
    /// Returns a human-readable label used in audit entries.
    pub fn as_str(&self) -> &'static str {
        match self {
            Modality::Text => "text",
            Modality::Image => "image",
            Modality::Voice => "voice",
        }
    }
}

// ── Channel message content ───────────────────────────────────────────────────

/// Payload carried by an inbound channel message.
#[derive(Debug, Clone, PartialEq)]
pub enum ChannelContent {
    /// Plain text body.
    Text(String),
    /// Raw image bytes with MIME type and optional caption.
    Image {
        bytes: Vec<u8>,
        mime: String,
        caption: Option<String>,
    },
    /// PCM audio (16-bit little-endian samples at 16 kHz).
    Voice(Vec<i16>),
}

impl ChannelContent {
    /// Returns the [`Modality`] of this content.
    pub fn modality(&self) -> Modality {
        match self {
            ChannelContent::Text(_) => Modality::Text,
            ChannelContent::Image { .. } => Modality::Image,
            ChannelContent::Voice(_) => Modality::Voice,
        }
    }
}

/// An inbound message received from a channel.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelMessage {
    /// Human-readable sender identifier (e.g. Telegram user id, Slack user id).
    pub from: String,
    /// Message content — text, image, or voice.
    pub content: ChannelContent,
}

/// A message to be sent back out through a channel.
#[derive(Debug, Clone, PartialEq)]
pub struct OutboundMessage {
    /// Recipient identifier on the target channel.
    pub to: String,
    /// Content to send.
    pub content: ChannelContent,
}

// ── Channel errors ────────────────────────────────────────────────────────────

/// Errors that can occur within a channel adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelError {
    /// No live connection is configured; live mode must be enabled explicitly.
    LiveModeNotEnabled,
    /// The channel's API returned an error.
    ApiError(String),
    /// The message content type is not supported by this channel/adapter.
    ModalityUnsupported { modality: &'static str },
    /// Outbound send failed for a transient reason; may be retried.
    TransientFailure(String),
}

impl std::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChannelError::LiveModeNotEnabled => write!(f, "live mode not enabled for this adapter"),
            ChannelError::ApiError(e) => write!(f, "channel API error: {e}"),
            ChannelError::ModalityUnsupported { modality } => {
                write!(f, "modality {modality:?} not supported by this adapter")
            }
            ChannelError::TransientFailure(e) => write!(f, "transient failure: {e}"),
        }
    }
}

// ── ChannelAdapter trait ──────────────────────────────────────────────────────

/// A channel driver that bridges one comms platform to the sensory bridge.
///
/// Implementations are expected to be **CI-safe by default**: returning
/// `None` from `receive` and `Err(LiveModeNotEnabled)` from `send` unless
/// explicitly configured for live network access (via a feature flag or
/// an env-gated token).
///
/// # Safety invariant
///
/// Implementations **must not** call the `SensoryBridge` or `vita` APIs
/// directly.  All inbound data flows through
/// [`ChannelGateway::run_once`] which applies policy checks via
/// `SensoryBridge::packetize_*_checked` before enqueueing.
pub trait ChannelAdapter: Send + Sync {
    /// Stable identifier used in audit entries (e.g. `"telegram"`, `"slack"`).
    fn id(&self) -> &str;

    /// Polls for the next inbound message, if any.
    ///
    /// Returns `None` when no message is available or the adapter is in
    /// fixture-replay mode and the fixture is exhausted.
    fn receive(&self) -> Option<ChannelMessage>;

    /// Sends `msg` to the channel.
    ///
    /// Fixture adapters return `Err(LiveModeNotEnabled)` unless explicitly
    /// configured with a real token.
    fn send(&self, msg: &OutboundMessage) -> Result<(), ChannelError>;

    /// Returns `true` when the adapter has live network connectivity.
    ///
    /// Used by [`ChannelGateway`] to decide whether to attempt outbound
    /// sends and to populate the `is_live` field of the gateway status.
    fn is_live(&self) -> bool {
        false
    }
}

// ── GatewayConfig ─────────────────────────────────────────────────────────────

/// Runtime configuration for [`ChannelGateway`].
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    /// Priority to assign to inbound text messages.
    pub text_priority: SensoryPriority,
    /// Priority to assign to inbound image messages.
    pub image_priority: SensoryPriority,
    /// Priority to assign to inbound voice messages.
    pub voice_priority: SensoryPriority,
    /// Maximum image size in bytes accepted from channel input.
    ///
    /// Mirrors `senses::HumanGuidance::max_image_bytes` but enforced at the
    /// gateway level before the sensory bridge even sees the payload.
    pub max_image_bytes: usize,
    /// Maximum PCM frame length in samples accepted from channel voice input.
    pub max_pcm_samples: usize,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            text_priority: SensoryPriority::Normal,
            image_priority: SensoryPriority::Normal,
            voice_priority: SensoryPriority::Normal,
            max_image_bytes: 10 * 1024 * 1024, // 10 MiB
            max_pcm_samples: 16_000 * 60,      // 60 s at 16 kHz
        }
    }
}

// ── ChannelGateway ────────────────────────────────────────────────────────────

/// Outcome of a single [`ChannelGateway::run_once`] call.
#[derive(Debug, Clone, PartialEq)]
pub struct PollOutcome {
    /// Channel adapter that produced the event (or `"none"` if none fired).
    pub channel_id: String,
    /// Sender on the channel.
    pub from: String,
    /// Modality of the inbound message.
    pub modality: Modality,
    /// `true` if the packet was successfully enqueued in the sensory bridge.
    pub enqueued: bool,
    /// `Some` if a policy or size check rejected the packet.
    pub rejection: Option<String>,
}

/// Orchestrates one or more [`ChannelAdapter`]s against a shared
/// [`SensoryBridge`].
///
/// The gateway is the sole bridge between external comms channels and the
/// agent's somatic loop.  It enforces size/policy limits before forwarding to
/// `SensoryBridge` and records outcomes for audit.
pub struct ChannelGateway {
    adapters: Vec<Box<dyn ChannelAdapter>>,
    bridge: SensoryBridge,
    config: GatewayConfig,
}

impl ChannelGateway {
    /// Creates a new gateway with the given adapters and bridge.
    pub fn new(
        adapters: Vec<Box<dyn ChannelAdapter>>,
        bridge: SensoryBridge,
        config: GatewayConfig,
    ) -> Self {
        Self {
            adapters,
            bridge,
            config,
        }
    }

    /// Returns a reference to the underlying sensory bridge.
    pub fn bridge(&self) -> &SensoryBridge {
        &self.bridge
    }

    /// Returns the number of registered adapters.
    pub fn adapter_count(&self) -> usize {
        self.adapters.len()
    }

    /// Polls all adapters once and forwards any incoming messages to the
    /// sensory bridge after applying size and policy checks.
    ///
    /// The returned `Vec<PollOutcome>` has one entry per message received
    /// across all adapters.  When no adapters have pending messages an empty
    /// `Vec` is returned.
    pub fn run_once(&self) -> Vec<PollOutcome> {
        let mut outcomes = Vec::new();

        for adapter in &self.adapters {
            let mut count = 0usize;
            while let Some(msg) = adapter.receive() {
                let outcome = self.ingest(adapter.id(), msg);
                outcomes.push(outcome);
                count += 1;
                if count >= 50 {
                    break;
                }
            }
        }

        outcomes
    }

    fn ingest(&self, channel_id: &str, msg: ChannelMessage) -> PollOutcome {
        let modality = msg.content.modality();

        match msg.content {
            ChannelContent::Text(text) => {
                let result = self
                    .bridge
                    .packetize_text_checked(&text, self.config.text_priority);
                Self::outcome_from_result(channel_id, &msg.from, modality, result)
            }
            ChannelContent::Image {
                bytes,
                mime,
                caption,
            } => {
                if bytes.len() > self.config.max_image_bytes {
                    return PollOutcome {
                        channel_id: channel_id.to_string(),
                        from: msg.from.clone(),
                        modality,
                        enqueued: false,
                        rejection: Some(format!(
                            "image size {} B exceeds gateway limit {} B",
                            bytes.len(),
                            self.config.max_image_bytes
                        )),
                    };
                }
                let result = self.bridge.packetize_image_checked(
                    bytes,
                    mime,
                    caption,
                    self.config.image_priority,
                );
                Self::outcome_from_result(channel_id, &msg.from, modality, result)
            }
            ChannelContent::Voice(samples) => {
                if samples.len() > self.config.max_pcm_samples {
                    return PollOutcome {
                        channel_id: channel_id.to_string(),
                        from: msg.from.clone(),
                        modality,
                        enqueued: false,
                        rejection: Some(format!(
                            "voice frame {} samples exceeds gateway limit {} samples",
                            samples.len(),
                            self.config.max_pcm_samples
                        )),
                    };
                }
                let result = self
                    .bridge
                    .packetize_pcm_checked(samples, self.config.voice_priority);
                Self::outcome_from_result(channel_id, &msg.from, modality, result)
            }
        }
    }

    fn outcome_from_result(
        channel_id: &str,
        from: &str,
        modality: Modality,
        result: Result<(), SensoryBridgeError>,
    ) -> PollOutcome {
        match result {
            Ok(()) => PollOutcome {
                channel_id: channel_id.to_string(),
                from: from.to_string(),
                modality,
                enqueued: true,
                rejection: None,
            },
            Err(SensoryBridgeError::PolicyViolation { reason }) => PollOutcome {
                channel_id: channel_id.to_string(),
                from: from.to_string(),
                modality,
                enqueued: false,
                rejection: Some(reason),
            },
            Err(SensoryBridgeError::InvalidInput) => PollOutcome {
                channel_id: channel_id.to_string(),
                from: from.to_string(),
                modality,
                enqueued: false,
                rejection: Some("invalid input".to_string()),
            },
        }
    }
}

// ── Modality-aware routing & presence (E10 S10.5) ──────────────────────────────

/// Outcome of a single modality-aware inbound poll
/// ([`ChannelGateway::run_once_routed`]).
///
/// Carries both the base [`PollOutcome`] (size/policy checks + bridge result)
/// and the S10.5 [`RouteDecision`] / [`RouteAudit`] so the host can record the
/// correct `vita::AuditEntry` without `comms` depending on `vita`.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutedOutcome {
    /// The routing decision for the message's modality given backend capabilities.
    pub decision: RouteDecision,
    /// The audit record the host should persist
    /// (`ChannelMessageReceived` / `ModalityUnsupported`).
    pub audit: RouteAudit,
    /// The base size/policy/bridge outcome, present only when the message was
    /// routed (directly, via caption fallback, or via STT) and therefore
    /// actually enqueued in the sensory bridge. `None` for unsupported
    /// modalities, which are dropped before reaching the bridge.
    pub poll: Option<PollOutcome>,
}

impl ChannelGateway {
    /// Polls all adapters once with **modality-aware routing** (S10.5).
    ///
    /// For each inbound message the [`ModalityRouter`] first decides whether the
    /// target backend (described by `caps`) can serve the modality:
    ///
    /// - **Text** is enqueued as today.
    /// - **Image** is enqueued when `caps.vision`; otherwise its caption (if
    ///   any) is enqueued as text, and a caption-less image is dropped with a
    ///   `ModalityUnsupported` audit record.
    /// - **Voice** is transcribed by `stt` *before* routing (STT-before-route)
    ///   and the transcript enqueued as text; without STT the voice is dropped.
    ///
    /// Each routed message yields a `ChannelMessageReceived` audit record; each
    /// dropped one yields a `ModalityUnsupported` record. This method is purely
    /// additive — it does not change [`run_once`](Self::run_once).
    pub fn run_once_routed(
        &self,
        router: &ModalityRouter,
        caps: &ModalityCapability,
        stt: &dyn SttProvider,
    ) -> Vec<RoutedOutcome> {
        let mut outcomes = Vec::new();

        for adapter in &self.adapters {
            let mut count = 0usize;
            while let Some(msg) = adapter.receive() {
                outcomes.push(self.ingest_routed(adapter.id(), msg, router, caps, stt));
                count += 1;
                if count >= 50 {
                    break;
                }
            }
        }

        outcomes
    }

    fn ingest_routed(
        &self,
        channel_id: &str,
        msg: ChannelMessage,
        router: &ModalityRouter,
        caps: &ModalityCapability,
        stt: &dyn SttProvider,
    ) -> RoutedOutcome {
        let decision = router.route_inbound(&msg.content, caps, stt);
        let audit = ModalityRouter::inbound_audit(channel_id, &msg.from, &decision);

        // Map the decision onto an actual bridge ingest. Routed text/image go
        // through the existing size/policy-checked path; transcoded modalities
        // (caption fallback, STT transcript) are ingested as text.
        let poll = match &decision {
            RouteDecision::Routed(Modality::Text) => Some(self.ingest(channel_id, msg)),
            RouteDecision::Routed(Modality::Image) => Some(self.ingest(channel_id, msg)),
            RouteDecision::Routed(Modality::Voice) => {
                // Unreachable in practice: voice always routes via STT below.
                // Kept exhaustive so a future direct-voice route still ingests.
                Some(self.ingest(channel_id, msg))
            }
            RouteDecision::RoutedFallback { text, .. }
            | RouteDecision::RoutedViaStt { transcript: text } => {
                let routed = ChannelMessage {
                    from: msg.from.clone(),
                    content: ChannelContent::Text(text.clone()),
                };
                Some(self.ingest(channel_id, routed))
            }
            RouteDecision::Unsupported { .. } => None,
        };

        RoutedOutcome {
            decision,
            audit,
            poll,
        }
    }

    /// Sends an outbound reply, choosing its modality via the *presence* policy
    /// (S10.5) and returning the `ChannelMessageSent` audit record on success.
    ///
    /// `pref` expresses whether the reply should be spoken; when
    /// [`DeliveryPreference::Voice`] is requested and the context's `caps.tts` is
    /// set, the context's TTS provider renders the text to a voice note,
    /// otherwise it degrades to text. The chosen content is handed to the named
    /// adapter's `send()`.
    ///
    /// The router, route capabilities, and TTS provider are bundled into
    /// [`OutboundContext`] so the call site stays compact.
    ///
    /// Returns `Err(ChannelError)` if no adapter with `channel_id` is registered
    /// or the adapter's `send()` fails (e.g. fixture mode); on the error path no
    /// audit record is produced because nothing was sent.
    pub fn send_routed(
        &self,
        ctx: OutboundContext<'_>,
        channel_id: &str,
        to: &str,
        text: &str,
        pref: DeliveryPreference,
    ) -> Result<RouteAudit, ChannelError> {
        let adapter = self
            .adapters
            .iter()
            .find(|a| a.id() == channel_id)
            .ok_or_else(|| {
                ChannelError::ApiError(format!("no adapter registered for channel {channel_id:?}"))
            })?;

        let plan: OutboundPlan = ctx.plan(text, pref);
        let audit = ModalityRouter::outbound_audit(channel_id, to, &plan);

        let outbound = OutboundMessage {
            to: to.to_string(),
            content: plan.into_content(),
        };
        adapter.send(&outbound)?;

        Ok(audit)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::{FixtureMessage, SlackAdapter, TelegramAdapter};
    use senses::HumanGuidance;

    fn make_bridge() -> SensoryBridge {
        SensoryBridge::new(HumanGuidance::new("test"))
    }

    fn make_gateway(adapters: Vec<Box<dyn ChannelAdapter>>) -> ChannelGateway {
        ChannelGateway::new(adapters, make_bridge(), GatewayConfig::default())
    }

    // ── Modality helpers ─────────────────────────────────────────────────────

    #[test]
    fn modality_as_str_returns_stable_labels() {
        assert_eq!(Modality::Text.as_str(), "text");
        assert_eq!(Modality::Image.as_str(), "image");
        assert_eq!(Modality::Voice.as_str(), "voice");
    }

    #[test]
    fn channel_content_modality_matches_variant() {
        assert_eq!(ChannelContent::Text("hi".into()).modality(), Modality::Text);
        assert_eq!(
            ChannelContent::Image {
                bytes: vec![1],
                mime: "image/png".into(),
                caption: None,
            }
            .modality(),
            Modality::Image
        );
        assert_eq!(ChannelContent::Voice(vec![0]).modality(), Modality::Voice);
    }

    // ── Gateway config defaults ───────────────────────────────────────────────

    #[test]
    fn gateway_config_default_has_sensible_limits() {
        let cfg = GatewayConfig::default();
        assert_eq!(cfg.max_image_bytes, 10 * 1024 * 1024);
        assert_eq!(cfg.max_pcm_samples, 16_000 * 60);
        assert_eq!(cfg.text_priority, SensoryPriority::Normal);
    }

    // ── run_once with Telegram fixture ────────────────────────────────────────

    #[test]
    fn run_once_enqueues_text_message_from_telegram() {
        let adapter = TelegramAdapter::with_fixture(vec![FixtureMessage {
            from: "user42".into(),
            content: ChannelContent::Text("hello from telegram".into()),
        }]);
        let gw = make_gateway(vec![Box::new(adapter)]);

        let outcomes = gw.run_once();
        assert_eq!(outcomes.len(), 1);
        let o = &outcomes[0];
        assert_eq!(o.channel_id, "telegram");
        assert_eq!(o.from, "user42");
        assert_eq!(o.modality, Modality::Text);
        assert!(o.enqueued, "text message should be enqueued");
        assert!(o.rejection.is_none());
        assert!(gw.bridge().has_packets());
    }

    #[test]
    fn run_once_enqueues_image_message_from_slack() {
        let adapter = SlackAdapter::with_fixture(vec![FixtureMessage {
            from: "alice".into(),
            content: ChannelContent::Image {
                bytes: vec![0xFF, 0xD8, 0xFF], // minimal JPEG header
                mime: "image/jpeg".into(),
                caption: Some("screenshot".into()),
            },
        }]);
        let gw = make_gateway(vec![Box::new(adapter)]);

        let outcomes = gw.run_once();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].modality, Modality::Image);
        assert!(outcomes[0].enqueued);
    }

    #[test]
    fn run_once_enqueues_voice_message() {
        let samples: Vec<i16> = vec![0i16; 100];
        let adapter = TelegramAdapter::with_fixture(vec![FixtureMessage {
            from: "speaker".into(),
            content: ChannelContent::Voice(samples),
        }]);
        let gw = make_gateway(vec![Box::new(adapter)]);

        let outcomes = gw.run_once();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].modality, Modality::Voice);
        assert!(outcomes[0].enqueued);
    }

    #[test]
    fn run_once_rejects_oversized_image() {
        let config = GatewayConfig {
            max_image_bytes: 4,
            ..Default::default()
        };
        let adapter = TelegramAdapter::with_fixture(vec![FixtureMessage {
            from: "bob".into(),
            content: ChannelContent::Image {
                bytes: vec![0u8; 5],
                mime: "image/png".into(),
                caption: None,
            },
        }]);
        let gw = ChannelGateway::new(vec![Box::new(adapter)], make_bridge(), config);

        let outcomes = gw.run_once();
        assert_eq!(outcomes.len(), 1);
        assert!(!outcomes[0].enqueued);
        assert!(outcomes[0].rejection.is_some());
    }

    #[test]
    fn run_once_rejects_oversized_voice_frame() {
        let config = GatewayConfig {
            max_pcm_samples: 3,
            ..Default::default()
        };
        let adapter = TelegramAdapter::with_fixture(vec![FixtureMessage {
            from: "speaker".into(),
            content: ChannelContent::Voice(vec![0i16; 4]),
        }]);
        let gw = ChannelGateway::new(vec![Box::new(adapter)], make_bridge(), config);

        let outcomes = gw.run_once();
        assert!(!outcomes[0].enqueued);
        assert!(outcomes[0].rejection.is_some());
    }

    #[test]
    fn run_once_returns_empty_when_no_messages() {
        let adapter = TelegramAdapter::with_fixture(vec![]);
        let gw = make_gateway(vec![Box::new(adapter)]);
        assert!(gw.run_once().is_empty());
    }

    #[test]
    fn adapter_count_matches_registered_adapters() {
        let gw = make_gateway(vec![
            Box::new(TelegramAdapter::with_fixture(vec![])),
            Box::new(SlackAdapter::with_fixture(vec![])),
        ]);
        assert_eq!(gw.adapter_count(), 2);
    }

    #[test]
    fn run_once_drains_all_fixture_messages_in_order() {
        let adapter = TelegramAdapter::with_fixture(vec![
            FixtureMessage {
                from: "u1".into(),
                content: ChannelContent::Text("first".into()),
            },
            FixtureMessage {
                from: "u2".into(),
                content: ChannelContent::Text("second".into()),
            },
        ]);
        let gw = make_gateway(vec![Box::new(adapter)]);
        let outcomes = gw.run_once();
        assert_eq!(outcomes.len(), 2);
        assert_eq!(outcomes[0].from, "u1");
        assert_eq!(outcomes[1].from, "u2");
        // Second run: fixture exhausted
        assert!(gw.run_once().is_empty());
    }

    #[test]
    fn run_once_rejects_empty_text_via_policy_bounds() {
        let adapter = TelegramAdapter::with_fixture(vec![FixtureMessage {
            from: "bot".into(),
            content: ChannelContent::Text("".into()),
        }]);
        let gw = make_gateway(vec![Box::new(adapter)]);
        let outcomes = gw.run_once();
        assert!(!outcomes[0].enqueued);
        assert!(outcomes[0].rejection.is_some());
    }

    // ── Modality-aware routing & presence (S10.5) ─────────────────────────────

    use crate::routing::{
        DeliveryPreference, ModalityCapability, ModalityRouter, OutboundContext, RouteAuditKind,
        RouteDecision,
    };
    use crate::voice::{FixtureStt, FixtureTts};

    #[test]
    fn routed_text_always_enqueues_and_records_received() {
        let adapter = TelegramAdapter::with_fixture(vec![FixtureMessage {
            from: "user42".into(),
            content: ChannelContent::Text("hello".into()),
        }]);
        let gw = make_gateway(vec![Box::new(adapter)]);

        let outcomes = gw.run_once_routed(
            &ModalityRouter::new(),
            &ModalityCapability::text_only(),
            &FixtureStt::new(),
        );
        assert_eq!(outcomes.len(), 1);
        let o = &outcomes[0];
        assert_eq!(o.decision, RouteDecision::Routed(Modality::Text));
        assert_eq!(o.audit.kind, RouteAuditKind::Received);
        assert_eq!(o.audit.modality, "text");
        assert_eq!(o.audit.peer, "user42");
        assert!(o.poll.as_ref().unwrap().enqueued);
        assert!(gw.bridge().has_packets());
    }

    #[test]
    fn routed_image_to_vision_backend_enqueues_and_records_received() {
        let adapter = SlackAdapter::with_fixture(vec![FixtureMessage {
            from: "alice".into(),
            content: ChannelContent::Image {
                bytes: vec![0xFF, 0xD8, 0xFF],
                mime: "image/jpeg".into(),
                caption: Some("screenshot".into()),
            },
        }]);
        let gw = make_gateway(vec![Box::new(adapter)]);

        let outcomes = gw.run_once_routed(
            &ModalityRouter::new(),
            &ModalityCapability::from_vision(true),
            &FixtureStt::new(),
        );
        assert_eq!(outcomes.len(), 1);
        let o = &outcomes[0];
        assert_eq!(o.decision, RouteDecision::Routed(Modality::Image));
        assert_eq!(o.audit.kind, RouteAuditKind::Received);
        assert_eq!(o.audit.modality, "image");
        assert!(
            o.poll.as_ref().unwrap().enqueued,
            "image should be enqueued"
        );
        assert!(gw.bridge().has_packets());
    }

    #[test]
    fn routed_image_to_non_vision_backend_falls_back_to_caption() {
        let adapter = SlackAdapter::with_fixture(vec![FixtureMessage {
            from: "alice".into(),
            content: ChannelContent::Image {
                bytes: vec![0xFF, 0xD8, 0xFF],
                mime: "image/jpeg".into(),
                caption: Some("an error dialog".into()),
            },
        }]);
        let gw = make_gateway(vec![Box::new(adapter)]);

        let outcomes = gw.run_once_routed(
            &ModalityRouter::new(),
            &ModalityCapability::from_vision(false),
            &FixtureStt::new(),
        );
        assert_eq!(outcomes.len(), 1);
        let o = &outcomes[0];
        assert_eq!(
            o.decision,
            RouteDecision::RoutedFallback {
                original: Modality::Image,
                text: "an error dialog".into(),
            }
        );
        // The fallback caption is admitted to the bridge as text.
        assert_eq!(o.audit.kind, RouteAuditKind::Received);
        assert_eq!(o.audit.modality, "text");
        assert!(o.poll.as_ref().unwrap().enqueued);
        assert!(gw.bridge().has_packets());
    }

    #[test]
    fn routed_image_to_non_vision_backend_without_caption_is_unsupported() {
        let adapter = SlackAdapter::with_fixture(vec![FixtureMessage {
            from: "alice".into(),
            content: ChannelContent::Image {
                bytes: vec![0xFF, 0xD8, 0xFF],
                mime: "image/jpeg".into(),
                caption: None,
            },
        }]);
        let gw = make_gateway(vec![Box::new(adapter)]);

        let outcomes = gw.run_once_routed(
            &ModalityRouter::new(),
            &ModalityCapability::from_vision(false),
            &FixtureStt::new(),
        );
        assert_eq!(outcomes.len(), 1);
        let o = &outcomes[0];
        assert!(matches!(
            o.decision,
            RouteDecision::Unsupported {
                modality: Modality::Image,
                ..
            }
        ));
        assert_eq!(o.audit.kind, RouteAuditKind::Unsupported);
        assert_eq!(o.audit.modality, "image");
        // Dropped before the bridge: no poll outcome, nothing enqueued.
        assert!(o.poll.is_none());
        assert!(!gw.bridge().has_packets());
    }

    #[test]
    fn routed_voice_triggers_stt_before_enqueue() {
        let samples = vec![1i16, 2, 3];
        let adapter = TelegramAdapter::with_fixture(vec![FixtureMessage {
            from: "speaker".into(),
            content: ChannelContent::Voice(samples),
        }]);
        let gw = make_gateway(vec![Box::new(adapter)]);

        let stt = FixtureStt::new().with_transcript(3, "transcribed words");
        let caps = ModalityCapability::from_vision(false).with_stt(true);
        let outcomes = gw.run_once_routed(&ModalityRouter::new(), &caps, &stt);

        assert_eq!(outcomes.len(), 1);
        let o = &outcomes[0];
        assert_eq!(
            o.decision,
            RouteDecision::RoutedViaStt {
                transcript: "transcribed words".into(),
            }
        );
        // STT output is admitted to the bridge as text.
        assert_eq!(o.audit.kind, RouteAuditKind::Received);
        assert_eq!(o.audit.modality, "text");
        assert!(o.poll.as_ref().unwrap().enqueued);
        assert!(gw.bridge().has_packets());
    }

    #[test]
    fn routed_voice_without_stt_is_unsupported_and_not_enqueued() {
        let adapter = TelegramAdapter::with_fixture(vec![FixtureMessage {
            from: "speaker".into(),
            content: ChannelContent::Voice(vec![0i16; 10]),
        }]);
        let gw = make_gateway(vec![Box::new(adapter)]);

        let outcomes = gw.run_once_routed(
            &ModalityRouter::new(),
            &ModalityCapability::from_vision(false), // stt = false
            &FixtureStt::new(),
        );
        let o = &outcomes[0];
        assert!(matches!(
            o.decision,
            RouteDecision::Unsupported {
                modality: Modality::Voice,
                ..
            }
        ));
        assert_eq!(o.audit.kind, RouteAuditKind::Unsupported);
        assert!(o.poll.is_none());
        assert!(!gw.bridge().has_packets());
    }

    #[test]
    fn send_routed_text_records_sent_but_fixture_adapter_blocks_send() {
        // Fixture adapters return LiveModeNotEnabled, so send_routed surfaces the
        // adapter error (no audit on the error path).
        let gw = make_gateway(vec![Box::new(TelegramAdapter::with_fixture(vec![]))]);
        let router = ModalityRouter::new();
        let caps = ModalityCapability::text_only();
        let tts = FixtureTts::new();
        let result = gw.send_routed(
            OutboundContext::new(&router, &caps, &tts),
            "telegram",
            "alice",
            "reply",
            DeliveryPreference::Text,
        );
        assert_eq!(result, Err(ChannelError::LiveModeNotEnabled));
    }

    #[test]
    fn send_routed_unknown_channel_is_api_error() {
        let gw = make_gateway(vec![Box::new(TelegramAdapter::with_fixture(vec![]))]);
        let router = ModalityRouter::new();
        let caps = ModalityCapability::text_only();
        let tts = FixtureTts::new();
        let result = gw.send_routed(
            OutboundContext::new(&router, &caps, &tts),
            "discord",
            "x",
            "hi",
            DeliveryPreference::Text,
        );
        assert!(matches!(result, Err(ChannelError::ApiError(_))));
    }

    #[test]
    fn send_routed_via_live_adapter_records_sent_audit() {
        // A tiny always-live adapter that accepts sends, so we can assert the
        // ChannelMessageSent audit record on the success path.
        struct LiveEcho;
        impl ChannelAdapter for LiveEcho {
            fn id(&self) -> &str {
                "echo"
            }
            fn receive(&self) -> Option<ChannelMessage> {
                None
            }
            fn send(&self, _msg: &OutboundMessage) -> Result<(), ChannelError> {
                Ok(())
            }
            fn is_live(&self) -> bool {
                true
            }
        }

        let gw = make_gateway(vec![Box::new(LiveEcho)]);
        let router = ModalityRouter::new();
        let tts = FixtureTts::new().with_audio("spoken reply", vec![5i16, 6, 7]);
        let caps = ModalityCapability::full();

        let audit = gw
            .send_routed(
                OutboundContext::new(&router, &caps, &tts),
                "echo",
                "bob",
                "spoken reply",
                DeliveryPreference::Voice,
            )
            .expect("live adapter accepts send");
        assert_eq!(audit.kind, RouteAuditKind::Sent);
        assert_eq!(audit.peer, "bob");
        assert_eq!(audit.modality, "voice");
    }
}
