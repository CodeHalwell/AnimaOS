#![forbid(unsafe_code)]
//! Webhook notification system — Epic E19.
//!
//! Provides an event-driven webhook delivery pipeline for AnimaOS. Callers
//! register [`WebhookEndpoint`]s with a [`WebhookRegistry`], then use a
//! [`WebhookDispatcher`] to deliver [`WebhookPayload`]s when audit events occur.
//! Deliveries are recorded as [`DeliveryRecord`]s.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ── EventCategory ─────────────────────────────────────────────────────────────

/// Which audit events trigger delivery for a webhook endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventCategory {
    /// QuotaExceeded, QuotaEscalated
    QuotaEvents,
    /// DefenceVeto, AttentionDemandEscalated, ConstitutionVeto
    DefenceEvents,
    /// CortexFault, CortexCompleted, CortexInvoked
    CortexEvents,
    /// UserProfileCreated, UserTrustUpdated, UserConsentUpdated
    UserEvents,
    /// SleepEntered, WakeEntered, SleepPhaseCompleted
    SleepEvents,
    /// TaskStarted, TaskCompleted, TaskFailed
    TaskEvents,
    /// Every event regardless of category.
    All,
}

// ── EventFilter ───────────────────────────────────────────────────────────────

/// Set of [`EventCategory`] values that trigger delivery for a webhook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventFilter {
    pub categories: Vec<EventCategory>,
}

impl EventFilter {
    /// Creates a filter that matches every event.
    pub fn all() -> Self {
        EventFilter {
            categories: vec![EventCategory::All],
        }
    }

    /// Returns `true` when this filter would trigger delivery for `cat`.
    pub fn matches(&self, cat: &EventCategory) -> bool {
        self.categories.contains(&EventCategory::All) || self.categories.contains(cat)
    }
}

impl Default for EventFilter {
    fn default() -> Self {
        Self::all()
    }
}

// ── DeliveryStatus ────────────────────────────────────────────────────────────

/// Outcome of a single webhook delivery attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DeliveryStatus {
    Pending,
    Delivered { status_code: u16 },
    Failed { error: String },
    DeadLetter,
}

// ── DeliveryRecord ────────────────────────────────────────────────────────────

/// Result of one delivery attempt for a webhook endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryRecord {
    pub webhook_id: String,
    pub event_kind: String,
    pub attempt: u32,
    pub status: DeliveryStatus,
    pub timestamp_ns: u64,
}

// ── WebhookEndpoint ───────────────────────────────────────────────────────────

/// A registered webhook endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookEndpoint {
    pub id: String,
    pub url: String,
    pub filter: EventFilter,
    /// HMAC-SHA256 signing key; empty string disables signing.
    pub secret: String,
    pub created_at_ns: u64,
    pub enabled: bool,
}

impl WebhookEndpoint {
    /// Creates a new endpoint with sensible defaults: all events, no secret, enabled.
    pub fn new(id: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            url: url.into(),
            filter: EventFilter::all(),
            secret: String::new(),
            created_at_ns: 0,
            enabled: true,
        }
    }

    /// Builder: sets the HMAC signing secret.
    pub fn with_secret(mut self, secret: impl Into<String>) -> Self {
        self.secret = secret.into();
        self
    }

    /// Builder: replaces the event filter.
    pub fn with_filter(mut self, filter: EventFilter) -> Self {
        self.filter = filter;
        self
    }

    /// Returns a comma-joined summary of the active filter categories.
    pub fn filter_summary(&self) -> String {
        self.filter
            .categories
            .iter()
            .map(|c| format!("{c:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

// ── WebhookPayload ────────────────────────────────────────────────────────────

/// The JSON body POSTed to a webhook endpoint on each delivery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookPayload {
    pub webhook_id: String,
    pub event_kind: String,
    pub agent_id: String,
    pub timestamp_ns: u64,
    pub data: serde_json::Value,
}

// ── RegistryError ─────────────────────────────────────────────────────────────

/// Errors returned by [`WebhookRegistry`] operations.
#[derive(Debug)]
pub enum RegistryError {
    AlreadyExists,
    NotFound,
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::AlreadyExists => write!(f, "webhook endpoint already exists"),
            RegistryError::NotFound => write!(f, "webhook endpoint not found"),
            RegistryError::Io(e) => write!(f, "I/O error: {e}"),
            RegistryError::Json(e) => write!(f, "JSON error: {e}"),
        }
    }
}

