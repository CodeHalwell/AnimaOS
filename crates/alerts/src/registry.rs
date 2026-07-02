//! Persistent registry for [`AlertRule`]s.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::rule::AlertRule;
use crate::state::AlertStateTracker;

// ── RegistryError ─────────────────────────────────────────────────────────────

/// Errors returned by [`AlertRuleRegistry`] operations.
#[derive(Debug)]
pub enum RegistryError {
    /// A rule with this ID already exists.
    AlreadyExists(String),
    /// No rule found with the given ID.
    NotFound(String),
    /// I/O or serialisation error.
    Io(String),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::AlreadyExists(id) => write!(f, "rule already exists: {id}"),
            RegistryError::NotFound(id) => write!(f, "rule not found: {id}"),
            RegistryError::Io(msg) => write!(f, "I/O error: {msg}"),
        }
    }
}

// ── Backing store ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct RegistryStore {
    rules: HashMap<String, AlertRule>,
    trackers: Vec<AlertStateTracker>,
}

impl RegistryStore {
    fn empty() -> Self {
        Self {
            rules: HashMap::new(),
            trackers: Vec::new(),
        }
    }
}

// ── AlertRuleRegistry ─────────────────────────────────────────────────────────

/// Atomic JSON-persisted store of [`AlertRule`]s and their state trackers.
///
/// Rules are keyed by [`AlertRule::id`].  Persistence uses the
/// write-to-`.tmp`-then-rename pattern for crash safety.
pub struct AlertRuleRegistry {
    store: RegistryStore,
    path: Option<PathBuf>,
}

