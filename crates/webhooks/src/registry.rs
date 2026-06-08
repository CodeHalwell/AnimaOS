//! Webhook endpoint registry with atomic JSON persistence.

use crate::endpoint::WebhookEndpoint;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Errors returned by registry operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// An endpoint with this ID already exists.
    AlreadyExists(String),
    /// No endpoint with this ID was found.
    NotFound(String),
    /// Disk I/O failed.
    Io(String),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::AlreadyExists(id) => write!(f, "endpoint '{id}' already exists"),
            RegistryError::NotFound(id) => write!(f, "endpoint '{id}' not found"),
            RegistryError::Io(msg) => write!(f, "I/O error: {msg}"),
        }
    }
}

/// Persisted store contents.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Store {
    endpoints: HashMap<String, WebhookEndpoint>,
}

/// Registry of outbound webhook endpoints.
///
/// Endpoints are persisted atomically to a JSON file (write-to-`.tmp`-then-rename)
/// so a crash during `flush` never leaves a partial or corrupt store on disk.
pub struct WebhookRegistry {
    store: Store,
    path: Option<PathBuf>,
}

impl WebhookRegistry {
    /// Create a transient in-memory registry (no disk persistence).  Ideal for tests.
    pub fn in_memory() -> Self {
        WebhookRegistry {
            store: Store::default(),
            path: None,
        }
    }

