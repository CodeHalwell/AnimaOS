#![forbid(unsafe_code)]

//! JSONL-backed feedback store with atomic persistence — E24 S24.2.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::record::FeedbackRecord;

// ── StoreError ─────────────────────────────────────────────────────────────────

/// Errors returned by [`FeedbackStore`] operations.
#[derive(Debug, PartialEq)]
pub enum StoreError {
    /// A record with this `id` already exists.
    Duplicate { id: String },
    /// Serialisation or I/O failed.
    Io(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Duplicate { id } => write!(f, "duplicate feedback id: {id}"),
            StoreError::Io(e) => write!(f, "feedback store I/O error: {e}"),
        }
    }
}

// ── On-disk schema ─────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct StoreFile {
    schema_version: u32,
    records: Vec<FeedbackRecord>,
}

// ── FeedbackStore ──────────────────────────────────────────────────────────────

/// Persisted collection of [`FeedbackRecord`]s.
///
/// Records are kept in insertion order. Persistence is atomic: every
/// [`flush`](FeedbackStore::flush) writes to a `.tmp` sibling and then renames,
/// so a crash never corrupts the store file.
pub struct FeedbackStore {
    records: Vec<FeedbackRecord>,
    path: Option<PathBuf>,
}

impl FeedbackStore {
    /// Default path: `~/.anima/<agent_id>/feedback.json`
    pub fn default_path(agent_id: &str) -> PathBuf {
        jsonstore::agent_state_path(agent_id, "feedback.json")
    }

