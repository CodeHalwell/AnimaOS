//! Modality-aware routing & presence for E10 — S10.5.
//!
//! The earlier stories (S10.1–S10.4) gave the gateway the ability to *receive*
//! text, image, and voice and to *transcribe*/​*synthesise* audio. What was
//! missing — and was blocked on E8's `BackendCapabilities` — is the decision of
//! **whether the backend that will actually consume a message can serve that
//! message's modality**, and the *presence* notion of choosing the right
//! modality for an outbound reply. That is this module.
//!
//! ```text
//!  inbound modality ──► ModalityRouter::route_inbound ──► RouteDecision
//!      Text                                              Routed(Text)
//!      Image (vision ✓)                                  Routed(Image)
//!      Image (vision ✗, caption present)                 RoutedFallback(caption text)
//!      Image (vision ✗, no caption)                      Unsupported
//!      Voice (stt ✓)                                     RoutedViaStt(transcript)   [STT runs first]
//!      Voice (stt ✗)                                     Unsupported
//! ```
//!
//! # Why a local capability struct instead of `llm-backends::BackendCapabilities`
//!
//! `comms` is intentionally a *leaf* crate: it depends only on `senses`, and the
//! `anima-comms` host process is documented to **not** link `vita` so the human
//! channel stays a *sense*, never a controller. Depending on `llm-backends`
//! would not create a *cycle* (`llm-backends` only depends on `scheduler`, which
//! has no path deps), but it would drag an HTTP client (`ureq`), `scheduler`,
//! and `serde` into the comms gateway purely to read five `bool` flags.
//!
//! Instead we define a minimal [`ModalityCapability`] that the host populates
//! from `BackendCapabilities` at the wiring seam — a trivial field copy:
//!
//! ```ignore
//! // in the host crate, which *does* depend on llm-backends:
//! let caps = ModalityCapability::from_vision(backend_caps.vision)
//!     .with_stt(stt_provider.is_some())
//!     .with_tts(tts_provider.is_some());
//! ```
//!
//! # Why this module does not emit `AuditEntry` directly
//!
//! Same isolation reason: `comms` does not depend on `vita`. The router returns
//! a [`RouteAudit`] value describing *what* should be recorded; the host maps it
//! 1:1 onto the existing `vita::AuditEntry` variants
//! (`ChannelMessageReceived` / `ChannelMessageSent` / `ModalityUnsupported`).
//! This mirrors how [`crate::PollOutcome`] already keeps `vita` out of the
//! gateway. See [`RouteAudit::as_kind`] for the mapping.

use crate::voice::{SttProvider, TtsProvider};
use crate::{ChannelContent, Modality};

// ── ModalityCapability ──────────────────────────────────────────────────────

/// The modality competencies of the backend (and voice stack) a message will be
/// routed to.
///
/// This is the comms-local mirror of the subset of E8's
/// `llm-backends::BackendCapabilities` that routing cares about. The host
/// populates `vision` from `BackendCapabilities::vision`; `stt`/`tts` reflect
/// whether an [`SttProvider`]/[`TtsProvider`] is wired up for the route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModalityCapability {
    /// The backend can consume image inputs directly
    /// (maps from `BackendCapabilities::vision`).
    pub vision: bool,
    /// A speech-to-text provider is available, so inbound voice can be
    /// transcribed to text before routing.
    pub stt: bool,
    /// A text-to-speech provider is available, so outbound text can be rendered
    /// as a voice note.
    pub tts: bool,
}

impl ModalityCapability {
    /// Text-only backend with no voice stack — the safe default.
    pub const fn text_only() -> Self {
        Self {
            vision: false,
            stt: false,
            tts: false,
        }
    }

    /// A backend with every modality competency enabled.
    pub const fn full() -> Self {
        Self {
            vision: true,
            stt: true,
            tts: true,
        }
    }

