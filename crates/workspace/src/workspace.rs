#![forbid(unsafe_code)]

//! Workspace profile and status — E31 S31.1.
//!
//! A [`WorkspaceProfile`] is the top-level record for a logical tenant
//! within the agent.  Each workspace has an owner (the user who created it),
//! a display name, and a lifecycle status.
//!
//! Workspace identifiers follow the format `"<slug>"` where slug is a short
//! URL-safe identifier chosen by the creator (e.g. `"acme"`, `"team-alpha"`).
//! Use [`WorkspaceProfile::make_id`] to normalise a raw name into a safe slug.

use serde::{Deserialize, Serialize};

// ── WorkspaceStatus ───────────────────────────────────────────────────────────

/// Lifecycle status of a workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStatus {
    /// The workspace is active and accepting new activity.
    #[default]
    Active,
    /// The workspace has been suspended by an operator; activity is paused.
    Suspended,
    /// The workspace has been soft-deleted; data retained per retention policy.
    Deleted,
}

impl WorkspaceStatus {
    /// Returns a human-readable label for audit entries.
    pub fn as_str(self) -> &'static str {
        match self {
            WorkspaceStatus::Active => "active",
            WorkspaceStatus::Suspended => "suspended",
            WorkspaceStatus::Deleted => "deleted",
        }
    }
}

impl std::fmt::Display for WorkspaceStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for WorkspaceStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "active" => Ok(WorkspaceStatus::Active),
            "suspended" => Ok(WorkspaceStatus::Suspended),
            "deleted" => Ok(WorkspaceStatus::Deleted),
            other => Err(format!("unknown workspace status: {other:?}")),
        }
    }
}

// ── WorkspaceProfile ──────────────────────────────────────────────────────────

/// The identity record for a workspace.
///
/// Workspace IDs are lowercase alphanumeric slugs with hyphens allowed
/// (e.g. `"acme"`, `"team-alpha"`).  Use [`WorkspaceProfile::make_id`] to
/// normalise a raw display name into a safe slug.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceProfile {
    /// Stable identifier slug (lowercase alphanumeric + hyphens).
    pub workspace_id: String,
    /// Human-readable display name (operator-editable).
    pub display_name: String,
    /// `user_id` of the user who created and owns this workspace.
    pub owner_user_id: String,
    /// Optional description of the workspace's purpose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Lifecycle status.
    #[serde(default)]
    pub status: WorkspaceStatus,
    /// Unix nanoseconds when the workspace was created.
    pub created_at_ns: u64,
    /// Unix nanoseconds when the workspace was last modified.
    pub updated_at_ns: u64,
    /// Schema version for forward-compatible migrations.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
}

fn default_schema_version() -> u32 {
    1
}

impl WorkspaceProfile {
    /// Creates a new active workspace profile owned by `owner_user_id`.
    pub fn new(
        workspace_id: impl Into<String>,
        display_name: impl Into<String>,
        owner_user_id: impl Into<String>,
        now_ns: u64,
    ) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            display_name: display_name.into(),
            owner_user_id: owner_user_id.into(),
            description: None,
            status: WorkspaceStatus::Active,
            created_at_ns: now_ns,
            updated_at_ns: now_ns,
            schema_version: 1,
        }
    }

    /// Normalises a raw display name into a lowercase slug suitable for use as
    /// a workspace ID: lowercased, spaces and underscores replaced with `-`,
    /// non-alphanumeric/hyphen characters stripped.
    ///
    /// Returns `Err` when the resulting slug is empty.
    pub fn make_id(raw: &str) -> Result<String, String> {
        let slug: String = raw
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else if c == ' ' || c == '_' || c == '-' {
                    '-'
                } else {
                    '\0'
                }
            })
            .filter(|&c| c != '\0')
            .collect();

        // Collapse consecutive hyphens and trim leading/trailing hyphens.
        let slug = collapse_hyphens(&slug);

        if slug.is_empty() {
            Err(format!(
                "cannot derive workspace id from {raw:?}: result is empty"
            ))
        } else {
            Ok(slug)
        }
    }

    /// Sets the optional description, updating `updated_at_ns`.
    pub fn set_description(&mut self, desc: impl Into<String>, now_ns: u64) {
        self.description = Some(desc.into());
        self.updated_at_ns = now_ns;
    }

    /// Transitions the workspace to `Suspended` status.
    ///
    /// Returns `false` when already suspended or deleted (no change).
    pub fn suspend(&mut self, now_ns: u64) -> bool {
        if self.status == WorkspaceStatus::Active {
            self.status = WorkspaceStatus::Suspended;
            self.updated_at_ns = now_ns;
            true
        } else {
            false
        }
    }

    /// Reactivates a suspended workspace.
    ///
    /// Returns `false` when not suspended (no change).
    pub fn reactivate(&mut self, now_ns: u64) -> bool {
        if self.status == WorkspaceStatus::Suspended {
            self.status = WorkspaceStatus::Active;
            self.updated_at_ns = now_ns;
            true
        } else {
            false
        }
    }

    /// Soft-deletes the workspace; this is irreversible.
    ///
    /// Returns `false` when already deleted (no change).
    pub fn delete(&mut self, now_ns: u64) -> bool {
        if self.status != WorkspaceStatus::Deleted {
            self.status = WorkspaceStatus::Deleted;
            self.updated_at_ns = now_ns;
            true
        } else {
            false
        }
    }
}

