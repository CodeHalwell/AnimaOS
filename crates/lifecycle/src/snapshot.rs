//! S15.5 — State versioning & migration.
//!
//! An [`AgentSnapshot`] is a schema-versioned, self-describing record of the
//! whole agent self that can be saved to disk and restored after an AnimaOS
//! upgrade.  Snapshots are:
//!
//! - **The unit of backup/restore**: save with [`AgentSnapshot::save`], load
//!   with [`AgentSnapshot::load`], and the agent resumes from where it left off.
//! - **The unit of host migration**: a snapshot created on a container can be
//!   restored on a bare-metal target (modulo driver availability).
//! - **The substrate for the digital twin** (S15.4): the twin is initialized
//!   from a snapshot of the real agent.
//! - **The substrate for agent-level rollback** (E14): a failed self-improvement
//!   cycle rolls back by restoring a pre-cycle snapshot.
//!
//! ## Schema versioning
//!
//! [`SNAPSHOT_SCHEMA_VERSION`] is incremented whenever the on-disk layout
//! changes in a backward-incompatible way.  [`AgentSnapshot::migrate`] applies
//! all pending schema upgrades so a snapshot from any prior version can be
//! used after loading.
//!
//! ## Atomicity
//!
//! [`AgentSnapshot::save`] writes to a `.tmp` sibling first, then renames into
//! the final path.  On success the final path is always complete; on failure
//! only the `.tmp` file may exist.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use vita::audit::AuditEntry;

// ── Schema version ────────────────────────────────────────────────────────────

/// Current schema version for [`AgentSnapshot`].
///
/// Increment this when the on-disk layout changes in a backward-incompatible
/// way and add a migration arm in [`AgentSnapshot::migrate`].
pub const SNAPSHOT_SCHEMA_VERSION: u32 = 1;

// ── AgentSnapshot ─────────────────────────────────────────────────────────────

/// A versioned, self-describing snapshot of the whole agent state.
///
/// Fields marked optional are absent in older schema versions or when the
/// corresponding subsystem is not configured (e.g. no identity store, no L3
/// archive).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSnapshot {
    /// Schema version that was current when this snapshot was created.
    pub schema_version: u32,
    /// Stable agent identifier.
    pub agent_id: String,
    /// Wall-clock creation time (nanoseconds since Unix epoch).
    pub created_at_ns: u64,
    /// AnimaOS crate version string (e.g. `"0.1.0"`).
    pub animaos_version: String,
    /// Agent description / persona tag.
    pub description: Option<String>,
    /// Serialised identity document (JSON object), if configured.
    pub identity: Option<serde_json::Value>,
    /// Aggregate statistics derived from the audit log at snapshot time.
    pub audit_summary: AuditSummary,
    /// Human-readable annotation about why this snapshot was taken.
    pub reason: Option<String>,
}

// ── AuditSummary ─────────────────────────────────────────────────────────────

/// Aggregate statistics from the audit log captured at snapshot time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditSummary {
    /// Total number of audit entries at snapshot time.
    pub entry_count: usize,
    /// Number of tasks that completed successfully.
    pub tasks_completed: usize,
    /// Number of tasks that failed.
    pub tasks_failed: usize,
    /// Total tokens emitted by the agent.
    pub total_tokens_emitted: u64,
    /// Number of cortex invocations.
    pub cortex_invocations: usize,
    /// Number of sleep cycles entered.
    pub sleep_cycles: usize,
}

impl AuditSummary {
    fn from_entries(entries: &[AuditEntry]) -> Self {
        let mut s = AuditSummary {
            entry_count: entries.len(),
            tasks_completed: 0,
            tasks_failed: 0,
            total_tokens_emitted: 0,
            cortex_invocations: 0,
            sleep_cycles: 0,
        };
        for e in entries {
            match e {
                AuditEntry::TaskCompleted { tokens_emitted, .. } => {
                    s.tasks_completed += 1;
                    s.total_tokens_emitted += *tokens_emitted as u64;
                }
                AuditEntry::TaskFailed { .. } => s.tasks_failed += 1,
                AuditEntry::CortexInvoked { .. } => s.cortex_invocations += 1,
                AuditEntry::SleepEntered { .. } => s.sleep_cycles += 1,
                _ => {}
            }
        }
        s
    }
}

