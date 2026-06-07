#![forbid(unsafe_code)]

//! Persistent session store with atomic JSON writes — E22 S22.2.
//!
//! [`SessionStore`] holds a collection of [`SessionRecord`]s keyed by session
//! id and persists them to a JSON file. Writes are atomic: the data is written
//! to a `.tmp` sibling and then renamed, so a crash never leaves a partial file.
//!
//! Default path: `~/.anima/<agent_id>/sessions.json`

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::record::{ConversationTurn, SessionError, SessionRecord, SessionStatus};

// ── SessionQuery ──────────────────────────────────────────────────────────────

/// Filtering criteria for [`SessionStore::list`].
#[derive(Debug, Default, Clone)]
pub struct SessionQuery {
    /// If set, only return sessions owned by this user.
    pub user_id: Option<String>,
    /// If set, only return sessions with this status.
    pub status: Option<SessionStatus>,
    /// If set, only return sessions whose content matches this substring.
    pub content_query: Option<String>,
    /// Maximum number of results to return (0 = unlimited).
    pub limit: usize,
}

impl SessionQuery {
    /// Return sessions for a specific user.
    pub fn for_user(user_id: impl Into<String>) -> Self {
        Self {
            user_id: Some(user_id.into()),
            ..Default::default()
        }
    }

    /// Return sessions matching a text query.
    pub fn with_content(query: impl Into<String>) -> Self {
        Self {
            content_query: Some(query.into()),
            ..Default::default()
        }
    }

    /// Apply a maximum result limit.
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// Filter to active sessions only.
    pub fn active_only(mut self) -> Self {
        self.status = Some(SessionStatus::Active);
        self
    }

    fn matches(&self, session: &SessionRecord) -> bool {
        if let Some(uid) = &self.user_id {
            if &session.user_id != uid {
                return false;
            }
        }
        if let Some(status) = &self.status {
            if &session.status != status {
                return false;
            }
        }
        if let Some(q) = &self.content_query {
            if !session.matches_query(q) {
                return false;
            }
        }
        true
    }
}

// ── ExportFormat ──────────────────────────────────────────────────────────────

/// Output format for session export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportFormat {
    /// One JSON object per line (JSONL / NDJSON).
    Jsonl,
    /// Human-readable Markdown transcript.
    Markdown,
}

impl std::str::FromStr for ExportFormat {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "jsonl" | "ndjson" => Ok(ExportFormat::Jsonl),
            "markdown" | "md" => Ok(ExportFormat::Markdown),
            other => Err(format!("unknown export format: {other}")),
        }
    }
}

impl std::fmt::Display for ExportFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExportFormat::Jsonl => write!(f, "jsonl"),
            ExportFormat::Markdown => write!(f, "markdown"),
        }
    }
}

// ── on-disk schema ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct StoreFile {
    schema_version: u32,
    sessions: HashMap<String, SessionRecord>,
}

// ── SessionStore ──────────────────────────────────────────────────────────────

/// A durable collection of [`SessionRecord`]s for one agent.
///
/// Use [`SessionStore::open`] for a file-backed store (production) or
/// [`SessionStore::in_memory`] for a transient store (tests).
pub struct SessionStore {
    sessions: HashMap<String, SessionRecord>,
    path: Option<PathBuf>,
}

impl SessionStore {
    // ── constructors ──────────────────────────────────────────────────────────