    /// Builds a capability set from the backend's `vision` flag alone (the field
    /// carried by E8's `BackendCapabilities`), leaving the voice stack off.
    ///
    /// Combine with [`with_stt`](Self::with_stt) / [`with_tts`](Self::with_tts)
    /// to describe the voice providers wired for the route.
    pub const fn from_vision(vision: bool) -> Self {
        Self {
            vision,
            stt: false,
            tts: false,
        }
    }

    /// Returns a copy with the STT (voice-in) competency set to `stt`.
    pub const fn with_stt(mut self, stt: bool) -> Self {
        self.stt = stt;
        self
    }

    /// Returns a copy with the TTS (voice-out) competency set to `tts`.
    pub const fn with_tts(mut self, tts: bool) -> Self {
        self.tts = tts;
        self
    }

    /// `true` when the given inbound `modality` can be served — directly, or via
    /// a transcode/fallback the router knows how to perform.
    ///
    /// Note that an image with no caption is *not* served when `vision` is off
    /// (there is nothing to fall back to); callers wanting that nuance should
    /// inspect the [`RouteDecision`] from [`ModalityRouter::route_inbound`].
    pub fn serves(&self, modality: &Modality) -> bool {
        match modality {
            Modality::Text => true,
            Modality::Image => self.vision,
            Modality::Voice => self.stt,
        }
    }
}

impl Default for ModalityCapability {
    fn default() -> Self {
        Self::text_only()
    }
}

// ── RouteAudit ──────────────────────────────────────────────────────────────

/// The kind of `vita::AuditEntry` a [`RouteAudit`] maps to.
///
/// Lets the host pattern-match without string comparison; the names match the
/// existing audit variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteAuditKind {
    /// → `vita::AuditEntry::ChannelMessageReceived`.
    Received,
    /// → `vita::AuditEntry::ChannelMessageSent`.
    Sent,
    /// → `vita::AuditEntry::ModalityUnsupported`.
    Unsupported,
}

/// An audit record the host should persist via `vita::AuditLog`.
///
/// `comms` deliberately does not depend on `vita` (see module docs), so the
/// router emits this neutral value instead of an `AuditEntry`. The field set is
/// exactly what the three existing audit variants need; mapping is mechanical:
///
/// ```ignore
/// let entry = match audit.as_kind() {
///     RouteAuditKind::Received => AuditEntry::ChannelMessageReceived {
///         agent_id, channel: audit.channel, from: audit.peer, modality: audit.modality.into(),
///     },
///     RouteAuditKind::Sent => AuditEntry::ChannelMessageSent {
///         agent_id, channel: audit.channel, to: audit.peer, modality: audit.modality.into(),
///     },
///     RouteAuditKind::Unsupported => AuditEntry::ModalityUnsupported {
///         agent_id, channel: audit.channel, modality: audit.modality.into(),
///     },
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteAudit {
    /// Which `AuditEntry` variant this maps to.
    pub kind: RouteAuditKind,
    /// Channel adapter id (e.g. `"telegram"`).
    pub channel: String,
    /// The other party: sender for `Received`, recipient for `Sent`. Empty for
    /// `Unsupported` (the `ModalityUnsupported` entry records no peer).
    pub peer: String,
    /// The modality involved, as the stable label used in audit entries
    /// (`"text"`, `"image"`, `"voice"`).
    pub modality: &'static str,
}

impl RouteAudit {
    /// Returns the variant of `vita::AuditEntry` this record maps to.
    pub fn as_kind(&self) -> RouteAuditKind {
        self.kind
    }
}

// ── RouteDecision ───────────────────────────────────────────────────────────

