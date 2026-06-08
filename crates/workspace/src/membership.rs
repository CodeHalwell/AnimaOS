#![forbid(unsafe_code)]

//! Workspace membership and roles — E31 S31.2.
//!
//! Every user who belongs to a workspace has a [`WorkspaceMembership`] that
//! records their role.  Roles form a total order: `Guest < Member < Admin < Owner`.
//!
//! The `Owner` role may only be held by one user (the workspace creator).
//! [`WorkspaceRole::can_manage`] returns `true` for roles with administrative
//! authority (`Admin` and above).

use serde::{Deserialize, Serialize};

// ── WorkspaceRole ─────────────────────────────────────────────────────────────

/// The role a user holds within a workspace.
///
/// Roles are totally ordered; higher values imply strictly more authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceRole {
    /// Read-only access; cannot trigger agent tasks.
    #[default]
    Guest = 0,
    /// Full member; can submit tasks and access workspace memory.
    Member = 1,
    /// Can manage membership (add/remove members up to `Member` level).
    Admin = 2,
    /// Full control, including workspace deletion; assigned to the creator.
    Owner = 3,
}

impl WorkspaceRole {
    /// Returns a human-readable label for audit entries and display.
    pub fn as_str(self) -> &'static str {
        match self {
            WorkspaceRole::Guest => "guest",
            WorkspaceRole::Member => "member",
            WorkspaceRole::Admin => "admin",
            WorkspaceRole::Owner => "owner",
        }
    }

    /// Returns `true` when this role grants at least the given minimum level.
    pub fn at_least(self, minimum: WorkspaceRole) -> bool {
        self >= minimum
    }

    /// Returns `true` when this role has administrative authority (`Admin` or higher).
    pub fn can_manage(self) -> bool {
        self >= WorkspaceRole::Admin
    }
}

impl std::fmt::Display for WorkspaceRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for WorkspaceRole {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "guest" => Ok(WorkspaceRole::Guest),
            "member" => Ok(WorkspaceRole::Member),
            "admin" => Ok(WorkspaceRole::Admin),
            "owner" => Ok(WorkspaceRole::Owner),
            other => Err(format!("unknown workspace role: {other:?}")),
        }
    }
}

// ── WorkspaceMembership ───────────────────────────────────────────────────────

/// A user's membership record within a specific workspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceMembership {
    /// Workspace this membership belongs to.
    pub workspace_id: String,
    /// The user who is a member.
    pub user_id: String,
    /// The role granted to this user in this workspace.
    pub role: WorkspaceRole,
    /// Unix nanoseconds when the membership was created.
    pub joined_at_ns: u64,
}

impl WorkspaceMembership {
    /// Creates a new membership record.
    pub fn new(
        workspace_id: impl Into<String>,
        user_id: impl Into<String>,
        role: WorkspaceRole,
        now_ns: u64,
    ) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            user_id: user_id.into(),
            role,
            joined_at_ns: now_ns,
        }
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn role_ordering_is_correct() {
        assert!(WorkspaceRole::Guest < WorkspaceRole::Member);
        assert!(WorkspaceRole::Member < WorkspaceRole::Admin);
        assert!(WorkspaceRole::Admin < WorkspaceRole::Owner);
    }

    #[test]
    fn role_at_least_predicate() {
        assert!(WorkspaceRole::Admin.at_least(WorkspaceRole::Member));
        assert!(WorkspaceRole::Admin.at_least(WorkspaceRole::Admin));
        assert!(!WorkspaceRole::Admin.at_least(WorkspaceRole::Owner));
        assert!(WorkspaceRole::Guest.at_least(WorkspaceRole::Guest));
    }

    #[test]
    fn role_can_manage_predicate() {
        assert!(WorkspaceRole::Admin.can_manage());
        assert!(WorkspaceRole::Owner.can_manage());
        assert!(!WorkspaceRole::Member.can_manage());
        assert!(!WorkspaceRole::Guest.can_manage());
    }

    #[test]
    fn role_from_str_round_trips() {
        for role in [
            WorkspaceRole::Guest,
            WorkspaceRole::Member,
            WorkspaceRole::Admin,
            WorkspaceRole::Owner,
        ] {
            let parsed = WorkspaceRole::from_str(role.as_str()).expect("parse");
            assert_eq!(parsed, role);
        }
    }

    #[test]
    fn role_from_str_rejects_unknown() {
        assert!(WorkspaceRole::from_str("superuser").is_err());
    }

    #[test]
    fn membership_new_captures_fields() {
        let m = WorkspaceMembership::new("acme", "telegram:42", WorkspaceRole::Member, 1_000);
        assert_eq!(m.workspace_id, "acme");
        assert_eq!(m.user_id, "telegram:42");
        assert_eq!(m.role, WorkspaceRole::Member);
        assert_eq!(m.joined_at_ns, 1_000);
    }

    #[test]
    fn membership_round_trips_through_json() {
        let m = WorkspaceMembership::new("ws-1", "slack:U99", WorkspaceRole::Admin, 42);
        let json = serde_json::to_string(&m).unwrap();
        let restored: WorkspaceMembership = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, m);
    }
}
