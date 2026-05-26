// crates/vita/src/identity.rs
//! E5.5 — Identity Memory (S5.5.3, S5.5.4, S5.5.5).
//!
//! Identity memory holds stable, human-readable facts about the user, the
//! machine, and the agent's own configuration. It lives in a JSON file under
//! the agent's state directory and is written atomically (write-to-tmp-then-
//! rename) on every mutation.
//!
//! Default file path: `~/.anima/<agent_id>/identity.json`
//!
//! # Schema (S5.5.3)
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "user_preferences": {
//!     "response_style": "concise",
//!     "language": "en",
//!     "timezone": "UTC",
//!     "extra": {}
//!   },
//!   "recurring_tasks": [],
//!   "observed_patterns": [],
//!   "system_policies": {
//!     "max_daily_budget_fraction": null,
//!     "blocklisted_tools": [],
//!     "blocklisted_hosts": [],
//!     "extra": {}
//!   },
//!   "agent_self_model": {
//!     "name": "Anima",
//!     "role": "personal cognitive assistant",
//!     "preferred_backend": null,
//!     "extra": {}
//!   },
//!   "facts": {}
//! }
//! ```
//!
//! The `facts` map is a free-form `String → String` dictionary that the
//! `anima identity set <key> <value>` command targets (S5.5.4).
//!
//! # Router integration (S5.5.5)
//!
//! [`IdentityMemory::to_json`] converts the document to a `serde_json::Value`
//! suitable for injection into [`crate::InvokeRequest::identity`].  The cortex
//! sees identity as a distinct JSON object, not concatenated with task context,
//! satisfying exit criterion 3.
//!
//! # Audit trail (S5.5.4)
//!
//! Every call to [`IdentityMemory::set_fact`] appends an
//! [`crate::AuditEntry::IdentityUpdated`] entry to the caller-supplied
//! [`crate::AuditLog`], recording the key, old value, and new value.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{AuditEntry, AuditLog};

// ── Schema types (S5.5.3) ─────────────────────────────────────────────────

/// User-facing preferences stored in identity memory.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UserPreferences {
    /// Preferred response style (e.g. `"concise"`, `"detailed"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_style: Option<String>,
    /// Preferred language / locale (e.g. `"en"`, `"fr"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Time zone string (e.g. `"Europe/London"`, `"UTC"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// Extra arbitrary preference key/value pairs.
    #[serde(default)]
    pub extra: HashMap<String, String>,
}

/// A recurring task template stored in identity memory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecurringTask {
    /// Human-readable task description (shown to the cortex planner).
    pub description: String,
    /// Cron-style schedule (e.g. `"0 9 * * 1"` for every Monday at 09:00).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    /// Priority tier (`"High"` / `"Medium"` / `"Low"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
}

/// A behavioural pattern extracted from past cortex traces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservedPattern {
    /// Short stable identifier for the pattern.
    pub id: String,
    /// Human-readable description of the pattern.
    pub description: String,
    /// Number of cortex invocations in which the pattern was observed.
    pub evidence_count: u32,
}

/// System-level policy constraints stored in identity memory.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SystemPolicies {
    /// Maximum daily financial budget fraction (`0.0`–`1.0`). `None` = unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_daily_budget_fraction: Option<f32>,
    /// Tool IDs that are permanently blocked from cortex use.
    #[serde(default)]
    pub blocklisted_tools: Vec<String>,
    /// Network hosts that are permanently blocked from motor actions.
    #[serde(default)]
    pub blocklisted_hosts: Vec<String>,
    /// Extra arbitrary policy constraints.
    #[serde(default)]
    pub extra: HashMap<String, String>,
}

/// The agent's self-model — stable facts about the agent itself.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentSelfModel {
    /// The agent's display name (e.g. `"Anima"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The agent's purpose / role description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Preferred LLM backend string (e.g. `"anthropic"`, `"openai"`, `"mock"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_backend: Option<String>,
    /// Extra arbitrary self-model fields.
    #[serde(default)]
    pub extra: HashMap<String, String>,
}