/// The outcome of routing an inbound message's modality to a backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteDecision {
    /// The modality is served directly by the backend; deliver as-is.
    ///
    /// Carries the modality that will be consumed (`Text`/`Image`/`Voice`).
    Routed(Modality),
    /// The original modality is not served, but a graceful fallback was
    /// produced: the carried `String` is the text to route instead (today this
    /// is an image caption when the backend lacks vision).
    RoutedFallback {
        /// What was received but could not be sent natively.
        original: Modality,
        /// The substitute text routed in its place.
        text: String,
    },
    /// Inbound voice was transcribed to text by the STT provider before routing.
    ///
    /// The carried `String` is the transcript; it is delivered as a `Text`
    /// packet tagged (by the caller) with audio provenance.
    RoutedViaStt {
        /// The transcript produced from the PCM frame.
        transcript: String,
    },
    /// The modality cannot be served and no fallback is possible; the message is
    /// dropped and a `ModalityUnsupported` audit entry should be written.
    Unsupported {
        /// The modality that could not be served.
        modality: Modality,
        /// Human-readable explanation (e.g. why STT/vision was unavailable).
        reason: String,
    },
}

impl RouteDecision {
    /// `true` when the message was routed (directly, via fallback, or via STT).
    pub fn is_routed(&self) -> bool {
        !matches!(self, RouteDecision::Unsupported { .. })
    }
}

// ── OutboundPlan ────────────────────────────────────────────────────────────

/// How an outbound reply should be rendered for a channel (the *presence* side).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboundPlan {
    /// Send the reply as text.
    Text(String),
    /// Send the reply as a synthesised voice note (PCM 16-bit LE, 16 kHz).
    Voice(Vec<i16>),
}

impl OutboundPlan {
    /// The modality label of this plan, for audit recording.
    pub fn modality(&self) -> Modality {
        match self {
            OutboundPlan::Text(_) => Modality::Text,
            OutboundPlan::Voice(_) => Modality::Voice,
        }
    }

    /// Converts the plan into the gateway's [`ChannelContent`] for an adapter
    /// `send()`.
    pub fn into_content(self) -> ChannelContent {
        match self {
            OutboundPlan::Text(t) => ChannelContent::Text(t),
            OutboundPlan::Voice(pcm) => ChannelContent::Voice(pcm),
        }
    }
}

/// Whether an outbound reply prefers a spoken delivery (e.g. the inbound message
/// was a voice note, or the channel is a phone bridge).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryPreference {
    /// Deliver as text.
    Text,
    /// Deliver as voice if the route has a TTS provider; otherwise fall back to
    /// text.
    Voice,
}

/// Bundle of the per-route bits needed to plan and send an outbound reply.
///
/// Groups the [`ModalityRouter`], the route's [`ModalityCapability`], and the
/// [`TtsProvider`] so callers (e.g. [`crate::ChannelGateway::send_routed`]) pass
/// one borrowed context instead of three separate arguments.
#[derive(Clone, Copy)]
pub struct OutboundContext<'a> {
    /// The router that decides the outbound modality.
    pub router: &'a ModalityRouter,
    /// The target route's modality competencies (notably `tts`).
    pub caps: &'a ModalityCapability,
    /// The text-to-speech provider used when a voice reply is requested.
    pub tts: &'a dyn TtsProvider,
}

impl<'a> OutboundContext<'a> {
    /// Creates an outbound context from its three parts.
    pub fn new(
        router: &'a ModalityRouter,
        caps: &'a ModalityCapability,
        tts: &'a dyn TtsProvider,
    ) -> Self {
        Self { router, caps, tts }
    }

    /// Plans the reply via [`ModalityRouter::plan_outbound`] using this context.
    pub fn plan(&self, text: &str, pref: DeliveryPreference) -> OutboundPlan {
        self.router.plan_outbound(text, pref, self.caps, self.tts)
    }
}

// ── ModalityRouter ──────────────────────────────────────────────────────────

/// Routes a message's modality to a capable backend, performing STT-before-route
/// for voice and caption fallback for vision-less image routes.
///
/// The router is stateless; it borrows the STT/TTS providers per call so the
/// gateway can share one router across adapters.
#[derive(Debug, Default, Clone, Copy)]
pub struct ModalityRouter;