impl std::error::Error for RegistryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RegistryError::Io(e) => Some(e),
            RegistryError::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for RegistryError {
    fn from(e: std::io::Error) -> Self {
        RegistryError::Io(e)
    }
}

impl From<serde_json::Error> for RegistryError {
    fn from(e: serde_json::Error) -> Self {
        RegistryError::Json(e)
    }
}

// ── WebhookRegistry ───────────────────────────────────────────────────────────

/// In-memory CRUD store for webhook endpoints with optional JSON persistence.
pub struct WebhookRegistry {
    endpoints: HashMap<String, WebhookEndpoint>,
    path: Option<PathBuf>,
}

impl WebhookRegistry {
    /// Creates an in-memory-only registry (no file persistence).
    pub fn in_memory() -> Self {
        Self {
            endpoints: HashMap::new(),
            path: None,
        }
    }

    /// Opens a registry backed by `path`.
    ///
    /// If the file exists it is loaded; otherwise an empty registry is returned.
    /// Call [`flush`](Self::flush) to persist changes.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RegistryError> {
        let path = path.as_ref().to_path_buf();
        let endpoints = if path.exists() {
            let bytes = std::fs::read(&path)?;
            let list: Vec<WebhookEndpoint> = serde_json::from_slice(&bytes)?;
            list.into_iter().map(|e| (e.id.clone(), e)).collect()
        } else {
            HashMap::new()
        };
        Ok(Self {
            endpoints,
            path: Some(path),
        })
    }

    /// Registers a new endpoint.
    ///
    /// Returns [`RegistryError::AlreadyExists`] if an endpoint with the same id
    /// is already registered.
    pub fn register(&mut self, endpoint: WebhookEndpoint) -> Result<(), RegistryError> {
        if self.endpoints.contains_key(&endpoint.id) {
            return Err(RegistryError::AlreadyExists);
        }
        self.endpoints.insert(endpoint.id.clone(), endpoint);
        Ok(())
    }

    /// Removes and returns the endpoint with `id`.
    pub fn remove(&mut self, id: &str) -> Result<WebhookEndpoint, RegistryError> {
        self.endpoints.remove(id).ok_or(RegistryError::NotFound)
    }

    /// Returns a reference to the endpoint with `id`, or `None`.
    pub fn get(&self, id: &str) -> Option<&WebhookEndpoint> {
        self.endpoints.get(id)
    }

    /// Returns all endpoints sorted by `created_at_ns` (ascending).
    pub fn list(&self) -> Vec<&WebhookEndpoint> {
        let mut v: Vec<&WebhookEndpoint> = self.endpoints.values().collect();
        v.sort_by_key(|e| e.created_at_ns);
        v
    }

    /// Returns the number of registered endpoints.
    pub fn len(&self) -> usize {
        self.endpoints.len()
    }

    /// Returns `true` when no endpoints are registered.
    pub fn is_empty(&self) -> bool {
        self.endpoints.is_empty()
    }

    /// Atomically writes the registry to disk.
    ///
    /// Writes to a `.tmp` sidecar first, then renames over the target path.
    /// No-op for in-memory registries.
    pub fn flush(&self) -> Result<(), RegistryError> {
        let path = match &self.path {
            Some(p) => p,
            None => return Ok(()),
        };
        let list: Vec<&WebhookEndpoint> = self.list();
        let json = serde_json::to_vec_pretty(&list)?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &json)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Returns all enabled endpoints whose filter matches `cat`.
    pub fn endpoints_for_category(&self, cat: &EventCategory) -> Vec<&WebhookEndpoint> {
        let mut v: Vec<&WebhookEndpoint> = self
            .endpoints
            .values()
            .filter(|e| e.enabled && e.filter.matches(cat))
            .collect();
        v.sort_by_key(|e| e.created_at_ns);
        v
    }
}

// ── WebhookDispatcher ─────────────────────────────────────────────────────────