// ── Schema version ─────────────────────────────────────────────────────────

fn default_schema_version() -> u32 {
    1
}

/// Full identity memory document.
///
/// This is the top-level JSON object written to `identity.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityDocument {
    /// Schema version for forward-compatibility detection.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// User-facing preferences.
    #[serde(default)]
    pub user_preferences: UserPreferences,
    /// Recurring task templates.
    #[serde(default)]
    pub recurring_tasks: Vec<RecurringTask>,
    /// Observed behavioural patterns.
    #[serde(default)]
    pub observed_patterns: Vec<ObservedPattern>,
    /// System-level policy constraints.
    #[serde(default)]
    pub system_policies: SystemPolicies,
    /// Agent self-model.
    #[serde(default)]
    pub agent_self_model: AgentSelfModel,
    /// Free-form key/value facts dictionary (targeted by `anima identity set`).
    #[serde(default)]
    pub facts: HashMap<String, String>,
}

impl Default for IdentityDocument {
    fn default() -> Self {
        Self {
            schema_version: 1,
            user_preferences: UserPreferences::default(),
            recurring_tasks: Vec::new(),
            observed_patterns: Vec::new(),
            system_policies: SystemPolicies::default(),
            agent_self_model: AgentSelfModel::default(),
            facts: HashMap::new(),
        }
    }
}

impl IdentityDocument {
    /// Converts the document to a `serde_json::Value` for injection into
    /// [`crate::InvokeRequest::identity`].
    ///
    /// The returned object is a distinct JSON section in the cortex's prompt
    /// assembly, satisfying E5.5 exit criterion 3.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    /// Attempts to deserialise an `IdentityDocument` from a `serde_json::Value`.
    pub fn from_json(value: &serde_json::Value) -> Option<Self> {
        serde_json::from_value(value.clone()).ok()
    }
}

// ── Error type ─────────────────────────────────────────────────────────────

/// Errors returned by identity memory operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityError {
    /// The backing file could not be read or written.
    Io(String),
    /// The file content is not valid JSON or does not conform to the schema.
    ParseError(String),
}

impl std::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(s) => write!(f, "identity I/O error: {s}"),
            Self::ParseError(s) => write!(f, "identity parse error: {s}"),
        }
    }
}

impl std::error::Error for IdentityError {}

// ── IdentityMemory ─────────────────────────────────────────────────────────

/// File-backed identity memory store.
///
/// Holds the in-memory [`IdentityDocument`] and persists it atomically to
/// `path` on every mutation.
///
/// Use [`IdentityMemory::open`] to load from disk, or
/// [`IdentityMemory::in_memory`] for a test-only in-process store.
#[derive(Debug)]
pub struct IdentityMemory {
    /// Path to the backing `identity.json` file.
    ///
    /// The sentinel value `:memory:` indicates an in-memory-only store; no
    /// I/O is performed.
    pub path: PathBuf,
    /// In-memory identity document.
    document: IdentityDocument,
}

impl IdentityMemory {
    /// Opens (or creates) an identity store at `path`.
    ///
    /// If the file already exists it is parsed.  If it does not exist a fresh
    /// `IdentityDocument` is written immediately so the file is always present
    /// after `open` returns.
    pub fn open(path: &Path) -> Result<Self, IdentityError> {
        if path.exists() {
            let contents =
                std::fs::read_to_string(path).map_err(|e| IdentityError::Io(e.to_string()))?;
            let doc: IdentityDocument = serde_json::from_str(&contents)
                .map_err(|e| IdentityError::ParseError(e.to_string()))?;
            Ok(Self {
                path: path.to_owned(),
                document: doc,
            })
        } else {
            let store = Self {
                path: path.to_owned(),
                document: IdentityDocument::default(),
            };
            store.flush()?;
            Ok(store)
        }
    }