    /// Load (or create) a store at `path`.
    ///
    /// If the file does not exist an empty store is returned; if it exists but
    /// cannot be parsed an `Io` error is returned.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SessionError> {
        let path = path.as_ref().to_path_buf();
        if path.exists() {
            let raw =
                std::fs::read_to_string(&path).map_err(|e| SessionError::Io(e.to_string()))?;
            let file: StoreFile =
                serde_json::from_str(&raw).map_err(|e| SessionError::Io(e.to_string()))?;
            Ok(Self {
                sessions: file.sessions,
                path: Some(path),
            })
        } else {
            Ok(Self {
                sessions: HashMap::new(),
                path: Some(path),
            })
        }
    }

    /// Create a transient in-memory store (no disk I/O).
    pub fn in_memory() -> Self {
        Self {
            sessions: HashMap::new(),
            path: None,
        }
    }

    /// Derive the default file path for `agent_id`.
    ///
    /// `~/.anima/<agent_id>/sessions.json`
    pub fn default_path(agent_id: &str) -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(home)
            .join(".anima")
            .join(agent_id)
            .join("sessions.json")
    }

    // ── persistence ───────────────────────────────────────────────────────────

    /// Flush the store to disk (no-op for in-memory stores).
    ///
    /// Uses a write-to-`.tmp`-then-rename strategy so a crash never leaves a
    /// corrupted file.
    pub fn flush(&self) -> Result<(), SessionError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let file = StoreFile {
            schema_version: 1,
            sessions: self.sessions.clone(),
        };
        let json =
            serde_json::to_string_pretty(&file).map_err(|e| SessionError::Io(e.to_string()))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| SessionError::Io(e.to_string()))?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &json).map_err(|e| SessionError::Io(e.to_string()))?;
        std::fs::rename(&tmp, path).map_err(|e| SessionError::Io(e.to_string()))?;
        Ok(())
    }

    // ── CRUD ──────────────────────────────────────────────────────────────────

    /// Insert a new session.
    ///
    /// Returns `SessionError::AlreadyExists` if the id is already present.
    pub fn insert(&mut self, session: SessionRecord) -> Result<(), SessionError> {
        if self.sessions.contains_key(&session.id) {
            return Err(SessionError::AlreadyExists {
                session_id: session.id.clone(),
            });
        }
        self.sessions.insert(session.id.clone(), session);
        Ok(())
    }

    /// Retrieve a shared reference to a session by id.
    pub fn get(&self, session_id: &str) -> Option<&SessionRecord> {
        self.sessions.get(session_id)
    }

    /// Retrieve a mutable reference to a session by id.
    pub fn get_mut(&mut self, session_id: &str) -> Option<&mut SessionRecord> {
        self.sessions.get_mut(session_id)
    }

    /// Append a turn to an existing session and flush.
    ///
    /// Returns `SessionError::NotFound` if the session does not exist.
    pub fn append_turn(
        &mut self,
        session_id: &str,
        turn: ConversationTurn,
    ) -> Result<(), SessionError> {
        let session = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| SessionError::NotFound {
                session_id: session_id.to_string(),
            })?;
        session.append_turn(turn)?;
        self.flush()
    }

    /// Archive a session with an optional summary and flush.
    pub fn archive(
        &mut self,
        session_id: &str,
        summary: Option<String>,
    ) -> Result<(), SessionError> {
        let session = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| SessionError::NotFound {
                session_id: session_id.to_string(),
            })?;
        session.archive(summary)?;
        self.flush()
    }

    /// Mark a session deleted and flush.
    pub fn delete(&mut self, session_id: &str) -> Result<(), SessionError> {
        let session = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| SessionError::NotFound {
                session_id: session_id.to_string(),
            })?;
        session.delete();
        self.flush()
    }

    // ── query ─────────────────────────────────────────────────────────────────

    /// Return sessions matching the query, sorted by `started_at_ns` descending.
    ///
    /// Deleted sessions are excluded unless the query explicitly requests them
    /// via `status = Some(SessionStatus::Deleted)`.
    pub fn list(&self, query: &SessionQuery) -> Vec<&SessionRecord> {
        let exclude_deleted =
            query.status.as_ref() != Some(&SessionStatus::Deleted) && query.status.is_none();

        let mut results: Vec<&SessionRecord> = self
            .sessions
            .values()
            .filter(|s| {
                if exclude_deleted && s.status == SessionStatus::Deleted {
                    return false;
                }
                query.matches(s)
            })
            .collect();

        results.sort_by(|a, b| b.started_at_ns.cmp(&a.started_at_ns));

        if query.limit > 0 && results.len() > query.limit {
            results.truncate(query.limit);
        }
        results
    }

    /// Total number of sessions (including deleted).
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// `true` when the store contains no sessions.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    // ── export ────────────────────────────────────────────────────────────────

    /// Render a session as a string in the requested format.
    ///
    /// Returns `SessionError::NotFound` if the session does not exist.
    pub fn export(&self, session_id: &str, format: &ExportFormat) -> Result<String, SessionError> {
        let session = self
            .sessions
            .get(session_id)
            .ok_or_else(|| SessionError::NotFound {
                session_id: session_id.to_string(),
            })?;
        match format {
            ExportFormat::Jsonl => export_jsonl(session),
            ExportFormat::Markdown => Ok(export_markdown(session)),
        }
    }
}

// ── export helpers ────────────────────────────────────────────────────────────

fn export_jsonl(session: &SessionRecord) -> Result<String, SessionError> {
    let mut lines = Vec::new();
    for turn in &session.turns {
        let line = serde_json::to_string(turn).map_err(|e| SessionError::Io(e.to_string()))?;
        lines.push(line);
    }
    Ok(lines.join("\n"))
}