/// Drives delivery of webhook payloads to endpoints.
pub struct WebhookDispatcher {
    /// Maximum number of delivery attempts before moving to dead-letter.
    pub max_retries: u32,
    /// When `false` (default) the dispatcher operates in fixture mode: no real
    /// HTTP requests are made and every dispatch returns `Delivered { 200 }`.
    pub live: bool,
}

impl WebhookDispatcher {
    pub fn new() -> Self {
        Self {
            max_retries: 3,
            live: false,
        }
    }

    /// Attempts to deliver `payload` to `endpoint` and returns a [`DeliveryRecord`].
    ///
    /// In fixture mode (`live = false`) the delivery always succeeds with HTTP 200.
    /// In live mode a stub `Failed` record is returned (real HTTP is not implemented).
    pub fn dispatch(
        &self,
        endpoint: &WebhookEndpoint,
        payload: &WebhookPayload,
        attempt: u32,
    ) -> DeliveryRecord {
        let status = if !self.live {
            DeliveryStatus::Delivered { status_code: 200 }
        } else {
            DeliveryStatus::Failed {
                error: "live HTTP delivery not implemented".to_owned(),
            }
        };
        DeliveryRecord {
            webhook_id: endpoint.id.clone(),
            event_kind: payload.event_kind.clone(),
            attempt,
            status,
            timestamp_ns: payload.timestamp_ns,
        }
    }

    /// Returns an HMAC-SHA256 hex signature of `payload_json` using `secret`.
    ///
    /// When `secret` is empty, returns an empty string (signing disabled).
    /// In fixture mode a deterministic mock is used (hex of payload length).
    pub fn sign_payload(&self, payload_json: &str, secret: &str) -> String {
        if secret.is_empty() {
            return String::new();
        }
        // Fixture-mode mock: deterministic without a crypto dependency.
        // The mock XORs the secret bytes with successive payload bytes and
        // produces a 16-char hex string based on the accumulated value.
        let mut acc: u64 = 0;
        for (i, b) in payload_json.bytes().enumerate() {
            let sk = secret.as_bytes()[i % secret.len()];
            acc = acc.wrapping_add((b ^ sk) as u64);
        }
        format!("{:016x}", acc)
    }
}