fn collapse_hyphens(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut last_hyphen = false;
    for c in s.chars() {
        if c == '-' {
            if !last_hyphen && !result.is_empty() {
                result.push('-');
            }
            last_hyphen = true;
        } else {
            result.push(c);
            last_hyphen = false;
        }
    }
    // Trim trailing hyphen.
    if result.ends_with('-') {
        result.pop();
    }
    result
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn workspace_status_ordering_roundtrip() {
        for status in [
            WorkspaceStatus::Active,
            WorkspaceStatus::Suspended,
            WorkspaceStatus::Deleted,
        ] {
            let parsed = WorkspaceStatus::from_str(status.as_str()).expect("parse");
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn workspace_status_rejects_unknown() {
        assert!(WorkspaceStatus::from_str("archived").is_err());
    }

    #[test]
    fn workspace_profile_new_is_active() {
        let p = WorkspaceProfile::new("acme", "Acme Corp", "telegram:1", 1_000_000);
        assert_eq!(p.status, WorkspaceStatus::Active);
        assert_eq!(p.workspace_id, "acme");
        assert_eq!(p.owner_user_id, "telegram:1");
        assert!(p.description.is_none());
        assert_eq!(p.created_at_ns, 1_000_000);
    }

    #[test]
    fn make_id_lowercases_and_replaces_spaces() {
        assert_eq!(WorkspaceProfile::make_id("Acme Corp").unwrap(), "acme-corp");
    }

    #[test]
    fn make_id_strips_special_characters() {
        assert_eq!(WorkspaceProfile::make_id("Team #1!").unwrap(), "team-1");
    }

    #[test]
    fn make_id_collapses_consecutive_hyphens() {
        assert_eq!(WorkspaceProfile::make_id("foo  bar").unwrap(), "foo-bar");
    }

    #[test]
    fn make_id_returns_error_for_empty_result() {
        assert!(WorkspaceProfile::make_id("!!!").is_err());
    }

    #[test]
    fn suspend_transitions_active_to_suspended() {
        let mut p = WorkspaceProfile::new("ws", "WS", "u:1", 100);
        assert!(p.suspend(200));
        assert_eq!(p.status, WorkspaceStatus::Suspended);
        assert_eq!(p.updated_at_ns, 200);
    }

    #[test]
    fn suspend_is_no_op_when_already_suspended() {
        let mut p = WorkspaceProfile::new("ws", "WS", "u:1", 100);
        p.suspend(200);
        assert!(!p.suspend(300));
        assert_eq!(p.updated_at_ns, 200); // not updated again
    }

    #[test]
    fn reactivate_transitions_suspended_to_active() {
        let mut p = WorkspaceProfile::new("ws", "WS", "u:1", 100);
        p.suspend(200);
        assert!(p.reactivate(300));
        assert_eq!(p.status, WorkspaceStatus::Active);
    }

    #[test]
    fn delete_transitions_to_deleted() {
        let mut p = WorkspaceProfile::new("ws", "WS", "u:1", 100);
        assert!(p.delete(200));
        assert_eq!(p.status, WorkspaceStatus::Deleted);
    }

    #[test]
    fn delete_is_no_op_when_already_deleted() {
        let mut p = WorkspaceProfile::new("ws", "WS", "u:1", 100);
        p.delete(200);
        assert!(!p.delete(300));
    }

    #[test]
    fn profile_round_trips_through_json() {
        let mut p = WorkspaceProfile::new("acme", "Acme Corp", "telegram:42", 999);
        p.set_description("A test workspace", 1000);
        let json = serde_json::to_string(&p).unwrap();
        let restored: WorkspaceProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, p);
    }
}
