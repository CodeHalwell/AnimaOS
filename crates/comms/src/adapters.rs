//! Channel adapter implementations for E10 — Presence.
//!
//! All adapters default to **fixture mode** so CI runs fully offline.
//! Live network access requires explicit env-gated configuration (token + URL),
//! and is placed behind the `live` feature flag to prevent accidental egress
//! in test or CI builds.
//!
//! # Egress safety note
//!
//! Every outbound call from a live adapter is screened through the E7 egress
//! guard ([`actuators::egress::EgressGuard`]) before the token is read or any
//! socket is opened — the same SSRF / scheme / blocklist policy the web-search
//! actuator uses.  Live mode is additionally gated by the `ANIMA_COMMS_LIVE`
//! env-var (runtime) and the `live` Cargo feature (build-time), so the default
//! and CI builds never link an HTTP client or reach the network.
//!
//! Live sends require a per-channel bot token in the environment:
//! `ANIMA_TELEGRAM_TOKEN` for Telegram, `ANIMA_SLACK_TOKEN` for Slack.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use actuators::egress::{EgressGuard, EgressVerdict};

use crate::{ChannelAdapter, ChannelContent, ChannelError, ChannelMessage, OutboundMessage};

/// Telegram Bot API origin (token is appended to the path at send time).
const TELEGRAM_API_BASE: &str = "https://api.telegram.org";
/// Slack Web API origin.
const SLACK_API_BASE: &str = "https://slack.com";

/// Whether the `ANIMA_COMMS_LIVE` runtime gate enables live channel sends.
///
/// Parsed as a boolean rather than a presence check so an explicit
/// `ANIMA_COMMS_LIVE=0` (or `false`/`off`/empty) keeps egress **off** — a bare
/// presence test would let an operator who sets the var to disable live mode
/// accidentally turn it on. Only `1`/`true`/`yes`/`on` (case-insensitive)
/// enable it, matching the documented `ANIMA_COMMS_LIVE=1`.
fn comms_live_enabled() -> bool {
    std::env::var("ANIMA_COMMS_LIVE")
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// Screen `base_url` through `guard`, mapping a denial to a [`ChannelError`].
///
/// Run before reading any token or opening a socket so a policy violation can
/// never leak credentials or trigger network activity.
fn screen_egress(guard: &EgressGuard, base_url: &str) -> Result<(), ChannelError> {
    match guard.check_url(base_url) {
        EgressVerdict::Allow => Ok(()),
        EgressVerdict::Deny(reason) => Err(ChannelError::ApiError(format!(
            "egress-blocked: {}",
            reason.description()
        ))),
    }
}

/// A process-wide blocking HTTP client shared by every live channel send.
///
/// Building a `reqwest::blocking::Client` per call would force a fresh TCP/TLS
/// handshake on every message; a single lazily-built client keeps the
/// connection pool (Keep-Alive) warm across sends. The build result is cached
/// so a one-time TLS-backend failure surfaces consistently instead of being
/// retried on a hot path. Both channels share one timeout policy.
///
/// Redirects are **disabled**: [`screen_egress`] only vets the original API
/// origin, so following a 30x to another host would open a socket to an
/// unscreened URL and bypass the [`EgressGuard`] guarantee. A redirect instead
/// surfaces as a non-success status the callers already treat as an error.
#[cfg(feature = "live")]
fn shared_http_client() -> Result<&'static reqwest::blocking::Client, ChannelError> {
    static CLIENT: std::sync::OnceLock<Result<reqwest::blocking::Client, String>> =
        std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| ChannelError::TransientFailure(format!("http client build failed: {e}")))
}

/// Extract the plain-text body of an outbound message, or report the modality
/// as unsupported. The live channel paths only render text; image/voice replies
/// would need `sendPhoto` / `sendVoice` equivalents not implemented here.
fn outbound_text(msg: &OutboundMessage) -> Result<&str, ChannelError> {
    match &msg.content {
        ChannelContent::Text(t) => Ok(t.as_str()),
        other => Err(ChannelError::ModalityUnsupported {
            modality: other.modality().as_str(),
        }),
    }
}

// ── Fixture primitives ────────────────────────────────────────────────────────