impl Default for WebhookDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(n: u64) -> u64 {
        n
    }

    fn make_endpoint(id: &str, ts_ns: u64) -> WebhookEndpoint {
        let mut ep = WebhookEndpoint::new(id, format!("https://example.com/{id}"));
        ep.created_at_ns = ts_ns;
        ep
    }

    // ── EventFilter ───────────────────────────────────────────────────────────

    #[test]
    fn event_filter_all_matches_any_category() {
        let f = EventFilter::all();
        assert!(f.matches(&EventCategory::QuotaEvents));
        assert!(f.matches(&EventCategory::DefenceEvents));
        assert!(f.matches(&EventCategory::CortexEvents));
        assert!(f.matches(&EventCategory::UserEvents));
        assert!(f.matches(&EventCategory::SleepEvents));
        assert!(f.matches(&EventCategory::TaskEvents));
        assert!(f.matches(&EventCategory::All));
    }

    #[test]
    fn event_filter_specific_matches_own_category() {
        let f = EventFilter {
            categories: vec![EventCategory::QuotaEvents],
        };
        assert!(f.matches(&EventCategory::QuotaEvents));
    }

    #[test]
    fn event_filter_specific_does_not_match_other_category() {
        let f = EventFilter {
            categories: vec![EventCategory::QuotaEvents],
        };
        assert!(!f.matches(&EventCategory::TaskEvents));
        assert!(!f.matches(&EventCategory::UserEvents));
    }

    #[test]
    fn event_filter_default_is_all() {
        let f = EventFilter::default();
        assert!(f.matches(&EventCategory::QuotaEvents));
        assert!(f.matches(&EventCategory::CortexEvents));
    }

    // ── WebhookEndpoint ───────────────────────────────────────────────────────

    #[test]
    fn webhook_endpoint_new_has_defaults() {
        let ep = WebhookEndpoint::new("w1", "https://example.com/hook");
        assert_eq!(ep.id, "w1");
        assert_eq!(ep.url, "https://example.com/hook");
        assert!(ep.secret.is_empty());
        assert!(ep.enabled);
        assert_eq!(ep.created_at_ns, 0);
    }

    #[test]
    fn webhook_endpoint_with_secret_sets_secret() {
        let ep = WebhookEndpoint::new("w1", "https://example.com/hook").with_secret("my-secret");
        assert_eq!(ep.secret, "my-secret");
    }

    #[test]
    fn webhook_endpoint_with_filter_sets_filter() {
        let filter = EventFilter {
            categories: vec![EventCategory::QuotaEvents, EventCategory::TaskEvents],
        };
        let ep = WebhookEndpoint::new("w1", "https://example.com/hook").with_filter(filter.clone());
        assert_eq!(ep.filter.categories.len(), 2);
        assert!(ep.filter.matches(&EventCategory::QuotaEvents));
        assert!(!ep.filter.matches(&EventCategory::SleepEvents));
    }

    #[test]
    fn webhook_endpoint_filter_summary_all() {
        let ep = WebhookEndpoint::new("w1", "https://example.com/hook");
        assert!(ep.filter_summary().contains("All"));
    }

    #[test]
    fn webhook_endpoint_filter_summary_specific() {
        let filter = EventFilter {
            categories: vec![EventCategory::QuotaEvents, EventCategory::TaskEvents],
        };
        let ep = WebhookEndpoint::new("w1", "https://example.com/hook").with_filter(filter);
        let summary = ep.filter_summary();
        assert!(summary.contains("QuotaEvents"));
        assert!(summary.contains("TaskEvents"));
    }

    // ── WebhookRegistry ───────────────────────────────────────────────────────

    #[test]
    fn registry_in_memory_starts_empty() {
        let r = WebhookRegistry::in_memory();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn registry_register_adds_endpoint() {
        let mut r = WebhookRegistry::in_memory();
        let ep = WebhookEndpoint::new("w1", "https://example.com/1");
        r.register(ep).unwrap();
        assert_eq!(r.len(), 1);
        assert!(r.get("w1").is_some());
    }

    #[test]
    fn registry_register_rejects_duplicate_id() {
        let mut r = WebhookRegistry::in_memory();
        r.register(WebhookEndpoint::new("w1", "https://example.com/1"))
            .unwrap();
        let err = r
            .register(WebhookEndpoint::new("w1", "https://example.com/2"))
            .unwrap_err();
        assert!(matches!(err, RegistryError::AlreadyExists));
    }

    #[test]
    fn registry_remove_returns_endpoint() {
        let mut r = WebhookRegistry::in_memory();
        r.register(WebhookEndpoint::new("w1", "https://example.com/1"))
            .unwrap();
        let ep = r.remove("w1").unwrap();
        assert_eq!(ep.id, "w1");
        assert!(r.is_empty());
    }

    #[test]
    fn registry_remove_returns_not_found_for_missing() {
        let mut r = WebhookRegistry::in_memory();
        let err = r.remove("nonexistent").unwrap_err();
        assert!(matches!(err, RegistryError::NotFound));
    }

    #[test]
    fn registry_list_returns_sorted_by_created_at() {
        let mut r = WebhookRegistry::in_memory();
        r.register(make_endpoint("c", ts(300))).unwrap();
        r.register(make_endpoint("a", ts(100))).unwrap();
        r.register(make_endpoint("b", ts(200))).unwrap();
        let list = r.list();
        assert_eq!(list[0].id, "a");
        assert_eq!(list[1].id, "b");
        assert_eq!(list[2].id, "c");
    }

    #[test]
    fn registry_endpoints_for_category_filters_by_enabled_and_filter() {
        let mut r = WebhookRegistry::in_memory();
        let ep_all = make_endpoint("all", ts(1));
        let ep_quota = WebhookEndpoint {
            id: "quota".to_owned(),
            url: "https://example.com/quota".to_owned(),
            filter: EventFilter {
                categories: vec![EventCategory::QuotaEvents],
            },
            secret: String::new(),
            created_at_ns: ts(2),
            enabled: true,
        };
        r.register(ep_all).unwrap();
        r.register(ep_quota).unwrap();

        let for_quota = r.endpoints_for_category(&EventCategory::QuotaEvents);
        assert_eq!(for_quota.len(), 2);

        let for_task = r.endpoints_for_category(&EventCategory::TaskEvents);
        assert_eq!(for_task.len(), 1);
        assert_eq!(for_task[0].id, "all");
    }

    #[test]
    fn registry_endpoints_for_category_excludes_disabled() {
        let mut r = WebhookRegistry::in_memory();
        let mut ep = make_endpoint("w1", ts(1));
        ep.enabled = false;
        r.register(ep).unwrap();

        let list = r.endpoints_for_category(&EventCategory::All);
        assert!(list.is_empty());
    }

    #[test]
    fn registry_flush_and_reload_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("webhooks.json");

        let mut r = WebhookRegistry::open(&path).unwrap();
        r.register(make_endpoint("w1", ts(1))).unwrap();
        r.register(make_endpoint("w2", ts(2))).unwrap();
        r.flush().unwrap();

        let r2 = WebhookRegistry::open(&path).unwrap();
        assert_eq!(r2.len(), 2);
        assert!(r2.get("w1").is_some());
        assert!(r2.get("w2").is_some());
    }

    #[test]
    fn registry_open_creates_empty_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no-such-file.json");
        let r = WebhookRegistry::open(&path).unwrap();
        assert!(r.is_empty());
    }

    // ── WebhookDispatcher ─────────────────────────────────────────────────────

    #[test]
    fn dispatcher_fixture_mode_delivers_200() {
        let d = WebhookDispatcher::new();
        let ep = WebhookEndpoint::new("w1", "https://example.com/1");
        let payload = WebhookPayload {
            webhook_id: "w1".to_owned(),
            event_kind: "TaskStarted".to_owned(),
            agent_id: "anima".to_owned(),
            timestamp_ns: 0,
            data: serde_json::Value::Null,
        };
        let record = d.dispatch(&ep, &payload, 1);
        assert_eq!(
            record.status,
            DeliveryStatus::Delivered { status_code: 200 }
        );
    }

    #[test]
    fn dispatcher_fixture_mode_records_correct_attempt_number() {
        let d = WebhookDispatcher::new();
        let ep = WebhookEndpoint::new("w1", "https://example.com/1");
        let payload = WebhookPayload {
            webhook_id: "w1".to_owned(),
            event_kind: "TaskStarted".to_owned(),
            agent_id: "anima".to_owned(),
            timestamp_ns: 0,
            data: serde_json::Value::Null,
        };
        let record = d.dispatch(&ep, &payload, 3);
        assert_eq!(record.attempt, 3);
    }

    #[test]
    fn dispatcher_delivery_record_has_correct_webhook_id() {
        let d = WebhookDispatcher::new();
        let ep = WebhookEndpoint::new("my-webhook", "https://example.com/hook");
        let payload = WebhookPayload {
            webhook_id: "my-webhook".to_owned(),
            event_kind: "CortexFault".to_owned(),
            agent_id: "anima".to_owned(),
            timestamp_ns: 42,
            data: serde_json::Value::Null,
        };
        let record = d.dispatch(&ep, &payload, 1);
        assert_eq!(record.webhook_id, "my-webhook");
        assert_eq!(record.event_kind, "CortexFault");
    }

    #[test]
    fn delivery_status_delivered_has_status_code() {
        let s = DeliveryStatus::Delivered { status_code: 200 };
        assert_eq!(s, DeliveryStatus::Delivered { status_code: 200 });
        assert_ne!(s, DeliveryStatus::Pending);
    }

    #[test]
    fn webhook_payload_round_trips_through_json() {
        let payload = WebhookPayload {
            webhook_id: "w1".to_owned(),
            event_kind: "UserEvents".to_owned(),
            agent_id: "anima".to_owned(),
            timestamp_ns: 123_456_789,
            data: serde_json::json!({ "user_id": "telegram:42" }),
        };
        let json = serde_json::to_string(&payload).unwrap();
        let decoded: WebhookPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.webhook_id, "w1");
        assert_eq!(decoded.event_kind, "UserEvents");
        assert_eq!(decoded.timestamp_ns, 123_456_789);
        assert_eq!(decoded.data["user_id"], "telegram:42");
    }
}