    /// Opens an existing store file or creates an empty one.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if !path.exists() {
            return Ok(Self {
                records: Vec::new(),
                path: Some(path.to_path_buf()),
            });
        }
        let raw = std::fs::read_to_string(path).map_err(|e| StoreError::Io(e.to_string()))?;
        let file: StoreFile =
            serde_json::from_str(&raw).map_err(|e| StoreError::Io(format!("parse error: {e}")))?;
        Ok(Self {
            records: file.records,
            path: Some(path.to_path_buf()),
        })
    }

    /// Creates a transient in-memory store (used in tests and non-interactive mode).
    pub fn in_memory() -> Self {
        Self {
            records: Vec::new(),
            path: None,
        }
    }

    /// Records a new feedback entry.
    ///
    /// Returns `Err(StoreError::Duplicate)` if a record with the same `id`
    /// already exists (idempotency guard for retry scenarios).
    pub fn record(&mut self, feedback: FeedbackRecord) -> Result<(), StoreError> {
        if self.records.iter().any(|r| r.id == feedback.id) {
            return Err(StoreError::Duplicate { id: feedback.id });
        }
        self.records.push(feedback);
        Ok(())
    }

    /// All records in insertion order.
    pub fn list(&self) -> &[FeedbackRecord] {
        &self.records
    }

    /// Records for a specific user, in insertion order.
    pub fn list_for_user<'a>(&'a self, user_id: &str) -> Vec<&'a FeedbackRecord> {
        self.records
            .iter()
            .filter(|r| r.user_id == user_id)
            .collect()
    }

    /// Records for a specific cortex invocation.
    pub fn list_for_invocation<'a>(&'a self, invocation_id: &str) -> Vec<&'a FeedbackRecord> {
        self.records
            .iter()
            .filter(|r| r.invocation_id == invocation_id)
            .collect()
    }

    /// Number of stored records.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Atomically persists all records to disk.
    ///
    /// Does nothing for in-memory stores.
    pub fn flush(&self) -> Result<(), StoreError> {
        let path = match &self.path {
            Some(p) => p,
            None => return Ok(()),
        };

        let file = StoreFile {
            schema_version: 1,
            records: self.records.clone(),
        };
        let json = serde_json::to_string_pretty(&file)
            .map_err(|e| StoreError::Io(format!("serialise: {e}")))?;
        jsonstore::atomic_write(path, json.as_bytes())
            .map_err(|e| StoreError::Io(format!("persist: {e}")))?;
        Ok(())
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{FeedbackCategory, FeedbackRating};

    fn rec(id_suffix: u64, user: &str, inv: &str, rating: FeedbackRating) -> FeedbackRecord {
        let mut r = FeedbackRecord::new(user, inv, rating, id_suffix);
        r.id = format!("fb-{id_suffix}");
        r
    }

    #[test]
    fn empty_store_has_zero_records() {
        let store = FeedbackStore::in_memory();
        assert_eq!(store.len(), 0);
        assert!(store.is_empty());
    }

    #[test]
    fn record_adds_entry() {
        let mut store = FeedbackStore::in_memory();
        store
            .record(rec(1, "u1", "inv-1", FeedbackRating::ThumbsUp))
            .unwrap();
        assert_eq!(store.len(), 1);
        assert!(!store.is_empty());
    }

    #[test]
    fn duplicate_id_returns_error() {
        let mut store = FeedbackStore::in_memory();
        store
            .record(rec(1, "u1", "inv-1", FeedbackRating::ThumbsUp))
            .unwrap();
        let result = store.record(rec(1, "u1", "inv-1", FeedbackRating::ThumbsDown));
        assert!(matches!(result, Err(StoreError::Duplicate { .. })));
    }

    #[test]
    fn list_for_user_filters_correctly() {
        let mut store = FeedbackStore::in_memory();
        store
            .record(rec(1, "alice", "inv-1", FeedbackRating::ThumbsUp))
            .unwrap();
        store
            .record(rec(2, "bob", "inv-2", FeedbackRating::ThumbsDown))
            .unwrap();
        store
            .record(rec(3, "alice", "inv-3", FeedbackRating::Stars(5)))
            .unwrap();

        let alice = store.list_for_user("alice");
        assert_eq!(alice.len(), 2);
        assert!(alice.iter().all(|r| r.user_id == "alice"));
    }

    #[test]
    fn list_for_invocation_filters_correctly() {
        let mut store = FeedbackStore::in_memory();
        store
            .record(rec(1, "u1", "inv-x", FeedbackRating::ThumbsUp))
            .unwrap();
        store
            .record(rec(2, "u2", "inv-x", FeedbackRating::Stars(3)))
            .unwrap();
        store
            .record(rec(3, "u3", "inv-y", FeedbackRating::ThumbsDown))
            .unwrap();

        let inv_x = store.list_for_invocation("inv-x");
        assert_eq!(inv_x.len(), 2);
    }

    #[test]
    fn list_returns_insertion_order() {
        let mut store = FeedbackStore::in_memory();
        store
            .record(rec(10, "u1", "a", FeedbackRating::ThumbsUp))
            .unwrap();
        store
            .record(rec(20, "u2", "b", FeedbackRating::ThumbsDown))
            .unwrap();
        let ids: Vec<_> = store.list().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, &["fb-10", "fb-20"]);
    }

    #[test]
    fn record_with_correction_stored_correctly() {
        let mut store = FeedbackStore::in_memory();
        let r = FeedbackRecord::new("u1", "inv-1", FeedbackRating::ThumbsDown, 99)
            .with_correction("The answer should be 42");
        store.record(r).unwrap();
        let stored = &store.list()[0];
        assert!(stored.has_correction());
        assert!(stored.categories.contains(&FeedbackCategory::Corrected));
    }

    #[test]
    fn flush_and_reload_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("feedback.json");

        let mut store = FeedbackStore::open(&path).unwrap();
        store
            .record(rec(1, "u1", "inv-1", FeedbackRating::ThumbsUp))
            .unwrap();
        store
            .record(rec(2, "u2", "inv-2", FeedbackRating::Stars(3)))
            .unwrap();
        store.flush().unwrap();

        let loaded = FeedbackStore::open(&path).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.list()[0].user_id, "u1");
        assert_eq!(loaded.list()[1].user_id, "u2");
    }

    #[test]
    fn open_creates_empty_store_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        let store = FeedbackStore::open(&path).unwrap();
        assert!(store.is_empty());
    }

    #[test]
    fn in_memory_flush_is_no_op() {
        let store = FeedbackStore::in_memory();
        assert!(store.flush().is_ok());
    }
}