/// A pre-recorded inbound message used by fixture adapters in tests and CI.
#[derive(Debug, Clone)]
pub struct FixtureMessage {
    /// Simulated sender identifier.
    pub from: String,
    /// Simulated message payload.
    pub content: ChannelContent,
}

/// Shared fixture queue replayed by adapters in test/CI mode.
///
/// Both [`TelegramAdapter`] and [`SlackAdapter`] wrap this so fixture messages
/// are consumed exactly once, matching real-channel semantics (no duplicate
/// delivery).
#[derive(Debug, Clone)]
struct FixtureQueue(Arc<Mutex<VecDeque<FixtureMessage>>>);

impl FixtureQueue {
    fn new(messages: Vec<FixtureMessage>) -> Self {
        Self(Arc::new(Mutex::new(VecDeque::from(messages))))
    }

    fn pop(&self) -> Option<ChannelMessage> {
        self.0
            .lock()
            .expect("poisoned")
            .pop_front()
            .map(|f| ChannelMessage {
                from: f.from,
                content: f.content,
            })
    }

    fn is_empty(&self) -> bool {
        self.0.lock().expect("poisoned").is_empty()
    }
}

// ── TelegramAdapter ───────────────────────────────────────────────────────────

/// Telegram channel adapter.
///
/// # Modes
///
/// | Mode | `is_live()` | `receive()` | `send()` |
/// |---|---|---|---|
/// | Fixture (default) | `false` | Replays `FixtureMessage`s then returns `None` | `Err(LiveModeNotEnabled)` |
/// | Live (`ANIMA_COMMS_LIVE=1`) | `true` | Long-polls Telegram Bot API | HTTP POST to send API |
///
/// Live mode is not active in CI because `ANIMA_COMMS_LIVE` is not set in the
/// workflow environment.
pub struct TelegramAdapter {
    fixture: FixtureQueue,
    live: bool,
    egress_guard: EgressGuard,
}

impl TelegramAdapter {
    /// Creates a fixture-backed adapter that will replay `messages` in order.
    pub fn with_fixture(messages: Vec<FixtureMessage>) -> Self {
        Self {
            fixture: FixtureQueue::new(messages),
            live: comms_live_enabled(),
            egress_guard: EgressGuard::default(),
        }
    }

    /// Replaces the egress guard used to screen the Bot API host on live sends.
    ///
    /// Use this to apply an operator allow-list or extra blocklist entries; the
    /// default guard already enforces HTTPS-only + SSRF protection.
    pub fn with_egress_guard(mut self, guard: EgressGuard) -> Self {
        self.egress_guard = guard;
        self
    }

    /// Returns `true` when the fixture queue has been fully consumed.
    pub fn fixture_exhausted(&self) -> bool {
        self.fixture.is_empty()
    }

    /// POST a text message to the Telegram `sendMessage` endpoint.
    ///
    /// The Bot API host is already egress-screened by the caller. The token is
    /// read from `ANIMA_TELEGRAM_TOKEN` only on the live build; without the
    /// `live` feature this returns an `ApiError` so a misconfigured build can
    /// never silently no-op a send.
    fn post_message(&self, to: &str, text: &str) -> Result<(), ChannelError> {
        #[cfg(feature = "live")]
        {
            let token = std::env::var("ANIMA_TELEGRAM_TOKEN")
                .map_err(|_| ChannelError::ApiError("ANIMA_TELEGRAM_TOKEN not set".into()))?;
            let url = format!("{TELEGRAM_API_BASE}/bot{token}/sendMessage");
            let client = shared_http_client()?;
            let resp = client
                .post(&url)
                .json(&serde_json::json!({ "chat_id": to, "text": text }))
                .send()
                // `reqwest::Error` Display includes the request URL, which embeds
                // the bot token in `/bot{token}/sendMessage`; strip it so the
                // token can never leak into a `ChannelError` or its logs.
                .map_err(|e| {
                    ChannelError::TransientFailure(format!(
                        "telegram send failed: {}",
                        e.without_url()
                    ))
                })?;
            if !resp.status().is_success() {
                return Err(ChannelError::ApiError(format!(
                    "telegram API returned status {}",
                    resp.status()
                )));
            }
            Ok(())
        }
        #[cfg(not(feature = "live"))]
        {
            let _ = (to, text);
            Err(ChannelError::ApiError(
                "comms built without the `live` feature; rebuild with `--features live` \
                 for real Telegram sends"
                    .into(),
            ))
        }
    }
}