// ── MigrationError ────────────────────────────────────────────────────────────

/// Error returned by [`AgentSnapshot::migrate`] when migration fails.
#[derive(Debug)]
pub struct MigrationError {
    pub from_version: u32,
    pub message: String,
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "snapshot migration from v{} failed: {}",
            self.from_version, self.message
        )
    }
}

impl std::error::Error for MigrationError {}

// ── SnapshotError ─────────────────────────────────────────────────────────────

/// Error returned by snapshot I/O operations.
#[derive(Debug)]
pub enum SnapshotError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Migration(MigrationError),
    SchemaTooNew { found: u32, supported: u32 },
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SnapshotError::Io(e) => write!(f, "snapshot I/O error: {}", e),
            SnapshotError::Json(e) => write!(f, "snapshot JSON error: {}", e),
            SnapshotError::Migration(e) => write!(f, "{}", e),
            SnapshotError::SchemaTooNew { found, supported } => write!(
                f,
                "snapshot schema v{} is newer than this AnimaOS version (supports up to v{})",
                found, supported
            ),
        }
    }
}

impl std::error::Error for SnapshotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SnapshotError::Io(e) => Some(e),
            SnapshotError::Json(e) => Some(e),
            SnapshotError::Migration(e) => Some(e),
            SnapshotError::SchemaTooNew { .. } => None,
        }
    }
}

impl From<std::io::Error> for SnapshotError {
    fn from(e: std::io::Error) -> Self {
        SnapshotError::Io(e)
    }
}

impl From<serde_json::Error> for SnapshotError {
    fn from(e: serde_json::Error) -> Self {
        SnapshotError::Json(e)
    }
}

// ── AgentSnapshot impl ────────────────────────────────────────────────────────

impl AgentSnapshot {
    /// Capture a snapshot of the current agent state.
    ///
    /// # Parameters
    ///
    /// - `agent_id`: stable agent identifier.
    /// - `identity`: serialised identity document, or `None` if no identity
    ///   store is configured.
    /// - `audit_entries`: all entries from the current audit log.
    /// - `reason`: optional human-readable annotation.
    pub fn capture(
        agent_id: &str,
        identity: Option<serde_json::Value>,
        audit_entries: &[AuditEntry],
        reason: Option<String>,
    ) -> Self {
        AgentSnapshot {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            agent_id: agent_id.to_string(),
            created_at_ns: now_ns(),
            animaos_version: env!("CARGO_PKG_VERSION").to_string(),
            description: None,
            identity,
            audit_summary: AuditSummary::from_entries(audit_entries),
            reason,
        }
    }