impl ModalityRouter {
    /// Creates a new router.
    pub fn new() -> Self {
        Self
    }

    /// Decides whether `content`'s modality can be served by a backend with the
    /// given `caps`, using `stt` to transcribe inbound voice when needed.
    ///
    /// - **Text** → always [`RouteDecision::Routed`].
    /// - **Image** → [`RouteDecision::Routed`] when `caps.vision`; otherwise a
    ///   caption (if present) becomes [`RouteDecision::RoutedFallback`], and a
    ///   caption-less image becomes [`RouteDecision::Unsupported`].
    /// - **Voice** → when `caps.stt`, `stt.transcribe` is run **first** and the
    ///   transcript is returned as [`RouteDecision::RoutedViaStt`]; without STT
    ///   the voice is [`RouteDecision::Unsupported`]. A transcription error is
    ///   also reported as `Unsupported` (the router never panics on bad audio).
    pub fn route_inbound(
        &self,
        content: &ChannelContent,
        caps: &ModalityCapability,
        stt: &dyn SttProvider,
    ) -> RouteDecision {
        match content {
            ChannelContent::Text(_) => RouteDecision::Routed(Modality::Text),

            ChannelContent::Image { caption, .. } => {
                if caps.vision {
                    RouteDecision::Routed(Modality::Image)
                } else if let Some(cap) = caption.as_ref().filter(|c| !c.is_empty()) {
                    // Graceful degradation (S10.3): a route without vision can
                    // still act on the caption text the human supplied.
                    RouteDecision::RoutedFallback {
                        original: Modality::Image,
                        text: cap.clone(),
                    }
                } else {
                    RouteDecision::Unsupported {
                        modality: Modality::Image,
                        reason: "backend has no vision capability and image has no caption to \
                                 fall back to"
                            .to_string(),
                    }
                }
            }

            ChannelContent::Voice(samples) => {
                if !caps.stt {
                    return RouteDecision::Unsupported {
                        modality: Modality::Voice,
                        reason: "no speech-to-text provider available to transcribe inbound voice"
                            .to_string(),
                    };
                }
                // STT-before-route: convert PCM → text, then route the text.
                match stt.transcribe(samples) {
                    Ok(transcript) => RouteDecision::RoutedViaStt { transcript },
                    Err(e) => RouteDecision::Unsupported {
                        modality: Modality::Voice,
                        reason: format!("speech-to-text failed: {e}"),
                    },
                }
            }
        }
    }

    /// Plans an outbound reply for a channel given the route's capabilities and
    /// the desired [`DeliveryPreference`] (the *presence* side).
    ///
    /// When `pref` is [`DeliveryPreference::Voice`] and `caps.tts` is set, the
    /// `tts` provider renders `text` to PCM and the plan is
    /// [`OutboundPlan::Voice`]; if synthesis fails or TTS is unavailable the
    /// plan degrades to [`OutboundPlan::Text`] so the reply is never lost.
    pub fn plan_outbound(
        &self,
        text: &str,
        pref: DeliveryPreference,
        caps: &ModalityCapability,
        tts: &dyn TtsProvider,
    ) -> OutboundPlan {
        match pref {
            DeliveryPreference::Text => OutboundPlan::Text(text.to_string()),
            DeliveryPreference::Voice if caps.tts => match tts.synthesise(text) {
                Ok(pcm) => OutboundPlan::Voice(pcm),
                // Degrade to text rather than drop the reply.
                Err(_) => OutboundPlan::Text(text.to_string()),
            },
            DeliveryPreference::Voice => OutboundPlan::Text(text.to_string()),
        }
    }