impl Default for TelegramAdapter {
    fn default() -> Self {
        Self::with_fixture(vec![])
    }
}

impl ChannelAdapter for TelegramAdapter {
    fn id(&self) -> &str {
        "telegram"
    }

    fn receive(&self) -> Option<ChannelMessage> {
        self.fixture.pop()
    }

    fn send(&self, msg: &OutboundMessage) -> Result<(), ChannelError> {
        if !self.live {
            return Err(ChannelError::LiveModeNotEnabled);
        }
        let text = outbound_text(msg)?;
        // Screen the Bot API host before reading the token or touching the wire.
        screen_egress(&self.egress_guard, TELEGRAM_API_BASE)?;
        self.post_message(&msg.to, text)
    }

    fn is_live(&self) -> bool {
        self.live
    }
}

// ── SlackAdapter ──────────────────────────────────────────────────────────────

/// Slack channel adapter (Events API / Web API).
///
/// # Modes
///
/// Same mode table as [`TelegramAdapter`]: fixture by default, live behind the
/// `ANIMA_COMMS_LIVE` env-var.  Live mode calls Slack's `chat.postMessage`
/// endpoint (outbound) and processes Events API payloads (inbound).
pub struct SlackAdapter {
    fixture: FixtureQueue,
    live: bool,
    egress_guard: EgressGuard,
}

impl SlackAdapter {
    /// Creates a fixture-backed adapter that will replay `messages` in order.
    pub fn with_fixture(messages: Vec<FixtureMessage>) -> Self {
        Self {
            fixture: FixtureQueue::new(messages),
            live: comms_live_enabled(),
            egress_guard: EgressGuard::default(),
        }
    }

    /// Replaces the egress guard used to screen the Slack API host on live sends.
    pub fn with_egress_guard(mut self, guard: EgressGuard) -> Self {
        self.egress_guard = guard;
        self
    }

    /// Returns `true` when the fixture queue has been fully consumed.
    pub fn fixture_exhausted(&self) -> bool {
        self.fixture.is_empty()
    }

    /// POST a text message to Slack's `chat.postMessage` endpoint.
    ///
    /// Slack returns HTTP 200 with `{"ok": false, "error": "..."}` for logical
    /// failures, so the success path also inspects the JSON `ok` field.
    fn post_message(&self, to: &str, text: &str) -> Result<(), ChannelError> {
        #[cfg(feature = "live")]
        {
            let token = std::env::var("ANIMA_SLACK_TOKEN")
                .map_err(|_| ChannelError::ApiError("ANIMA_SLACK_TOKEN not set".into()))?;
            let url = format!("{SLACK_API_BASE}/api/chat.postMessage");
            let client = shared_http_client()?;
            let resp = client
                .post(&url)
                .bearer_auth(token)
                .json(&serde_json::json!({ "channel": to, "text": text }))
                .send()
                // Slack carries the token in the bearer header (not the URL), but
                // strip the URL defensively to keep the same no-credential-leak
                // guarantee as the Telegram path.
                .map_err(|e| {
                    ChannelError::TransientFailure(format!(
                        "slack send failed: {}",
                        e.without_url()
                    ))
                })?;
            if !resp.status().is_success() {
                return Err(ChannelError::ApiError(format!(
                    "slack API returned status {}",
                    resp.status()
                )));
            }
            let body: serde_json::Value = resp.json().map_err(|e| {
                ChannelError::TransientFailure(format!("slack response decode failed: {e}"))
            })?;
            if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
                let err = body
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                return Err(ChannelError::ApiError(format!("slack API error: {err}")));
            }
            Ok(())
        }
        #[cfg(not(feature = "live"))]
        {
            let _ = (to, text);
            Err(ChannelError::ApiError(
                "comms built without the `live` feature; rebuild with `--features live` \
                 for real Slack sends"
                    .into(),
            ))
        }
    }
}

impl Default for SlackAdapter {
    fn default() -> Self {
        Self::with_fixture(vec![])
    }
}