    /// Opens an in-memory-only identity store (no backing file).
    ///
    /// All mutations are applied in memory but are never persisted to disk.
    /// Used in tests and ephemeral contexts.
    pub fn in_memory() -> Self {
        Self {
            path: PathBuf::from(":memory:"),
            document: IdentityDocument::default(),
        }
    }

    /// Returns a shared reference to the in-memory [`IdentityDocument`].
    pub fn document(&self) -> &IdentityDocument {
        &self.document
    }

    /// Returns a mutable reference to the in-memory [`IdentityDocument`].
    ///
    /// Callers that mutate the document directly must call
    /// [`IdentityMemory::flush_document`] to persist the changes.
    pub fn document_mut(&mut self) -> &mut IdentityDocument {
        &mut self.document
    }

    /// Persists any mutations made via [`document_mut`](Self::document_mut).
    pub fn flush_document(&self) -> Result<(), IdentityError> {
        self.flush()
    }

    /// Returns the value of a free-form fact key, or `None`.
    ///
    /// Corresponds to `anima identity show <key>`.
    pub fn get_fact(&self, key: &str) -> Option<&str> {
        self.document.facts.get(key).map(String::as_str)
    }

    /// Sets or overwrites a free-form fact, persists to disk, and appends an
    /// [`AuditEntry::IdentityUpdated`] entry to the supplied log.
    ///
    /// Satisfies S5.5.4 (`anima identity set <key> <value>`) and E5.5 exit
    /// criterion 1 ("edits round-trip through the audit log").
    pub fn set_fact(
        &mut self,
        key: &str,
        value: &str,
        log: &mut AuditLog,
        agent_id: &str,
    ) -> Result<(), IdentityError> {
        let old_value = self.document.facts.get(key).cloned();
        self.document.facts.insert(key.to_owned(), value.to_owned());
        self.flush()?;
        log.push(AuditEntry::IdentityUpdated {
            agent_id: agent_id.to_owned(),
            key: key.to_owned(),
            old_value,
            new_value: value.to_owned(),
        });
        Ok(())
    }

    /// Converts the in-memory document to a `serde_json::Value` for injection
    /// into `InvokeRequest::identity` (S5.5.5 router integration).
    pub fn to_json(&self) -> serde_json::Value {
        self.document.to_json()
    }