impl AlertRuleRegistry {
    /// Open or create a registry at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RegistryError> {
        let path = path.as_ref().to_path_buf();
        let store = if path.exists() {
            let data =
                std::fs::read_to_string(&path).map_err(|e| RegistryError::Io(e.to_string()))?;
            serde_json::from_str(&data).map_err(|e| RegistryError::Io(e.to_string()))?
        } else {
            RegistryStore::empty()
        };
        Ok(Self {
            store,
            path: Some(path),
        })
    }

    /// Create an in-memory registry (no persistence; useful for tests).
    pub fn in_memory() -> Self {
        Self {
            store: RegistryStore::empty(),
            path: None,
        }
    }

    /// Default file path: `<state_dir>/<agent_id>/alert_rules.json`.
    ///
    /// Routes through [`jsonstore::agent_state_path`] so the alert store shares
    /// the one `ANIMA_STATE_DIR`-aware state root with every other per-agent
    /// store rather than resolving `$HOME/.anima` independently (OPS-13).
    pub fn default_path(agent_id: &str) -> PathBuf {
        jsonstore::agent_state_path(agent_id, "alert_rules.json")
    }

    // ── Mutating API ──────────────────────────────────────────────────────────

    /// Add a new rule.  Returns `AlreadyExists` if the ID is taken.
    pub fn add(&mut self, rule: AlertRule) -> Result<(), RegistryError> {
        if self.store.rules.contains_key(&rule.id) {
            return Err(RegistryError::AlreadyExists(rule.id.clone()));
        }
        self.store.rules.insert(rule.id.clone(), rule);
        self.flush()
    }

    /// Remove a rule by ID.  Returns `NotFound` if not present.
    pub fn remove(&mut self, id: &str) -> Result<AlertRule, RegistryError> {
        let rule = self
            .store
            .rules
            .remove(id)
            .ok_or_else(|| RegistryError::NotFound(id.to_string()))?;
        // Remove matching tracker too.
        self.store.trackers.retain(|t| t.rule_id != id);
        self.flush()?;
        Ok(rule)
    }

    /// Enable or disable a rule.
    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), RegistryError> {
        let rule = self
            .store
            .rules
            .get_mut(id)
            .ok_or_else(|| RegistryError::NotFound(id.to_string()))?;
        rule.enabled = enabled;
        self.flush()
    }

    /// Replace the mutable state trackers (called after an evaluation pass).
    pub fn update_trackers(
        &mut self,
        trackers: Vec<AlertStateTracker>,
    ) -> Result<(), RegistryError> {
        self.store.trackers = trackers;
        self.flush()
    }

    // ── Read API ──────────────────────────────────────────────────────────────

    /// Look up a rule by ID.
    pub fn get(&self, id: &str) -> Option<&AlertRule> {
        self.store.rules.get(id)
    }

    /// Returns all rules, sorted by ID for deterministic output.
    pub fn list(&self) -> Vec<&AlertRule> {
        let mut v: Vec<&AlertRule> = self.store.rules.values().collect();
        v.sort_by_key(|r| r.id.as_str());
        v
    }

    /// Returns all rules as owned values, sorted by ID.
    pub fn rules_owned(&self) -> Vec<AlertRule> {
        self.list().into_iter().cloned().collect()
    }

    /// Returns the mutable state trackers.
    pub fn trackers_mut(&mut self) -> &mut Vec<AlertStateTracker> {
        &mut self.store.trackers
    }

    /// Number of rules currently stored.
    pub fn len(&self) -> usize {
        self.store.rules.len()
    }

    /// Returns `true` when no rules are registered.
    pub fn is_empty(&self) -> bool {
        self.store.rules.is_empty()
    }

    // ── Persistence ───────────────────────────────────────────────────────────

    fn flush(&self) -> Result<(), RegistryError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let json = serde_json::to_string_pretty(&self.store)
            .map_err(|e| RegistryError::Io(e.to_string()))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| RegistryError::Io(e.to_string()))?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, json.as_bytes()).map_err(|e| RegistryError::Io(e.to_string()))?;
        std::fs::rename(&tmp, path).map_err(|e| RegistryError::Io(e.to_string()))?;
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::{AlertCondition, AlertSeverity, ComparisonOp, MetricField};

    fn sample_rule(id: &str) -> AlertRule {
        AlertRule::new(
            id,
            "Sample rule",
            AlertCondition::new(MetricField::CortexFaultRate, ComparisonOp::GreaterThan, 0.1),
            AlertSeverity::Warning,
        )
    }

    #[test]
    fn empty_registry_has_zero_rules() {
        let r = AlertRuleRegistry::in_memory();
        assert_eq!(r.len(), 0);
        assert!(r.is_empty());
    }

    #[test]
    fn add_and_get_rule() {
        let mut r = AlertRuleRegistry::in_memory();
        r.add(sample_rule("r1")).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r.get("r1").unwrap().id, "r1");
    }

    #[test]
    fn add_rejects_duplicate_id() {
        let mut r = AlertRuleRegistry::in_memory();
        r.add(sample_rule("dup")).unwrap();
        assert!(matches!(
            r.add(sample_rule("dup")),
            Err(RegistryError::AlreadyExists(_))
        ));
    }

    #[test]
    fn remove_rule_by_id() {
        let mut r = AlertRuleRegistry::in_memory();
        r.add(sample_rule("to-remove")).unwrap();
        let removed = r.remove("to-remove").unwrap();
        assert_eq!(removed.id, "to-remove");
        assert!(r.is_empty());
    }

    #[test]
    fn remove_returns_not_found_for_missing_rule() {
        let mut r = AlertRuleRegistry::in_memory();
        assert!(matches!(
            r.remove("missing"),
            Err(RegistryError::NotFound(_))
        ));
    }

    #[test]
    fn list_returns_rules_sorted_by_id() {
        let mut r = AlertRuleRegistry::in_memory();
        r.add(sample_rule("z-last")).unwrap();
        r.add(sample_rule("a-first")).unwrap();
        r.add(sample_rule("m-middle")).unwrap();
        let ids: Vec<&str> = r.list().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["a-first", "m-middle", "z-last"]);
    }

    #[test]
    fn set_enabled_toggles_rule() {
        let mut r = AlertRuleRegistry::in_memory();
        r.add(sample_rule("toggle")).unwrap();
        r.set_enabled("toggle", false).unwrap();
        assert!(!r.get("toggle").unwrap().enabled);
        r.set_enabled("toggle", true).unwrap();
        assert!(r.get("toggle").unwrap().enabled);
    }

    #[test]
    fn set_enabled_returns_not_found_for_missing() {
        let mut r = AlertRuleRegistry::in_memory();
        assert!(matches!(
            r.set_enabled("missing", false),
            Err(RegistryError::NotFound(_))
        ));
    }

    #[test]
    fn flush_and_reload_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rules.json");
        {
            let mut r = AlertRuleRegistry::open(&path).unwrap();
            r.add(sample_rule("persist1")).unwrap();
            r.add(sample_rule("persist2")).unwrap();
        }
        let r2 = AlertRuleRegistry::open(&path).unwrap();
        assert_eq!(r2.len(), 2);
        assert!(r2.get("persist1").is_some());
        assert!(r2.get("persist2").is_some());
    }

    #[test]
    fn open_creates_empty_registry_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        let r = AlertRuleRegistry::open(&path).unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn in_memory_flush_is_no_op() {
        let mut r = AlertRuleRegistry::in_memory();
        // Should not panic and should return Ok.
        assert!(r.add(sample_rule("no-file")).is_ok());
    }
}