    /// Builds the [`RouteAudit`] for an inbound routing decision on `channel`
    /// from `from`.
    ///
    /// Returns a `Received` record (with the *effective* modality — `text` when
    /// voice was transcribed or an image fell back to its caption) for routed
    /// messages, or an `Unsupported` record for dropped ones.
    pub fn inbound_audit(channel: &str, from: &str, decision: &RouteDecision) -> RouteAudit {
        match decision {
            RouteDecision::Routed(m) => RouteAudit {
                kind: RouteAuditKind::Received,
                channel: channel.to_string(),
                peer: from.to_string(),
                modality: m.as_str(),
            },
            // The bridge ultimately ingests text in both transcode cases, so the
            // received modality recorded is "text".
            RouteDecision::RoutedFallback { .. } | RouteDecision::RoutedViaStt { .. } => {
                RouteAudit {
                    kind: RouteAuditKind::Received,
                    channel: channel.to_string(),
                    peer: from.to_string(),
                    modality: Modality::Text.as_str(),
                }
            }
            RouteDecision::Unsupported { modality, .. } => RouteAudit {
                kind: RouteAuditKind::Unsupported,
                channel: channel.to_string(),
                // ModalityUnsupported records no peer.
                peer: String::new(),
                modality: modality.as_str(),
            },
        }
    }