    /// Returns the default identity file path for an agent.
    ///
    /// Path: `~/.anima/<agent_id>/identity.json`.
    pub fn default_path(agent_id: &str) -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
        PathBuf::from(home)
            .join(".anima")
            .join(agent_id)
            .join("identity.json")
    }

    // ── Internal helpers ────────────────────────────────────────────────────

    /// Write the in-memory document to `path` atomically.
    ///
    /// In-memory stores (sentinel path `:memory:`) are silently skipped.
    fn flush(&self) -> Result<(), IdentityError> {
        if self.path == Path::new(":memory:") {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| IdentityError::Io(e.to_string()))?;
        }
        let json = serde_json::to_string_pretty(&self.document)
            .map_err(|e| IdentityError::Io(e.to_string()))?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json.as_bytes()).map_err(|e| IdentityError::Io(e.to_string()))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| IdentityError::Io(e.to_string()))?;
        Ok(())
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AuditEntry;
    use crate::AuditLog;

    // S5.5.3 ─────────────────────────────────────────────────────────────────

    /// A fresh in-memory store has default (empty) fields and schema_version=1.
    #[test]
    fn fresh_identity_store_has_default_fields() {
        let store = IdentityMemory::in_memory();
        assert_eq!(store.document().schema_version, 1);
        assert!(store.document().facts.is_empty());
        assert!(store.document().recurring_tasks.is_empty());
        assert!(store.document().observed_patterns.is_empty());
    }

    /// `to_json` returns a JSON object with the expected top-level keys.
    #[test]
    fn to_json_returns_object_with_schema_keys() {
        let store = IdentityMemory::in_memory();
        let json = store.to_json();
        assert!(json.is_object(), "to_json must return an object");
        let obj = json.as_object().unwrap();
        assert!(obj.contains_key("schema_version"));
        assert!(obj.contains_key("user_preferences"));
        assert!(obj.contains_key("recurring_tasks"));
        assert!(obj.contains_key("observed_patterns"));
        assert!(obj.contains_key("system_policies"));
        assert!(obj.contains_key("agent_self_model"));
        assert!(obj.contains_key("facts"));
    }

    /// `IdentityDocument::from_json` round-trips through `to_json`.
    #[test]
    fn identity_document_round_trips_through_json() {
        let mut doc = IdentityDocument::default();
        doc.agent_self_model.name = Some("TestAgent".to_owned());
        doc.user_preferences.language = Some("fr".to_owned());
        doc.facts.insert("hobby".to_owned(), "hiking".to_owned());

        let json = doc.to_json();
        let restored = IdentityDocument::from_json(&json).expect("round-trip must succeed");
        assert_eq!(restored, doc);
    }

    // S5.5.4 ─────────────────────────────────────────────────────────────────

    /// `set_fact` stores the value and emits an `IdentityUpdated` audit entry.
    #[test]
    fn set_fact_stores_value_and_emits_audit_entry() {
        let mut store = IdentityMemory::in_memory();
        let mut log = AuditLog::new();

        store
            .set_fact("name", "Alice", &mut log, "agent-1")
            .expect("set_fact must succeed");

        assert_eq!(store.get_fact("name"), Some("Alice"));
        assert_eq!(log.len(), 1);
        match &log.entries()[0] {
            AuditEntry::IdentityUpdated {
                agent_id,
                key,
                old_value,
                new_value,
            } => {
                assert_eq!(agent_id, "agent-1");
                assert_eq!(key, "name");
                assert!(old_value.is_none(), "old_value must be None for a new key");
                assert_eq!(new_value, "Alice");
            }
            other => panic!("unexpected entry: {other:?}"),
        }
    }

    /// Overwriting an existing fact records the old value in the audit entry.
    #[test]
    fn set_fact_records_old_value_on_overwrite() {
        let mut store = IdentityMemory::in_memory();
        let mut log = AuditLog::new();

        store
            .set_fact("role", "developer", &mut log, "agent-x")
            .unwrap();
        store
            .set_fact("role", "architect", &mut log, "agent-x")
            .unwrap();

        assert_eq!(log.len(), 2);
        match &log.entries()[1] {
            AuditEntry::IdentityUpdated {
                old_value,
                new_value,
                ..
            } => {
                assert_eq!(old_value.as_deref(), Some("developer"));
                assert_eq!(new_value, "architect");
            }
            other => panic!("unexpected entry: {other:?}"),
        }
    }

    /// `get_fact` returns `None` for an unknown key.
    #[test]
    fn get_fact_returns_none_for_unknown_key() {
        let store = IdentityMemory::in_memory();
        assert!(store.get_fact("nonexistent").is_none());
    }

    // S5.5.3 — file persistence ───────────────────────────────────────────────

    /// An identity store persists through a simulated process restart.
    ///
    /// This is the file-persistence analogue of the L3 archive restart test
    /// (E2.6 exit criterion 1).
    #[test]
    fn identity_store_survives_process_restart() {
        let path = std::env::temp_dir().join("animaos_test_identity_restart.json");
        let _ = std::fs::remove_file(&path);

        // First "process".
        {
            let mut store = IdentityMemory::open(&path).expect("open must succeed");
            let mut log = AuditLog::new();
            store
                .set_fact("project", "AnimaOS", &mut log, "agent-restart")
                .unwrap();
            store
                .set_fact("version", "0.1.0", &mut log, "agent-restart")
                .unwrap();
        }

        // Second "process" — re-open from disk.
        {
            let store = IdentityMemory::open(&path).expect("re-open must succeed");
            assert_eq!(store.get_fact("project"), Some("AnimaOS"));
            assert_eq!(store.get_fact("version"), Some("0.1.0"));
        }

        let _ = std::fs::remove_file(&path);
    }

    /// `open` creates the file when it does not exist.
    #[test]
    fn open_creates_file_when_absent() {
        let path = std::env::temp_dir().join("animaos_test_identity_create.json");
        let _ = std::fs::remove_file(&path);

        assert!(!path.exists(), "test pre-condition: file must not exist");
        IdentityMemory::open(&path).expect("open must create the file");
        assert!(path.exists(), "open must create the file");

        let _ = std::fs::remove_file(&path);
    }

    /// `open` rejects a file with invalid JSON.
    #[test]
    fn open_rejects_invalid_json() {
        let path = std::env::temp_dir().join("animaos_test_identity_bad.json");
        std::fs::write(&path, b"not-json").unwrap();
        let err = IdentityMemory::open(&path).expect_err("must fail on bad JSON");
        assert!(matches!(err, IdentityError::ParseError(_)));
        let _ = std::fs::remove_file(&path);
    }

    // S5.5.5 — router integration ─────────────────────────────────────────────

    /// Identity is injected into `InvokeRequest::identity` as a distinct JSON
    /// object and is recoverable as an `IdentityDocument` on the cortex side.
    ///
    /// Satisfies E5.5 exit criterion 3: "identity facts loaded at invocation
    /// time are visible in the cortex's prompt assembly as a distinct section,
    /// not concatenated with task context."
    #[test]
    fn identity_is_injectable_as_distinct_json_section() {
        let mut store = IdentityMemory::in_memory();
        let mut log = AuditLog::new();
        store
            .set_fact("preferred_language", "Rust", &mut log, "agent-y")
            .unwrap();
        store.document_mut().agent_self_model.name = Some("TestBot".to_owned());

        let json = store.to_json();

        // The JSON must be an object (not a string) — a distinct section.
        assert!(json.is_object(), "identity must be a distinct JSON object");

        // The cortex can recover the document.
        let recovered = IdentityDocument::from_json(&json).expect("recovery must succeed");
        assert_eq!(
            recovered
                .facts
                .get("preferred_language")
                .map(String::as_str),
            Some("Rust")
        );
        assert_eq!(recovered.agent_self_model.name.as_deref(), Some("TestBot"));
    }

    /// `default_path` returns the expected path for a given agent ID.
    #[test]
    fn default_path_returns_expected_path() {
        let path = IdentityMemory::default_path("my-agent");
        assert!(path.ends_with("my-agent/identity.json"));
        assert!(path.to_string_lossy().contains(".anima"));
    }

    // E5.5 exit criterion 1 ───────────────────────────────────────────────────

    /// `anima identity show` and `anima identity set` round-trip: set a fact,
    /// read it back, and confirm the audit log carries both the key and value.
    ///
    /// This is the programmatic analogue of the CLI round-trip test.
    #[test]
    fn anima_identity_show_and_set_round_trip_through_audit_log() {
        let mut store = IdentityMemory::in_memory();
        let mut log = AuditLog::new();
        const AGENT: &str = "anima";

        // `anima identity set timezone "America/New_York"`
        store
            .set_fact("timezone", "America/New_York", &mut log, AGENT)
            .unwrap();

        // `anima identity show timezone`
        let shown = store.get_fact("timezone");
        assert_eq!(
            shown,
            Some("America/New_York"),
            "show must return the set value"
        );

        // Audit trail must carry the change.
        let entry = log
            .entries()
            .iter()
            .find(|e| matches!(e, AuditEntry::IdentityUpdated { key, .. } if key == "timezone"));
        assert!(
            entry.is_some(),
            "audit log must contain an IdentityUpdated entry for 'timezone'"
        );

        if let Some(AuditEntry::IdentityUpdated { new_value, .. }) = entry {
            assert_eq!(new_value, "America/New_York");
        }
    }
}