    /// Open (or create) a registry backed by `path`.
    ///
    /// If the file does not exist an empty registry is created.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RegistryError> {
        let path = path.as_ref().to_path_buf();
        let store = if path.exists() {
            let raw =
                std::fs::read_to_string(&path).map_err(|e| RegistryError::Io(e.to_string()))?;
            serde_json::from_str(&raw).map_err(|e| RegistryError::Io(e.to_string()))?
        } else {
            Store::default()
        };
        Ok(WebhookRegistry {
            store,
            path: Some(path),
        })
    }

    /// Default path for an agent's webhook registry:
    /// `~/.anima/<agent_id>/webhook_endpoints.json`.
    pub fn default_path(agent_id: &str) -> PathBuf {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(home)
            .join(".anima")
            .join(agent_id)
            .join("webhook_endpoints.json")
    }

    /// Register a new endpoint.  Returns `AlreadyExists` if an endpoint with
    /// the same ID is already present.
    pub fn register(&mut self, endpoint: WebhookEndpoint) -> Result<(), RegistryError> {
        if self.store.endpoints.contains_key(&endpoint.id) {
            return Err(RegistryError::AlreadyExists(endpoint.id.clone()));
        }
        self.store.endpoints.insert(endpoint.id.clone(), endpoint);
        self.flush()
    }

    /// Remove an endpoint by ID.  Returns `NotFound` if absent.
    pub fn remove(&mut self, id: &str) -> Result<WebhookEndpoint, RegistryError> {
        let ep = self
            .store
            .endpoints
            .remove(id)
            .ok_or_else(|| RegistryError::NotFound(id.to_string()))?;
        self.flush()?;
        Ok(ep)
    }

    /// Look up an endpoint by ID.
    pub fn get(&self, id: &str) -> Option<&WebhookEndpoint> {
        self.store.endpoints.get(id)
    }

    /// Enable or disable an endpoint.
    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), RegistryError> {
        let ep = self
            .store
            .endpoints
            .get_mut(id)
            .ok_or_else(|| RegistryError::NotFound(id.to_string()))?;
        ep.enabled = enabled;
        self.flush()
    }

    /// Return all endpoints, sorted by ID for deterministic output.
    pub fn list(&self) -> Vec<&WebhookEndpoint> {
        let mut v: Vec<&WebhookEndpoint> = self.store.endpoints.values().collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    /// Return the endpoints that should receive an event of the given `kind`.
    ///
    /// Only enabled endpoints whose filter matches are returned.
    pub fn endpoints_for_event(&self, kind: &str) -> Vec<&WebhookEndpoint> {
        self.store
            .endpoints
            .values()
            .filter(|ep| ep.enabled && ep.filter.matches(kind))
            .collect()
    }

    /// Number of registered endpoints.
    pub fn len(&self) -> usize {
        self.store.endpoints.len()
    }

    /// Returns `true` when no endpoints are registered.
    pub fn is_empty(&self) -> bool {
        self.store.endpoints.is_empty()
    }

    /// Flush the current state to disk atomically.
    fn flush(&self) -> Result<(), RegistryError> {
        let Some(path) = &self.path else {
            return Ok(()); // in-memory — nothing to write
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| RegistryError::Io(e.to_string()))?;
        }
        let json = serde_json::to_string_pretty(&self.store)
            .map_err(|e| RegistryError::Io(e.to_string()))?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &json).map_err(|e| RegistryError::Io(e.to_string()))?;
        std::fs::rename(&tmp, path).map_err(|e| RegistryError::Io(e.to_string()))?;
        Ok(())
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::EventFilter;

    fn ep(id: &str) -> WebhookEndpoint {
        WebhookEndpoint::new(
            id,
            format!("https://example.com/{id}"),
            None,
            EventFilter::All,
        )
    }

    #[test]
    fn empty_registry_has_zero_endpoints() {
        let r = WebhookRegistry::in_memory();
        assert_eq!(r.len(), 0);
        assert!(r.is_empty());
    }

    #[test]
    fn register_adds_endpoint() {
        let mut r = WebhookRegistry::in_memory();
        r.register(ep("wh-1")).unwrap();
        assert_eq!(r.len(), 1);
        assert!(r.get("wh-1").is_some());
    }

    #[test]
    fn register_rejects_duplicate_id() {
        let mut r = WebhookRegistry::in_memory();
        r.register(ep("wh-dup")).unwrap();
        let err = r.register(ep("wh-dup")).unwrap_err();
        assert_eq!(err, RegistryError::AlreadyExists("wh-dup".to_string()));
    }

    #[test]
    fn remove_returns_endpoint() {
        let mut r = WebhookRegistry::in_memory();
        r.register(ep("wh-x")).unwrap();
        let removed = r.remove("wh-x").unwrap();
        assert_eq!(removed.id, "wh-x");
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn remove_returns_not_found_for_missing_id() {
        let mut r = WebhookRegistry::in_memory();
        let err = r.remove("ghost").unwrap_err();
        assert_eq!(err, RegistryError::NotFound("ghost".to_string()));
    }

    #[test]
    fn set_enabled_updates_state() {
        let mut r = WebhookRegistry::in_memory();
        r.register(ep("wh-toggle")).unwrap();
        r.set_enabled("wh-toggle", false).unwrap();
        assert!(!r.get("wh-toggle").unwrap().enabled);
        r.set_enabled("wh-toggle", true).unwrap();
        assert!(r.get("wh-toggle").unwrap().enabled);
    }

    #[test]
    fn set_enabled_returns_not_found_for_missing_id() {
        let mut r = WebhookRegistry::in_memory();
        let err = r.set_enabled("missing", true).unwrap_err();
        assert_eq!(err, RegistryError::NotFound("missing".to_string()));
    }

    #[test]
    fn list_returns_endpoints_sorted_by_id() {
        let mut r = WebhookRegistry::in_memory();
        r.register(ep("wh-c")).unwrap();
        r.register(ep("wh-a")).unwrap();
        r.register(ep("wh-b")).unwrap();
        let ids: Vec<&str> = r.list().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["wh-a", "wh-b", "wh-c"]);
    }

    #[test]
    fn endpoints_for_event_returns_matching_enabled_only() {
        let mut r = WebhookRegistry::in_memory();
        r.register(WebhookEndpoint::new(
            "wh-all",
            "https://a.example.com",
            None,
            EventFilter::All,
        ))
        .unwrap();
        r.register(WebhookEndpoint::new(
            "wh-selected",
            "https://b.example.com",
            None,
            EventFilter::only(["task_completed"]),
        ))
        .unwrap();
        r.register(WebhookEndpoint::new(
            "wh-disabled",
            "https://c.example.com",
            None,
            EventFilter::All,
        ))
        .unwrap();
        r.set_enabled("wh-disabled", false).unwrap();

        let matches: Vec<&str> = r
            .endpoints_for_event("task_completed")
            .iter()
            .map(|e| e.id.as_str())
            .collect();
        assert!(matches.contains(&"wh-all"), "All-filter should match");
        assert!(
            matches.contains(&"wh-selected"),
            "Selected filter should match"
        );
        assert!(
            !matches.contains(&"wh-disabled"),
            "Disabled endpoint should not match"
        );
    }

    #[test]
    fn endpoints_for_event_excludes_non_matching_filter() {
        let mut r = WebhookRegistry::in_memory();
        r.register(WebhookEndpoint::new(
            "wh-narrow",
            "https://narrow.example.com",
            None,
            EventFilter::only(["alert_fired"]),
        ))
        .unwrap();

        let matches = r.endpoints_for_event("task_completed");
        assert!(matches.is_empty());
    }

    #[test]
    fn flush_and_reload_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hooks.json");
        {
            let mut r = WebhookRegistry::open(&path).unwrap();
            r.register(ep("wh-persist")).unwrap();
            r.register(ep("wh-persist2")).unwrap();
        }
        let r2 = WebhookRegistry::open(&path).unwrap();
        assert_eq!(r2.len(), 2);
        assert!(r2.get("wh-persist").is_some());
        assert!(r2.get("wh-persist2").is_some());
    }

    #[test]
    fn open_creates_empty_registry_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        let r = WebhookRegistry::open(&path).unwrap();
        assert!(r.is_empty());
    }
}