    /// Builds the `Sent` [`RouteAudit`] for an outbound `plan` to `to` on
    /// `channel`.
    pub fn outbound_audit(channel: &str, to: &str, plan: &OutboundPlan) -> RouteAudit {
        RouteAudit {
            kind: RouteAuditKind::Sent,
            channel: channel.to_string(),
            peer: to.to_string(),
            modality: plan.modality().as_str(),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::{FixtureStt, FixtureTts};

    fn no_stt() -> FixtureStt {
        FixtureStt::new()
    }

    fn no_tts() -> FixtureTts {
        FixtureTts::new()
    }

    // ── ModalityCapability ────────────────────────────────────────────────────

    #[test]
    fn capability_defaults_to_text_only() {
        let c = ModalityCapability::default();
        assert!(!c.vision && !c.stt && !c.tts);
        assert_eq!(c, ModalityCapability::text_only());
    }

    #[test]
    fn from_vision_maps_only_vision_flag() {
        assert!(ModalityCapability::from_vision(true).vision);
        assert!(!ModalityCapability::from_vision(true).stt);
        assert!(!ModalityCapability::from_vision(false).vision);
    }

    #[test]
    fn builder_sets_stt_and_tts() {
        let c = ModalityCapability::from_vision(false)
            .with_stt(true)
            .with_tts(true);
        assert!(c.stt && c.tts && !c.vision);
    }

    #[test]
    fn serves_matches_modality_competency() {
        let full = ModalityCapability::full();
        assert!(full.serves(&Modality::Text));
        assert!(full.serves(&Modality::Image));
        assert!(full.serves(&Modality::Voice));

        let text_only = ModalityCapability::text_only();
        assert!(text_only.serves(&Modality::Text));
        assert!(!text_only.serves(&Modality::Image));
        assert!(!text_only.serves(&Modality::Voice));
    }

    // ── Text routing ──────────────────────────────────────────────────────────

    #[test]
    fn text_always_routes_even_on_text_only_backend() {
        let router = ModalityRouter::new();
        let decision = router.route_inbound(
            &ChannelContent::Text("hi".into()),
            &ModalityCapability::text_only(),
            &no_stt(),
        );
        assert_eq!(decision, RouteDecision::Routed(Modality::Text));
        assert!(decision.is_routed());
    }

    // ── Image routing ─────────────────────────────────────────────────────────

    #[test]
    fn image_routes_to_vision_capable_backend() {
        let router = ModalityRouter::new();
        let content = ChannelContent::Image {
            bytes: vec![0xFF, 0xD8, 0xFF],
            mime: "image/jpeg".into(),
            caption: Some("a cat".into()),
        };
        let decision =
            router.route_inbound(&content, &ModalityCapability::from_vision(true), &no_stt());
        assert_eq!(decision, RouteDecision::Routed(Modality::Image));
    }

    #[test]
    fn image_without_vision_falls_back_to_caption() {
        let router = ModalityRouter::new();
        let content = ChannelContent::Image {
            bytes: vec![0xFF, 0xD8, 0xFF],
            mime: "image/jpeg".into(),
            caption: Some("a screenshot of an error".into()),
        };
        let decision =
            router.route_inbound(&content, &ModalityCapability::from_vision(false), &no_stt());
        assert_eq!(
            decision,
            RouteDecision::RoutedFallback {
                original: Modality::Image,
                text: "a screenshot of an error".into(),
            }
        );
        assert!(decision.is_routed());
    }

    #[test]
    fn image_without_vision_and_no_caption_is_unsupported() {
        let router = ModalityRouter::new();
        let content = ChannelContent::Image {
            bytes: vec![0xFF, 0xD8, 0xFF],
            mime: "image/jpeg".into(),
            caption: None,
        };
        let decision =
            router.route_inbound(&content, &ModalityCapability::from_vision(false), &no_stt());
        assert!(matches!(
            decision,
            RouteDecision::Unsupported {
                modality: Modality::Image,
                ..
            }
        ));
        assert!(!decision.is_routed());
    }

    #[test]
    fn image_without_vision_and_empty_caption_is_unsupported() {
        let router = ModalityRouter::new();
        let content = ChannelContent::Image {
            bytes: vec![0x1],
            mime: "image/png".into(),
            caption: Some(String::new()),
        };
        let decision =
            router.route_inbound(&content, &ModalityCapability::from_vision(false), &no_stt());
        assert!(matches!(
            decision,
            RouteDecision::Unsupported {
                modality: Modality::Image,
                ..
            }
        ));
    }

    // ── Voice routing (STT-before-route) ──────────────────────────────────────

    #[test]
    fn voice_triggers_stt_before_routing() {
        let router = ModalityRouter::new();
        let stt = FixtureStt::new().with_transcript(3, "hello from voice");
        let caps = ModalityCapability::from_vision(false).with_stt(true);

        let decision = router.route_inbound(&ChannelContent::Voice(vec![1, 2, 3]), &caps, &stt);
        assert_eq!(
            decision,
            RouteDecision::RoutedViaStt {
                transcript: "hello from voice".into(),
            }
        );
    }

    #[test]
    fn voice_without_stt_is_unsupported() {
        let router = ModalityRouter::new();
        let caps = ModalityCapability::from_vision(false); // stt = false
        let decision = router.route_inbound(&ChannelContent::Voice(vec![0; 10]), &caps, &no_stt());
        assert!(matches!(
            decision,
            RouteDecision::Unsupported {
                modality: Modality::Voice,
                ..
            }
        ));
    }

    #[test]
    fn voice_stt_error_is_reported_as_unsupported_not_panic() {
        let router = ModalityRouter::new();
        // FixtureStt with no transcript + no default → transcribe() errors.
        let stt = FixtureStt::new();
        let caps = ModalityCapability::from_vision(false).with_stt(true);
        let decision = router.route_inbound(&ChannelContent::Voice(vec![9, 9]), &caps, &stt);
        match decision {
            RouteDecision::Unsupported { modality, reason } => {
                assert_eq!(modality, Modality::Voice);
                assert!(reason.contains("speech-to-text failed"));
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    // ── Outbound presence planning ────────────────────────────────────────────

    #[test]
    fn outbound_text_preference_plans_text() {
        let router = ModalityRouter::new();
        let plan = router.plan_outbound(
            "reply",
            DeliveryPreference::Text,
            &ModalityCapability::full(),
            &no_tts(),
        );
        assert_eq!(plan, OutboundPlan::Text("reply".into()));
        assert_eq!(plan.modality(), Modality::Text);
    }

    #[test]
    fn outbound_voice_preference_synthesises_when_tts_available() {
        let router = ModalityRouter::new();
        let tts = FixtureTts::new().with_audio("reply", vec![10i16, 20, 30]);
        let caps = ModalityCapability::full(); // tts = true
        let plan = router.plan_outbound("reply", DeliveryPreference::Voice, &caps, &tts);
        assert_eq!(plan, OutboundPlan::Voice(vec![10i16, 20, 30]));
        assert_eq!(plan.modality(), Modality::Voice);
    }

    #[test]
    fn outbound_voice_preference_degrades_to_text_without_tts() {
        let router = ModalityRouter::new();
        let caps = ModalityCapability::from_vision(true); // tts = false
        let plan = router.plan_outbound("reply", DeliveryPreference::Voice, &caps, &no_tts());
        assert_eq!(plan, OutboundPlan::Text("reply".into()));
    }

    #[test]
    fn outbound_voice_preference_degrades_to_text_on_tts_error() {
        let router = ModalityRouter::new();
        let tts = FixtureTts::new(); // no audio, no default → synthesise() errors
        let caps = ModalityCapability::full();
        let plan = router.plan_outbound("reply", DeliveryPreference::Voice, &caps, &tts);
        assert_eq!(plan, OutboundPlan::Text("reply".into()));
    }

    #[test]
    fn outbound_plan_into_content_round_trips() {
        assert_eq!(
            OutboundPlan::Text("x".into()).into_content(),
            ChannelContent::Text("x".into())
        );
        assert_eq!(
            OutboundPlan::Voice(vec![1, 2]).into_content(),
            ChannelContent::Voice(vec![1, 2])
        );
    }

    // ── Audit mapping ─────────────────────────────────────────────────────────

    #[test]
    fn inbound_audit_for_routed_image_is_received_with_image_modality() {
        let decision = RouteDecision::Routed(Modality::Image);
        let audit = ModalityRouter::inbound_audit("telegram", "alice", &decision);
        assert_eq!(audit.kind, RouteAuditKind::Received);
        assert_eq!(audit.channel, "telegram");
        assert_eq!(audit.peer, "alice");
        assert_eq!(audit.modality, "image");
    }

    #[test]
    fn inbound_audit_for_caption_fallback_records_text_modality() {
        let decision = RouteDecision::RoutedFallback {
            original: Modality::Image,
            text: "cap".into(),
        };
        let audit = ModalityRouter::inbound_audit("slack", "bob", &decision);
        assert_eq!(audit.kind, RouteAuditKind::Received);
        assert_eq!(audit.modality, "text");
    }

    #[test]
    fn inbound_audit_for_stt_records_text_modality() {
        let decision = RouteDecision::RoutedViaStt {
            transcript: "spoken".into(),
        };
        let audit = ModalityRouter::inbound_audit("telegram", "carol", &decision);
        assert_eq!(audit.kind, RouteAuditKind::Received);
        assert_eq!(audit.modality, "text");
        assert_eq!(audit.peer, "carol");
    }

    #[test]
    fn inbound_audit_for_unsupported_is_unsupported_with_empty_peer() {
        let decision = RouteDecision::Unsupported {
            modality: Modality::Image,
            reason: "no vision".into(),
        };
        let audit = ModalityRouter::inbound_audit("slack", "dave", &decision);
        assert_eq!(audit.kind, RouteAuditKind::Unsupported);
        assert_eq!(audit.modality, "image");
        assert!(audit.peer.is_empty(), "ModalityUnsupported records no peer");
    }

    #[test]
    fn outbound_audit_is_sent_with_plan_modality() {
        let plan = OutboundPlan::Voice(vec![1, 2, 3]);
        let audit = ModalityRouter::outbound_audit("telegram", "erin", &plan);
        assert_eq!(audit.kind, RouteAuditKind::Sent);
        assert_eq!(audit.peer, "erin");
        assert_eq!(audit.modality, "voice");
        assert_eq!(audit.as_kind(), RouteAuditKind::Sent);
    }
}
