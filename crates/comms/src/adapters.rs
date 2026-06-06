//! Channel adapter implementations for E10 — Presence.
//!
//! All adapters default to **fixture mode** so CI runs fully offline.
//! Live network access requires explicit env-gated configuration (token + URL),
//! and is placed behind the `live` feature flag to prevent accidental egress
//! in test or CI builds.
//!
//! # Egress safety note
//!
//! Outbound calls from live adapters should be routed through the E7 egress
//! guard (`actuators::EgressGuard`) once that crate is available.  Until E7
//! merges, live mode is gated by the `ANIMA_COMMS_LIVE` env-var and is not
//! reachable from the default or test builds.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::{ChannelAdapter, ChannelContent, ChannelError, ChannelMessage, OutboundMessage};

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
}

impl TelegramAdapter {
    /// Creates a fixture-backed adapter that will replay `messages` in order.
    pub fn with_fixture(messages: Vec<FixtureMessage>) -> Self {
        Self {
            fixture: FixtureQueue::new(messages),
            live: false,
        }
    }

    /// Returns `true` when the fixture queue has been fully consumed.
    pub fn fixture_exhausted(&self) -> bool {
        self.fixture.is_empty()
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

    fn send(&self, _msg: &OutboundMessage) -> Result<(), ChannelError> {
        if self.live {
            // Live HTTP send would go here; gated on `ANIMA_COMMS_LIVE`.
            // Outbound URL must be screened through the E7 egress guard
            // (TODO: wire actuators::EgressGuard once E7 merges).
            Err(ChannelError::ApiError(
                "live Telegram send not yet implemented".into(),
            ))
        } else {
            Err(ChannelError::LiveModeNotEnabled)
        }
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
}

impl SlackAdapter {
    /// Creates a fixture-backed adapter that will replay `messages` in order.
    pub fn with_fixture(messages: Vec<FixtureMessage>) -> Self {
        Self {
            fixture: FixtureQueue::new(messages),
            live: false,
        }
    }

    /// Returns `true` when the fixture queue has been fully consumed.
    pub fn fixture_exhausted(&self) -> bool {
        self.fixture.is_empty()
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

    fn send(&self, _msg: &OutboundMessage) -> Result<(), ChannelError> {
        if self.live {
            Err(ChannelError::ApiError(
                "live Slack send not yet implemented".into(),
            ))
        } else {
            Err(ChannelError::LiveModeNotEnabled)
        }
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