    /// Write the snapshot to `path` atomically.
    ///
    /// The snapshot is serialised as JSON and written to `<path>.tmp` before
    /// being renamed to `path`.  If the write fails, `<path>.tmp` may exist
    /// but `path` is unchanged.
    pub fn save(&self, path: &Path) -> Result<(), SnapshotError> {
        let json = serde_json::to_string_pretty(self)?;
        let tmp = tmp_path(path);
        std::fs::write(&tmp, &json)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Load a snapshot from `path` and apply any pending schema migrations.
    ///
    /// Returns an error when:
    /// - The file cannot be read or parsed.
    /// - The snapshot's `schema_version` exceeds [`SNAPSHOT_SCHEMA_VERSION`].
    /// - A migration step fails.
    pub fn load(path: &Path) -> Result<Self, SnapshotError> {
        let bytes = std::fs::read(path)?;
        let snap: AgentSnapshot = serde_json::from_slice(&bytes)?;
        if snap.schema_version > SNAPSHOT_SCHEMA_VERSION {
            return Err(SnapshotError::SchemaTooNew {
                found: snap.schema_version,
                supported: SNAPSHOT_SCHEMA_VERSION,
            });
        }
        snap.migrate().map_err(SnapshotError::Migration)
    }

    /// Apply schema migrations until the snapshot is at
    /// [`SNAPSHOT_SCHEMA_VERSION`].
    ///
    /// A no-op when `self.schema_version == SNAPSHOT_SCHEMA_VERSION`.
    pub fn migrate(self) -> Result<Self, MigrationError> {
        // Add migration arms here when SNAPSHOT_SCHEMA_VERSION is incremented.
        // Pattern:
        //   if self.schema_version == N {
        //       // transform fields
        //       self.schema_version = N + 1;
        //   }
        if self.schema_version > SNAPSHOT_SCHEMA_VERSION {
            return Err(MigrationError {
                from_version: self.schema_version,
                message: format!(
                    "version {} > current {}",
                    self.schema_version, SNAPSHOT_SCHEMA_VERSION
                ),
            });
        }
        // Already at current version — nothing to do.
        Ok(self)
    }

    /// Default save path for an agent snapshot.
    ///
    /// Returns `~/.anima/<agent_id>/snapshot.json` when `$HOME` is set.
    pub fn default_path(agent_id: &str) -> Option<PathBuf> {
        dirs_home().map(|h| h.join(".anima").join(agent_id).join("snapshot.json"))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn now_ns() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut p = path.to_path_buf();
    let ext = p
        .extension()
        .map(|e| format!("{}.tmp", e.to_string_lossy()))
        .unwrap_or_else(|| "tmp".to_string());
    p.set_extension(ext);
    p
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use vita::audit::AuditEntry;

    fn completed(agent: &str, id: u64, tokens: u32) -> AuditEntry {
        AuditEntry::TaskCompleted {
            agent_id: agent.to_string(),
            task_id: id,
            tokens_emitted: tokens,
            response: "ok".to_string(),
        }
    }

    fn failed(agent: &str, id: u64) -> AuditEntry {
        AuditEntry::TaskFailed {
            agent_id: agent.to_string(),
            task_id: id,
            error: "err".to_string(),
        }
    }

    fn sleep_entered(agent: &str) -> AuditEntry {
        AuditEntry::SleepEntered {
            agent_id: agent.to_string(),
        }
    }

    fn cortex_invoked(id: &str) -> AuditEntry {
        AuditEntry::CortexInvoked {
            task_id: id.to_string(),
            latency_to_first_action_ms: 10,
        }
    }

    #[test]
    fn snapshot_schema_version_is_current() {
        let snap = AgentSnapshot::capture("a", None, &[], None);
        assert_eq!(snap.schema_version, SNAPSHOT_SCHEMA_VERSION);
    }

    #[test]
    fn snapshot_captures_agent_id() {
        let snap = AgentSnapshot::capture("my-agent", None, &[], None);
        assert_eq!(snap.agent_id, "my-agent");
    }

    #[test]
    fn audit_summary_counts_tasks() {
        let entries = vec![
            completed("a", 1, 100),
            completed("a", 2, 50),
            failed("a", 3),
            sleep_entered("a"),
            cortex_invoked("c1"),
        ];
        let snap = AgentSnapshot::capture("a", None, &entries, None);
        assert_eq!(snap.audit_summary.tasks_completed, 2);
        assert_eq!(snap.audit_summary.tasks_failed, 1);
        assert_eq!(snap.audit_summary.total_tokens_emitted, 150);
        assert_eq!(snap.audit_summary.sleep_cycles, 1);
        assert_eq!(snap.audit_summary.cortex_invocations, 1);
        assert_eq!(snap.audit_summary.entry_count, 5);
    }

    #[test]
    fn snapshot_stores_identity() {
        let id = serde_json::json!({"name": "Alice", "role": "researcher"});
        let snap = AgentSnapshot::capture("a", Some(id.clone()), &[], None);
        assert_eq!(snap.identity, Some(id));
    }

    #[test]
    fn snapshot_stores_reason() {
        let snap = AgentSnapshot::capture("a", None, &[], Some("pre-upgrade".to_string()));
        assert_eq!(snap.reason.as_deref(), Some("pre-upgrade"));
    }

    #[test]
    fn snapshot_survives_round_trip_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snap.json");

        let entries = vec![completed("a", 1, 42)];
        let original = AgentSnapshot::capture("agent-rt", None, &entries, None);
        original.save(&path).expect("save");

        let loaded = AgentSnapshot::load(&path).expect("load");
        assert_eq!(loaded.agent_id, "agent-rt");
        assert_eq!(loaded.schema_version, SNAPSHOT_SCHEMA_VERSION);
        assert_eq!(loaded.audit_summary.tasks_completed, 1);
        assert_eq!(loaded.audit_summary.total_tokens_emitted, 42);
    }

    #[test]
    fn snapshot_write_is_atomic_no_tmp_file_after_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snap.json");
        let tmp = tmp_path(&path);

        let snap = AgentSnapshot::capture("a", None, &[], None);
        snap.save(&path).expect("save");

        // Final file exists, tmp file cleaned up by rename.
        assert!(path.exists(), "final path should exist");
        assert!(
            !tmp.exists(),
            "tmp file should not remain after successful save"
        );
    }