fn export_markdown(session: &SessionRecord) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Session {}\n\n", session.id));
    out.push_str(&format!("**User:** {}\n", session.user_id));
    out.push_str(&format!("**Agent:** {}\n", session.agent_id));
    out.push_str(&format!("**Status:** {}\n\n", session.status));

    if let Some(summary) = &session.summary {
        out.push_str("## Summary\n\n");
        out.push_str(summary);
        out.push_str("\n\n");
    }

    out.push_str("## Transcript\n\n");
    for turn in &session.turns {
        let role_label = match turn.role {
            crate::record::ConversationRole::User => "**User**",
            crate::record::ConversationRole::Assistant => "**Assistant**",
            crate::record::ConversationRole::System => "_System_",
            crate::record::ConversationRole::Tool => "_Tool_",
        };
        out.push_str(&format!("### Turn {} — {role_label}\n\n", turn.index));
        out.push_str(&turn.content);
        if !turn.tool_calls.is_empty() {
            out.push_str("\n\n_Tool calls: ");
            out.push_str(&turn.tool_calls.join(", "));
            out.push('_');
        }
        out.push_str("\n\n");
    }
    out
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{ConversationRole, ConversationTurn, SessionRecord};

    fn sample_session(id: &str, user: &str) -> SessionRecord {
        SessionRecord::new(id, user, "agent-a")
    }

    fn turn(role: ConversationRole, content: &str) -> ConversationTurn {
        ConversationTurn::new(0, role, content)
    }

    // ── basic CRUD ────────────────────────────────────────────────────────────

    #[test]
    fn empty_store_has_zero_sessions() {
        let store = SessionStore::in_memory();
        assert_eq!(store.len(), 0);
        assert!(store.is_empty());
    }

    #[test]
    fn insert_adds_session() {
        let mut store = SessionStore::in_memory();
        store.insert(sample_session("s1", "user:alice")).unwrap();
        assert_eq!(store.len(), 1);
        assert!(store.get("s1").is_some());
    }

    #[test]
    fn insert_rejects_duplicate_id() {
        let mut store = SessionStore::in_memory();
        store.insert(sample_session("s1", "user:alice")).unwrap();
        let res = store.insert(sample_session("s1", "user:bob"));
        assert_eq!(
            res,
            Err(SessionError::AlreadyExists {
                session_id: "s1".to_string()
            })
        );
    }

    #[test]
    fn get_returns_none_for_missing_session() {
        let store = SessionStore::in_memory();
        assert!(store.get("ghost").is_none());
    }

    #[test]
    fn append_turn_persists_turn() {
        let mut store = SessionStore::in_memory();
        store.insert(sample_session("s1", "user:alice")).unwrap();
        store
            .append_turn("s1", turn(ConversationRole::User, "hello"))
            .unwrap();
        assert_eq!(store.get("s1").unwrap().turn_count(), 1);
    }

    #[test]
    fn append_turn_returns_error_for_missing_session() {
        let mut store = SessionStore::in_memory();
        let res = store.append_turn("nope", turn(ConversationRole::User, "hi"));
        assert_eq!(
            res,
            Err(SessionError::NotFound {
                session_id: "nope".to_string()
            })
        );
    }

    #[test]
    fn archive_session_with_summary() {
        let mut store = SessionStore::in_memory();
        store.insert(sample_session("s1", "user:alice")).unwrap();
        store.archive("s1", Some("Nice chat".to_string())).unwrap();
        let s = store.get("s1").unwrap();
        assert_eq!(s.status, SessionStatus::Archived);
        assert_eq!(s.summary.as_deref(), Some("Nice chat"));
    }

    #[test]
    fn delete_session_marks_deleted() {
        let mut store = SessionStore::in_memory();
        store.insert(sample_session("s1", "user:alice")).unwrap();
        store.delete("s1").unwrap();
        assert_eq!(store.get("s1").unwrap().status, SessionStatus::Deleted);
    }

    // ── list / query ──────────────────────────────────────────────────────────

    #[test]
    fn list_excludes_deleted_by_default() {
        let mut store = SessionStore::in_memory();
        store.insert(sample_session("s1", "user:alice")).unwrap();
        store.insert(sample_session("s2", "user:alice")).unwrap();
        store.delete("s2").unwrap();
        let results = store.list(&SessionQuery::default());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "s1");
    }

    #[test]
    fn list_filters_by_user_id() {
        let mut store = SessionStore::in_memory();
        store.insert(sample_session("s1", "user:alice")).unwrap();
        store.insert(sample_session("s2", "user:bob")).unwrap();
        let results = store.list(&SessionQuery::for_user("user:alice"));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].user_id, "user:alice");
    }

    #[test]
    fn list_filters_by_content_query() {
        let mut store = SessionStore::in_memory();
        store.insert(sample_session("s1", "user:alice")).unwrap();
        store
            .append_turn("s1", turn(ConversationRole::User, "discuss metrics"))
            .unwrap();
        store.insert(sample_session("s2", "user:alice")).unwrap();
        store
            .append_turn("s2", turn(ConversationRole::User, "discuss recipes"))
            .unwrap();
        let q = SessionQuery::with_content("metrics");
        let results = store.list(&q);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "s1");
    }

    #[test]
    fn list_respects_limit() {
        let mut store = SessionStore::in_memory();
        for i in 0..5 {
            store
                .insert(sample_session(&format!("s{i}"), "user:alice"))
                .unwrap();
        }
        let q = SessionQuery::default().with_limit(3);
        assert_eq!(store.list(&q).len(), 3);
    }

    #[test]
    fn list_active_only_excludes_archived() {
        let mut store = SessionStore::in_memory();
        store.insert(sample_session("s1", "user:alice")).unwrap();
        store.insert(sample_session("s2", "user:alice")).unwrap();
        store.archive("s2", None).unwrap();
        let results = store.list(&SessionQuery::default().active_only());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "s1");
    }

    // ── persistence ───────────────────────────────────────────────────────────

    #[test]
    fn flush_and_reload_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.json");

        let mut store = SessionStore::open(&path).unwrap();
        store.insert(sample_session("s1", "user:alice")).unwrap();
        store
            .append_turn("s1", turn(ConversationRole::User, "persistent"))
            .unwrap();
        store.flush().unwrap();

        let store2 = SessionStore::open(&path).unwrap();
        let s = store2.get("s1").unwrap();
        assert_eq!(s.turn_count(), 1);
        assert_eq!(s.turns[0].content, "persistent");
    }

    #[test]
    fn open_creates_empty_store_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.json");
        let store = SessionStore::open(&path).unwrap();
        assert!(store.is_empty());
    }

    #[test]
    fn in_memory_flush_is_no_op() {
        let mut store = SessionStore::in_memory();
        store.insert(sample_session("s1", "user:alice")).unwrap();
        store.flush().unwrap(); // must not panic
    }

    // ── export ────────────────────────────────────────────────────────────────

    #[test]
    fn export_jsonl_produces_one_line_per_turn() {
        let mut store = SessionStore::in_memory();
        store.insert(sample_session("s1", "user:alice")).unwrap();
        store
            .append_turn("s1", turn(ConversationRole::User, "line1"))
            .unwrap();
        store
            .append_turn("s1", turn(ConversationRole::Assistant, "line2"))
            .unwrap();
        let out = store.export("s1", &ExportFormat::Jsonl).unwrap();
        let lines: Vec<_> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("line1"));
        assert!(lines[1].contains("line2"));
    }

    #[test]
    fn export_markdown_contains_session_id_and_turns() {
        let mut store = SessionStore::in_memory();
        store.insert(sample_session("s1", "user:alice")).unwrap();
        store
            .append_turn("s1", turn(ConversationRole::User, "hello there"))
            .unwrap();
        let md = store.export("s1", &ExportFormat::Markdown).unwrap();
        assert!(md.contains("s1"));
        assert!(md.contains("hello there"));
        assert!(md.contains("**User**"));
    }

    #[test]
    fn export_markdown_includes_summary_when_archived() {
        let mut store = SessionStore::in_memory();
        store.insert(sample_session("s1", "user:alice")).unwrap();
        store
            .archive("s1", Some("Key takeaway".to_string()))
            .unwrap();
        let md = store.export("s1", &ExportFormat::Markdown).unwrap();
        assert!(md.contains("Key takeaway"));
    }

    #[test]
    fn export_returns_not_found_for_missing_session() {
        let store = SessionStore::in_memory();
        let res = store.export("ghost", &ExportFormat::Jsonl);
        assert_eq!(
            res,
            Err(SessionError::NotFound {
                session_id: "ghost".to_string()
            })
        );
    }

    #[test]
    fn export_format_from_str_round_trip() {
        assert_eq!(
            "jsonl".parse::<ExportFormat>().unwrap(),
            ExportFormat::Jsonl
        );
        assert_eq!(
            "markdown".parse::<ExportFormat>().unwrap(),
            ExportFormat::Markdown
        );
        assert_eq!(
            "md".parse::<ExportFormat>().unwrap(),
            ExportFormat::Markdown
        );
        assert!("csv".parse::<ExportFormat>().is_err());
    }
}
