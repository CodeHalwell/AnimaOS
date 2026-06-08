//! Webhook endpoint model: URL, secret, event filter, and enable/disable state.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// The set of events an endpoint subscribes to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventFilter {
    /// Forward every event to this endpoint.
    #[default]
    All,
    /// Forward only events whose `kind` string appears in this set.
    Selected { kinds: HashSet<String> },
}

impl EventFilter {
    /// Returns `true` if this filter passes the given event kind string.
    pub fn matches(&self, kind: &str) -> bool {
        match self {
            EventFilter::All => true,
            EventFilter::Selected { kinds } => kinds.contains(kind),
        }
    }

    /// Construct a filter covering the given set of kind strings.
    pub fn only(kinds: impl IntoIterator<Item = impl Into<String>>) -> Self {
        EventFilter::Selected {
            kinds: kinds.into_iter().map(Into::into).collect(),
        }
    }
}

/// A registered outbound webhook endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookEndpoint {
    /// Stable unique identifier (e.g. `"wh-a1b2c3d4"`).
    pub id: String,
    /// HTTPS (or HTTP for localhost) URL to deliver payloads to.
    pub url: String,
    /// Optional HMAC-SHA256 signing secret.  When set, every delivery adds an
    /// `X-Anima-Signature: sha256=<hex>` header so the receiver can verify the
    /// payload was not tampered with in transit.
    pub secret: Option<String>,
    /// Which event kinds this endpoint subscribes to.
    pub filter: EventFilter,
    /// Whether the endpoint is currently active.
    pub enabled: bool,
    /// Nanosecond-epoch timestamp of registration.
    pub created_at_ns: u64,
}

impl WebhookEndpoint {
    /// Create a new, enabled endpoint.
    pub fn new(
        id: impl Into<String>,
        url: impl Into<String>,
        secret: Option<String>,
        filter: EventFilter,
    ) -> Self {
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        WebhookEndpoint {
            id: id.into(),
            url: url.into(),
            secret,
            filter,
            enabled: true,
            created_at_ns: now_ns,
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_filter_all_matches_any_kind() {
        assert!(EventFilter::All.matches("task_completed"));
        assert!(EventFilter::All.matches("sleep_entered"));
        assert!(EventFilter::All.matches("anything"));
    }

    #[test]
    fn event_filter_selected_matches_only_registered_kinds() {
        let f = EventFilter::only(["task_completed", "alert_fired"]);
        assert!(f.matches("task_completed"));
        assert!(f.matches("alert_fired"));
        assert!(!f.matches("sleep_entered"));
        assert!(!f.matches("unknown"));
    }

    #[test]
    fn event_filter_selected_empty_set_never_matches() {
        let f = EventFilter::only(std::iter::empty::<&str>());
        assert!(!f.matches("task_completed"));
    }

    #[test]
    fn event_filter_default_is_all() {
        assert!(EventFilter::default().matches("anything"));
    }

    #[test]
    fn webhook_endpoint_new_is_enabled() {
        let ep = WebhookEndpoint::new("wh-1", "https://example.com/hook", None, EventFilter::All);
        assert!(ep.enabled);
        assert_eq!(ep.id, "wh-1");
        assert_eq!(ep.url, "https://example.com/hook");
        assert!(ep.secret.is_none());
    }

    #[test]
    fn webhook_endpoint_round_trips_through_json() {
        let ep = WebhookEndpoint::new(
            "wh-test",
            "https://hooks.example.com/anima",
            Some("secret-key".to_string()),
            EventFilter::only(["task_completed"]),
        );
        let json = serde_json::to_string(&ep).unwrap();
        let restored: WebhookEndpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(ep.id, restored.id);
        assert_eq!(ep.url, restored.url);
        assert_eq!(ep.secret, restored.secret);
        assert_eq!(ep.enabled, restored.enabled);
    }
}