    #[test]
    fn migration_is_noop_when_already_at_current_version() {
        let snap = AgentSnapshot::capture("a", None, &[], None);
        assert_eq!(snap.schema_version, SNAPSHOT_SCHEMA_VERSION);
        let migrated = snap.clone().migrate().expect("migrate");
        assert_eq!(migrated.schema_version, SNAPSHOT_SCHEMA_VERSION);
        assert_eq!(migrated.agent_id, snap.agent_id);
    }

    #[test]
    fn load_rejects_schema_version_too_new() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.json");

        let mut snap = AgentSnapshot::capture("a", None, &[], None);
        snap.schema_version = SNAPSHOT_SCHEMA_VERSION + 1;
        let json = serde_json::to_string(&snap).unwrap();
        std::fs::write(&path, json).unwrap();

        let result = AgentSnapshot::load(&path);
        match result {
            Err(SnapshotError::SchemaTooNew { found, supported }) => {
                assert_eq!(found, SNAPSHOT_SCHEMA_VERSION + 1);
                assert_eq!(supported, SNAPSHOT_SCHEMA_VERSION);
            }
            other => panic!("expected SchemaTooNew, got {:?}", other),
        }
    }

    #[test]
    fn snapshot_is_serialisable_to_json_and_back() {
        let entries = vec![completed("a", 1, 50)];
        let snap = AgentSnapshot::capture("a", Some(serde_json::json!({"k": "v"})), &entries, None);
        let json = serde_json::to_string(&snap).expect("serialise");
        let snap2: AgentSnapshot = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(snap, snap2);
    }

    #[test]
    fn load_returns_error_on_missing_file() {
        let path = PathBuf::from("/tmp/does_not_exist_lifecycle_test_xyz.json");
        assert!(AgentSnapshot::load(&path).is_err());
    }

    #[test]
    fn snapshot_with_identity_survives_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id_snap.json");

        let identity = serde_json::json!({
            "user_preferences": {"language": "en"},
            "facts": {"role": "developer"}
        });
        let snap = AgentSnapshot::capture("a", Some(identity.clone()), &[], None);
        snap.save(&path).unwrap();

        let loaded = AgentSnapshot::load(&path).unwrap();
        assert_eq!(loaded.identity, Some(identity));
    }
}