impl ChannelAdapter for SlackAdapter {
    fn id(&self) -> &str {
        "slack"
    }

    fn receive(&self) -> Option<ChannelMessage> {
        self.fixture.pop()
    }

    fn send(&self, msg: &OutboundMessage) -> Result<(), ChannelError> {
        if !self.live {
            return Err(ChannelError::LiveModeNotEnabled);
        }
        let text = outbound_text(msg)?;
        screen_egress(&self.egress_guard, SLACK_API_BASE)?;
        self.post_message(&msg.to, text)
    }

    fn is_live(&self) -> bool {
        self.live
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChannelContent;

    fn text_fixture(from: &str, text: &str) -> FixtureMessage {
        FixtureMessage {
            from: from.into(),
            content: ChannelContent::Text(text.into()),
        }
    }

    // ── TelegramAdapter ──────────────────────────────────────────────────────

    #[test]
    fn telegram_id_is_telegram() {
        assert_eq!(TelegramAdapter::default().id(), "telegram");
    }

    #[test]
    fn telegram_fixture_delivers_messages_in_order() {
        let adapter = TelegramAdapter::with_fixture(vec![
            text_fixture("alice", "hi"),
            text_fixture("bob", "hello"),
        ]);
        let m1 = adapter.receive().unwrap();
        let m2 = adapter.receive().unwrap();
        assert_eq!(m1.from, "alice");
        assert_eq!(m2.from, "bob");
        assert!(adapter.receive().is_none());
    }

    #[test]
    fn telegram_fixture_exhausted_after_all_consumed() {
        let adapter = TelegramAdapter::with_fixture(vec![text_fixture("u", "msg")]);
        assert!(!adapter.fixture_exhausted());
        adapter.receive();
        assert!(adapter.fixture_exhausted());
    }

    #[test]
    fn telegram_send_returns_live_mode_not_enabled_in_fixture_mode() {
        let adapter = TelegramAdapter::default();
        let msg = OutboundMessage {
            to: "alice".into(),
            content: ChannelContent::Text("reply".into()),
        };
        assert_eq!(adapter.send(&msg), Err(ChannelError::LiveModeNotEnabled));
    }

    #[test]
    fn telegram_is_not_live_by_default() {
        assert!(!TelegramAdapter::default().is_live());
    }

    #[test]
    fn telegram_fixture_delivers_image_content() {
        let adapter = TelegramAdapter::with_fixture(vec![FixtureMessage {
            from: "cam".into(),
            content: ChannelContent::Image {
                bytes: vec![0xFF, 0xD8],
                mime: "image/jpeg".into(),
                caption: Some("photo".into()),
            },
        }]);
        let msg = adapter.receive().unwrap();
        assert!(matches!(msg.content, ChannelContent::Image { .. }));
    }

    #[test]
    fn telegram_fixture_delivers_voice_content() {
        let adapter = TelegramAdapter::with_fixture(vec![FixtureMessage {
            from: "voice_user".into(),
            content: ChannelContent::Voice(vec![1i16, 2, 3]),
        }]);
        let msg = adapter.receive().unwrap();
        assert!(matches!(msg.content, ChannelContent::Voice(_)));
    }

    // ── SlackAdapter ─────────────────────────────────────────────────────────

    #[test]
    fn slack_id_is_slack() {
        assert_eq!(SlackAdapter::default().id(), "slack");
    }

    #[test]
    fn slack_fixture_delivers_messages_in_order() {
        let adapter = SlackAdapter::with_fixture(vec![
            text_fixture("carol", "hey"),
            text_fixture("dave", "hi there"),
        ]);
        let m1 = adapter.receive().unwrap();
        let m2 = adapter.receive().unwrap();
        assert_eq!(m1.from, "carol");
        assert_eq!(m2.from, "dave");
        assert!(adapter.receive().is_none());
    }

    #[test]
    fn slack_send_returns_live_mode_not_enabled_in_fixture_mode() {
        let adapter = SlackAdapter::default();
        let msg = OutboundMessage {
            to: "channel".into(),
            content: ChannelContent::Text("update".into()),
        };
        assert_eq!(adapter.send(&msg), Err(ChannelError::LiveModeNotEnabled));
    }

    #[test]
    fn slack_is_not_live_by_default() {
        assert!(!SlackAdapter::default().is_live());
    }

    #[test]
    fn slack_fixture_exhausted_tracks_correctly() {
        let adapter =
            SlackAdapter::with_fixture(vec![text_fixture("u1", "a"), text_fixture("u2", "b")]);
        assert!(!adapter.fixture_exhausted());
        adapter.receive();
        assert!(!adapter.fixture_exhausted());
        adapter.receive();
        assert!(adapter.fixture_exhausted());
    }

    // ── Live send path (egress + modality), hermetic ─────────────────────────
    //
    // These exercise the branches that run *before* any socket is opened, so
    // they pass on the default (non-`live`) build without network access.

    /// A Telegram adapter forced into live mode with a chosen egress guard.
    fn live_telegram(guard: EgressGuard) -> TelegramAdapter {
        TelegramAdapter {
            fixture: FixtureQueue::new(vec![]),
            live: true,
            egress_guard: guard,
        }
    }

    /// A Slack adapter forced into live mode with a chosen egress guard.
    fn live_slack(guard: EgressGuard) -> SlackAdapter {
        SlackAdapter {
            fixture: FixtureQueue::new(vec![]),
            live: true,
            egress_guard: guard,
        }
    }

    fn text_out(to: &str, text: &str) -> OutboundMessage {
        OutboundMessage {
            to: to.into(),
            content: ChannelContent::Text(text.into()),
        }
    }

    #[test]
    fn telegram_live_send_blocked_by_egress_guard() {
        let guard = EgressGuard::default().with_blocklisted_host("api.telegram.org");
        let adapter = live_telegram(guard);
        let err = adapter.send(&text_out("alice", "hi")).unwrap_err();
        match err {
            ChannelError::ApiError(msg) => assert!(msg.contains("egress-blocked"), "got: {msg}"),
            other => panic!("expected egress ApiError, got {other:?}"),
        }
    }

    #[test]
    fn slack_live_send_blocked_by_egress_guard() {
        let guard = EgressGuard::default().with_blocklisted_host("slack.com");
        let adapter = live_slack(guard);
        let err = adapter.send(&text_out("channel", "update")).unwrap_err();
        match err {
            ChannelError::ApiError(msg) => assert!(msg.contains("egress-blocked"), "got: {msg}"),
            other => panic!("expected egress ApiError, got {other:?}"),
        }
    }

    #[test]
    fn telegram_live_send_rejects_non_text_modality() {
        let adapter = live_telegram(EgressGuard::default());
        let msg = OutboundMessage {
            to: "alice".into(),
            content: ChannelContent::Voice(vec![0i16; 4]),
        };
        assert_eq!(
            adapter.send(&msg),
            Err(ChannelError::ModalityUnsupported { modality: "voice" })
        );
    }

    #[test]
    fn slack_live_send_rejects_non_text_modality() {
        let adapter = live_slack(EgressGuard::default());
        let msg = OutboundMessage {
            to: "channel".into(),
            content: ChannelContent::Image {
                bytes: vec![0xFF, 0xD8],
                mime: "image/jpeg".into(),
                caption: None,
            },
        };
        assert_eq!(
            adapter.send(&msg),
            Err(ChannelError::ModalityUnsupported { modality: "image" })
        );
    }

    /// On the default (non-`live`) build, a fully-allowed live text send reaches
    /// `post_message` and reports the missing build feature rather than
    /// silently succeeding.
    #[cfg(not(feature = "live"))]
    #[test]
    fn telegram_live_text_without_feature_reports_missing_feature() {
        let adapter = live_telegram(EgressGuard::default());
        match adapter.send(&text_out("alice", "hi")).unwrap_err() {
            ChannelError::ApiError(msg) => assert!(msg.contains("live"), "got: {msg}"),
            other => panic!("expected feature ApiError, got {other:?}"),
        }
    }

    // ── FixtureQueue thread-safety ────────────────────────────────────────────

    #[test]
    fn fixture_queue_clone_shares_state() {
        let q = FixtureQueue::new(vec![text_fixture("u", "msg")]);
        let q2 = q.clone();
        // Pop via the clone — the original should now be empty.
        assert!(q2.pop().is_some());
        assert!(q.pop().is_none());
    }
}
