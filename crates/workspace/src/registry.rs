#![forbid(unsafe_code)]

//! Workspace registry with atomic JSON persistence — E31 S31.4.
//!
//! [`WorkspaceRegistry`] is the single authoritative store for all workspace
//! records.  Each record bundles the workspace profile, its quota settings, and
//! its current membership list.
//!
//! Writes are atomic: the JSON is written to a `.tmp` sibling then renamed,
//! so a crash never leaves a corrupt registry file.
//!
//! Default path: `~/.anima/<agent_id>/workspaces.json`

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    membership::{WorkspaceMembership, WorkspaceRole},
    quota::{QuotaUsage, WorkspaceQuota},
    workspace::WorkspaceProfile,
};

// ── WorkspaceRecord ───────────────────────────────────────────────────────────

/// Bundled workspace state: profile, quota, and membership list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceRecord {
    /// Workspace identity and lifecycle state.
    pub profile: WorkspaceProfile,
    /// Resource limits for this workspace.
    #[serde(default)]
    pub quota: WorkspaceQuota,
    /// Current members (including the owner).
    #[serde(default)]
    pub members: Vec<WorkspaceMembership>,
}

impl WorkspaceRecord {
    /// Constructs a new record with the given profile and default quota.
    ///
    /// The owner is automatically added as the first member with `Owner` role.
    pub fn new(profile: WorkspaceProfile, now_ns: u64) -> Self {
        let owner_id = profile.owner_user_id.clone();
        let workspace_id = profile.workspace_id.clone();
        let owner =
            WorkspaceMembership::new(&workspace_id, &owner_id, WorkspaceRole::Owner, now_ns);
        Self {
            profile,
            quota: WorkspaceQuota::default(),
            members: vec![owner],
        }
    }

    /// Returns the role of `user_id` in this workspace, or `None` if not a member.
    pub fn member_role(&self, user_id: &str) -> Option<WorkspaceRole> {
        self.members
            .iter()
            .find(|m| m.user_id == user_id)
            .map(|m| m.role)
    }

    /// Returns the number of members.
    pub fn member_count(&self) -> usize {
        self.members.len()
    }

    /// Builds a [`QuotaUsage`] snapshot from the current membership count.
    ///
    /// Storage and token usage are not tracked in-registry (they come from the
    /// memory tier); this snapshot only populates the `current_members` field.
    pub fn membership_usage(&self) -> QuotaUsage {
        QuotaUsage {
            current_members: self.members.len(),
            ..Default::default()
        }
    }
}

// ── Errors ────────────────────────────────────────────────────────────────────

/// Errors returned by [`WorkspaceRegistry`] operations.
#[derive(Debug, PartialEq)]
pub enum WorkspaceError {
    /// A workspace with this ID already exists.
    AlreadyExists { workspace_id: String },
    /// No workspace with this ID was found.
    NotFound { workspace_id: String },
    /// The user is already a member of this workspace.
    MemberAlreadyExists {
        workspace_id: String,
        user_id: String,
    },
    /// No membership record for this user in this workspace.
    MemberNotFound {
        workspace_id: String,
        user_id: String,
    },
    /// The caller's role is insufficient to perform the operation.
    InsufficientRole {
        required: WorkspaceRole,
        actual: WorkspaceRole,
    },
    /// The workspace quota would be exceeded by the requested operation.
    QuotaExceeded {
        workspace_id: String,
        violation: crate::quota::QuotaViolation,
    },
    /// The workspace owner cannot be removed.
    CannotRemoveOwner {
        workspace_id: String,
        user_id: String,
    },
    /// Only one owner is allowed per workspace; use `Admin` for elevated access.
    DuplicateOwner { workspace_id: String },
    /// Serialisation or I/O failure.
    Io(String),
}

impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkspaceError::AlreadyExists { workspace_id } => {
                write!(f, "workspace already exists: {workspace_id}")
            }
            WorkspaceError::NotFound { workspace_id } => {
                write!(f, "workspace not found: {workspace_id}")
            }
            WorkspaceError::MemberAlreadyExists {
                workspace_id,
                user_id,
            } => write!(
                f,
                "user {user_id:?} is already a member of workspace {workspace_id:?}"
            ),
            WorkspaceError::MemberNotFound {
                workspace_id,
                user_id,
            } => write!(
                f,
                "user {user_id:?} is not a member of workspace {workspace_id:?}"
            ),
            WorkspaceError::InsufficientRole { required, actual } => {
                write!(f, "insufficient role: required {required}, actual {actual}")
            }
            WorkspaceError::QuotaExceeded {
                workspace_id,
                violation,
            } => write!(
                f,
                "quota exceeded in workspace {workspace_id:?}: {violation}"
            ),
            WorkspaceError::CannotRemoveOwner {
                workspace_id,
                user_id,
            } => write!(
                f,
                "cannot remove owner {user_id:?} from workspace {workspace_id:?}"
            ),
            WorkspaceError::DuplicateOwner { workspace_id } => {
                write!(f, "workspace {workspace_id:?} already has an owner")
            }
            WorkspaceError::Io(e) => write!(f, "workspace registry I/O error: {e}"),
        }
    }
}

// ── RegistryFile (on-disk schema) ─────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct RegistryFile {
    schema_version: u32,
    workspaces: HashMap<String, WorkspaceRecord>,
}

// ── WorkspaceRegistry ─────────────────────────────────────────────────────────

/// In-memory workspace registry with optional JSON persistence.
///
/// Create with [`WorkspaceRegistry::open`] for persistent mode or
/// [`WorkspaceRegistry::in_memory`] for transient mode.  Call
/// [`WorkspaceRegistry::flush`] to persist after mutations.
#[derive(Debug)]
pub struct WorkspaceRegistry {
    workspaces: HashMap<String, WorkspaceRecord>,
    path: Option<PathBuf>,
}

impl WorkspaceRegistry {
    /// Returns the default path for a given `agent_id`.
    ///
    /// Path: `~/.anima/<agent_id>/workspaces.json`
    pub fn default_path(agent_id: &str) -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_owned());
        PathBuf::from(home)
            .join(".anima")
            .join(agent_id)
            .join("workspaces.json")
    }

    /// Opens (or creates) a registry at `path`.
    ///
    /// If `path` does not exist an empty registry is returned; the file is
    /// created on the first [`flush`](Self::flush) call.
    pub fn open(path: &Path) -> Result<Self, WorkspaceError> {
        if path.exists() {
            let data =
                std::fs::read_to_string(path).map_err(|e| WorkspaceError::Io(e.to_string()))?;
            let file: RegistryFile =
                serde_json::from_str(&data).map_err(|e| WorkspaceError::Io(e.to_string()))?;
            Ok(Self {
                workspaces: file.workspaces,
                path: Some(path.to_owned()),
            })
        } else {
            Ok(Self {
                workspaces: HashMap::new(),
                path: Some(path.to_owned()),
            })
        }
    }

    /// Creates a transient in-memory registry with no persistence.
    pub fn in_memory() -> Self {
        Self {
            workspaces: HashMap::new(),
            path: None,
        }
    }

    // ── workspace CRUD ────────────────────────────────────────────────────────

    /// Creates a new workspace.
    ///
    /// The owner is automatically added as a member with [`WorkspaceRole::Owner`].
    /// Returns [`WorkspaceError::AlreadyExists`] when the ID is already taken.
    pub fn create(&mut self, profile: WorkspaceProfile, now_ns: u64) -> Result<(), WorkspaceError> {
        let id = profile.workspace_id.clone();
        if self.workspaces.contains_key(&id) {
            return Err(WorkspaceError::AlreadyExists { workspace_id: id });
        }
        let record = WorkspaceRecord::new(profile, now_ns);
        self.workspaces.insert(id, record);
        Ok(())
    }

    /// Returns a shared reference to a workspace record.
    pub fn get(&self, workspace_id: &str) -> Option<&WorkspaceRecord> {
        self.workspaces.get(workspace_id)
    }

    /// Returns a mutable reference to a workspace record.
    pub fn get_mut(&mut self, workspace_id: &str) -> Option<&mut WorkspaceRecord> {
        self.workspaces.get_mut(workspace_id)
    }

    /// Updates the quota for a workspace.
    ///
    /// `actor` is the user requesting the change; they must hold a managing role
    /// ([`WorkspaceRole::Admin`] or higher) in the workspace.
    ///
    /// Returns [`WorkspaceError::NotFound`] when the workspace does not exist and
    /// [`WorkspaceError::InsufficientRole`] when `actor` lacks authority.
    pub fn set_quota(
        &mut self,
        actor: &str,
        workspace_id: &str,
        quota: WorkspaceQuota,
    ) -> Result<(), WorkspaceError> {
        let rec =
            self.workspaces
                .get_mut(workspace_id)
                .ok_or_else(|| WorkspaceError::NotFound {
                    workspace_id: workspace_id.to_owned(),
                })?;
        authorize_manage(rec, actor)?;
        rec.quota = quota;
        Ok(())
    }

    // ── membership ────────────────────────────────────────────────────────────

    /// Adds a user to a workspace with the given role.
    ///
    /// `actor` is the user requesting the change; they must hold a managing role
    /// ([`WorkspaceRole::Admin`] or higher) in the workspace.
    ///
    /// Returns [`WorkspaceError::InsufficientRole`] when `actor` lacks authority,
    /// [`WorkspaceError::DuplicateOwner`] when `role` is `Owner` (a workspace has
    /// exactly one owner: the creator), [`WorkspaceError::MemberAlreadyExists`]
    /// when the user is already a member, and [`WorkspaceError::QuotaExceeded`]
    /// when the workspace member quota would be exceeded.
    pub fn add_member(
        &mut self,
        actor: &str,
        workspace_id: &str,
        user_id: impl Into<String>,
        role: WorkspaceRole,
        now_ns: u64,
    ) -> Result<(), WorkspaceError> {
        if role == WorkspaceRole::Owner {
            return Err(WorkspaceError::DuplicateOwner {
                workspace_id: workspace_id.to_owned(),
            });
        }

        let user_id: String = user_id.into();
        let rec =
            self.workspaces
                .get_mut(workspace_id)
                .ok_or_else(|| WorkspaceError::NotFound {
                    workspace_id: workspace_id.to_owned(),
                })?;

        authorize_manage(rec, actor)?;

        // An actor may only grant a role strictly below their own. This keeps
        // Admins to managing membership "up to Member level" (the role
        // contract in `membership.rs`): an Admin cannot mint another Admin, so
        // delegating Admin to a user does not let them escalate the workspace.
        let actor_role = rec.member_role(actor).unwrap_or(WorkspaceRole::Guest);
        if role >= actor_role {
            return Err(WorkspaceError::InsufficientRole {
                required: WorkspaceRole::Owner,
                actual: actor_role,
            });
        }

        // Duplicate check before quota so a re-add of an existing member
        // never returns QuotaExceeded.
        if rec.members.iter().any(|m| m.user_id == user_id) {
            return Err(WorkspaceError::MemberAlreadyExists {
                workspace_id: workspace_id.to_owned(),
                user_id,
            });
        }

        // Quota check: would adding one member exceed the limit?
        let usage = rec.membership_usage();
        if !usage.can_add_members(1, &rec.quota) {
            return Err(WorkspaceError::QuotaExceeded {
                workspace_id: workspace_id.to_owned(),
                violation: crate::quota::QuotaViolation::MemberLimit,
            });
        }

        let membership = WorkspaceMembership::new(workspace_id, &user_id, role, now_ns);
        rec.members.push(membership);
        Ok(())
    }

    /// Removes a user from a workspace.
    ///
    /// `actor` is the user requesting the change; they must hold a managing role
    /// ([`WorkspaceRole::Admin`] or higher) in the workspace.  A member may always
    /// remove themselves (leave the workspace), even without a managing role.
    ///
    /// The owner (the user with [`WorkspaceRole::Owner`]) cannot be removed.
    /// Returns [`WorkspaceError::InsufficientRole`] when `actor` lacks authority
    /// and [`WorkspaceError::MemberNotFound`] when the user is not a member.
    pub fn remove_member(
        &mut self,
        actor: &str,
        workspace_id: &str,
        user_id: &str,
    ) -> Result<WorkspaceRole, WorkspaceError> {
        let rec =
            self.workspaces
                .get_mut(workspace_id)
                .ok_or_else(|| WorkspaceError::NotFound {
                    workspace_id: workspace_id.to_owned(),
                })?;

        // A user may always remove themselves; otherwise a managing role is required.
        if actor != user_id {
            authorize_manage(rec, actor)?;
        }

        let pos = rec
            .members
            .iter()
            .position(|m| m.user_id == user_id)
            .ok_or_else(|| WorkspaceError::MemberNotFound {
                workspace_id: workspace_id.to_owned(),
                user_id: user_id.to_owned(),
            })?;

        let removed_role = rec.members[pos].role;
        if removed_role == WorkspaceRole::Owner {
            return Err(WorkspaceError::CannotRemoveOwner {
                workspace_id: workspace_id.to_owned(),
                user_id: user_id.to_owned(),
            });
        }
        // Symmetric with add_member: removing a peer (or higher) managing role
        // requires outranking it, so one Admin cannot evict another. Does not
        // apply to self-removal, which returned above.
        if actor != user_id {
            let actor_role = rec.member_role(actor).unwrap_or(WorkspaceRole::Guest);
            if removed_role >= actor_role {
                return Err(WorkspaceError::InsufficientRole {
                    required: WorkspaceRole::Owner,
                    actual: actor_role,
                });
            }
        }
        rec.members.remove(pos);
        Ok(removed_role)
    }

    /// Returns the role of `user_id` in `workspace_id`, or `None` if not a member.
    pub fn member_role(&self, workspace_id: &str, user_id: &str) -> Option<WorkspaceRole> {
        self.workspaces.get(workspace_id)?.member_role(user_id)
    }

    // ── iteration ────────────────────────────────────────────────────────────

    /// Returns an iterator over all `(workspace_id, WorkspaceRecord)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &WorkspaceRecord)> {
        self.workspaces.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Returns the number of workspaces in the registry.
    pub fn len(&self) -> usize {
        self.workspaces.len()
    }

    /// Returns `true` when the registry contains no workspaces.
    pub fn is_empty(&self) -> bool {
        self.workspaces.is_empty()
    }

    // ── persistence ───────────────────────────────────────────────────────────

    /// Atomically writes the registry to its backing file.
    ///
    /// Uses the write-to-`.tmp`-then-rename pattern for crash safety.
    /// Returns `Ok(())` immediately when running in in-memory mode.
    pub fn flush(&self) -> Result<(), WorkspaceError> {
        let path = match &self.path {
            Some(p) => p,
            None => return Ok(()),
        };

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| WorkspaceError::Io(e.to_string()))?;
            }
        }

        let file = RegistryFile {
            schema_version: 1,
            workspaces: self.workspaces.clone(),
        };
        let json =
            serde_json::to_string_pretty(&file).map_err(|e| WorkspaceError::Io(e.to_string()))?;

        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, json).map_err(|e| WorkspaceError::Io(e.to_string()))?;
        std::fs::rename(&tmp, path).map_err(|e| WorkspaceError::Io(e.to_string()))
    }
}

// ── authorization helper ──────────────────────────────────────────────────────

/// Verifies that `actor` holds a managing role ([`WorkspaceRole::Admin`] or
/// higher) in `rec`, returning [`WorkspaceError::InsufficientRole`] otherwise.
///
/// A non-member actor is treated as the lowest role ([`WorkspaceRole::Guest`])
/// so the same error path covers both "not a member" and "role too low".
fn authorize_manage(rec: &WorkspaceRecord, actor: &str) -> Result<(), WorkspaceError> {
    let role = rec.member_role(actor).unwrap_or(WorkspaceRole::Guest);
    if role.can_manage() {
        Ok(())
    } else {
        Err(WorkspaceError::InsufficientRole {
            required: WorkspaceRole::Admin,
            actual: role,
        })
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{membership::WorkspaceRole, quota::WorkspaceQuota, workspace::WorkspaceProfile};

    fn make_profile(id: &str, owner: &str) -> WorkspaceProfile {
        WorkspaceProfile::new(id, id, owner, 0)
    }

    // ── workspace CRUD ────────────────────────────────────────────────────────

    #[test]
    fn empty_registry_has_zero_workspaces() {
        let reg = WorkspaceRegistry::in_memory();
        assert_eq!(reg.len(), 0);
        assert!(reg.is_empty());
    }

    #[test]
    fn create_adds_workspace_with_owner_as_member() {
        let mut reg = WorkspaceRegistry::in_memory();
        reg.create(make_profile("acme", "telegram:1"), 0).unwrap();
        assert_eq!(reg.len(), 1);
        let rec = reg.get("acme").unwrap();
        assert_eq!(rec.member_count(), 1);
        assert_eq!(rec.member_role("telegram:1"), Some(WorkspaceRole::Owner));
    }

    #[test]
    fn create_rejects_duplicate_workspace_id() {
        let mut reg = WorkspaceRegistry::in_memory();
        reg.create(make_profile("acme", "u:1"), 0).unwrap();
        let err = reg.create(make_profile("acme", "u:2"), 0).unwrap_err();
        assert_eq!(
            err,
            WorkspaceError::AlreadyExists {
                workspace_id: "acme".to_owned()
            }
        );
    }

    #[test]
    fn get_returns_none_for_missing_workspace() {
        let reg = WorkspaceRegistry::in_memory();
        assert!(reg.get("missing").is_none());
    }

    #[test]
    fn set_quota_updates_the_quota() {
        let mut reg = WorkspaceRegistry::in_memory();
        reg.create(make_profile("ws", "u:1"), 0).unwrap();
        let new_quota = WorkspaceQuota::new(5, 100, 512, 10);
        reg.set_quota("u:1", "ws", new_quota.clone()).unwrap();
        assert_eq!(reg.get("ws").unwrap().quota, new_quota);
    }

    #[test]
    fn set_quota_returns_error_for_missing_workspace() {
        let mut reg = WorkspaceRegistry::in_memory();
        let err = reg
            .set_quota("u:1", "ghost", WorkspaceQuota::default())
            .unwrap_err();
        assert_eq!(
            err,
            WorkspaceError::NotFound {
                workspace_id: "ghost".to_owned()
            }
        );
    }

    // ── membership ────────────────────────────────────────────────────────────

    #[test]
    fn add_member_increases_count() {
        let mut reg = WorkspaceRegistry::in_memory();
        reg.create(make_profile("acme", "u:1"), 0).unwrap();
        reg.add_member("u:1", "acme", "u:2", WorkspaceRole::Member, 100)
            .unwrap();
        assert_eq!(reg.get("acme").unwrap().member_count(), 2);
        assert_eq!(reg.member_role("acme", "u:2"), Some(WorkspaceRole::Member));
    }

    #[test]
    fn add_member_rejects_duplicate() {
        let mut reg = WorkspaceRegistry::in_memory();
        reg.create(make_profile("acme", "u:1"), 0).unwrap();
        reg.add_member("u:1", "acme", "u:2", WorkspaceRole::Member, 0)
            .unwrap();
        let err = reg
            .add_member("u:1", "acme", "u:2", WorkspaceRole::Admin, 0)
            .unwrap_err();
        assert_eq!(
            err,
            WorkspaceError::MemberAlreadyExists {
                workspace_id: "acme".to_owned(),
                user_id: "u:2".to_owned()
            }
        );
    }

    #[test]
    fn add_member_respects_quota() {
        let mut reg = WorkspaceRegistry::in_memory();
        reg.create(make_profile("tiny", "u:1"), 0).unwrap();
        // Set quota to only allow 1 member (already at capacity with the owner).
        reg.set_quota(
            "u:1",
            "tiny",
            WorkspaceQuota::new(1, u64::MAX, u64::MAX, usize::MAX),
        )
        .unwrap();
        let err = reg
            .add_member("u:1", "tiny", "u:2", WorkspaceRole::Member, 0)
            .unwrap_err();
        assert!(matches!(err, WorkspaceError::QuotaExceeded { .. }));
    }

    #[test]
    fn remove_member_reduces_count() {
        let mut reg = WorkspaceRegistry::in_memory();
        reg.create(make_profile("acme", "u:1"), 0).unwrap();
        reg.add_member("u:1", "acme", "u:2", WorkspaceRole::Member, 0)
            .unwrap();
        reg.remove_member("u:1", "acme", "u:2").unwrap();
        assert_eq!(reg.get("acme").unwrap().member_count(), 1);
        assert!(reg.member_role("acme", "u:2").is_none());
    }

    #[test]
    fn remove_member_returns_error_for_non_member() {
        let mut reg = WorkspaceRegistry::in_memory();
        reg.create(make_profile("acme", "u:1"), 0).unwrap();
        let err = reg.remove_member("u:1", "acme", "u:99").unwrap_err();
        assert_eq!(
            err,
            WorkspaceError::MemberNotFound {
                workspace_id: "acme".to_owned(),
                user_id: "u:99".to_owned()
            }
        );
    }

    #[test]
    fn member_role_returns_none_for_non_member() {
        let mut reg = WorkspaceRegistry::in_memory();
        reg.create(make_profile("acme", "u:1"), 0).unwrap();
        assert!(reg.member_role("acme", "u:99").is_none());
    }

    #[test]
    fn iter_returns_all_workspaces() {
        let mut reg = WorkspaceRegistry::in_memory();
        reg.create(make_profile("ws-1", "u:1"), 0).unwrap();
        reg.create(make_profile("ws-2", "u:2"), 0).unwrap();
        let ids: Vec<&str> = reg.iter().map(|(id, _)| id).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"ws-1"));
        assert!(ids.contains(&"ws-2"));
    }

    // ── persistence ───────────────────────────────────────────────────────────

    #[test]
    fn flush_and_reload_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspaces.json");

        let mut reg = WorkspaceRegistry::open(&path).unwrap();
        reg.create(make_profile("acme", "u:1"), 0).unwrap();
        reg.add_member("u:1", "acme", "u:2", WorkspaceRole::Admin, 100)
            .unwrap();
        reg.flush().unwrap();

        let restored = WorkspaceRegistry::open(&path).unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(
            restored.member_role("acme", "u:2"),
            Some(WorkspaceRole::Admin)
        );
    }

    #[test]
    fn open_creates_empty_registry_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspaces.json");
        let reg = WorkspaceRegistry::open(&path).unwrap();
        assert!(reg.is_empty());
    }

    #[test]
    fn in_memory_flush_is_no_op() {
        let reg = WorkspaceRegistry::in_memory();
        assert!(reg.flush().is_ok());
    }

    #[test]
    fn remove_member_rejects_owner_removal() {
        let mut reg = WorkspaceRegistry::in_memory();
        reg.create(make_profile("acme", "u:1"), 0).unwrap();
        let err = reg.remove_member("u:1", "acme", "u:1").unwrap_err();
        assert_eq!(
            err,
            WorkspaceError::CannotRemoveOwner {
                workspace_id: "acme".to_owned(),
                user_id: "u:1".to_owned()
            }
        );
    }

    #[test]
    fn add_member_rejects_owner_role() {
        let mut reg = WorkspaceRegistry::in_memory();
        reg.create(make_profile("acme", "u:1"), 0).unwrap();
        let err = reg
            .add_member("u:1", "acme", "u:2", WorkspaceRole::Owner, 0)
            .unwrap_err();
        assert_eq!(
            err,
            WorkspaceError::DuplicateOwner {
                workspace_id: "acme".to_owned()
            }
        );
    }

    #[test]
    fn add_member_at_capacity_returns_already_exists_for_existing_member() {
        let mut reg = WorkspaceRegistry::in_memory();
        reg.create(make_profile("tiny", "u:1"), 0).unwrap();
        reg.set_quota(
            "u:1",
            "tiny",
            WorkspaceQuota::new(1, u64::MAX, u64::MAX, usize::MAX),
        )
        .unwrap();
        // u:1 is already a member; should get MemberAlreadyExists, not QuotaExceeded.
        let err = reg
            .add_member("u:1", "tiny", "u:1", WorkspaceRole::Member, 0)
            .unwrap_err();
        assert!(matches!(err, WorkspaceError::MemberAlreadyExists { .. }));
    }

    // ── authorization (caller role enforcement) ──────────────────────────────

    #[test]
    fn non_admin_cannot_add_member() {
        let mut reg = WorkspaceRegistry::in_memory();
        reg.create(make_profile("acme", "owner"), 0).unwrap();
        // Add a plain member who will then attempt a privileged change.
        reg.add_member("owner", "acme", "member-1", WorkspaceRole::Member, 0)
            .unwrap();
        let err = reg
            .add_member("member-1", "acme", "member-2", WorkspaceRole::Member, 0)
            .unwrap_err();
        assert!(matches!(err, WorkspaceError::InsufficientRole { .. }));
        // The unauthorized add must not have taken effect.
        assert!(reg.member_role("acme", "member-2").is_none());
    }

    #[test]
    fn non_member_cannot_add_member() {
        let mut reg = WorkspaceRegistry::in_memory();
        reg.create(make_profile("acme", "owner"), 0).unwrap();
        let err = reg
            .add_member("stranger", "acme", "member-1", WorkspaceRole::Member, 0)
            .unwrap_err();
        assert!(matches!(err, WorkspaceError::InsufficientRole { .. }));
    }

    #[test]
    fn admin_can_add_member() {
        let mut reg = WorkspaceRegistry::in_memory();
        reg.create(make_profile("acme", "owner"), 0).unwrap();
        reg.add_member("owner", "acme", "admin-1", WorkspaceRole::Admin, 0)
            .unwrap();
        // The admin may now add members on their own authority.
        reg.add_member("admin-1", "acme", "member-1", WorkspaceRole::Member, 0)
            .unwrap();
        assert_eq!(
            reg.member_role("acme", "member-1"),
            Some(WorkspaceRole::Member)
        );
    }

    #[test]
    fn non_admin_cannot_remove_member() {
        let mut reg = WorkspaceRegistry::in_memory();
        reg.create(make_profile("acme", "owner"), 0).unwrap();
        reg.add_member("owner", "acme", "member-1", WorkspaceRole::Member, 0)
            .unwrap();
        reg.add_member("owner", "acme", "member-2", WorkspaceRole::Member, 0)
            .unwrap();
        let err = reg
            .remove_member("member-1", "acme", "member-2")
            .unwrap_err();
        assert!(matches!(err, WorkspaceError::InsufficientRole { .. }));
        // member-2 must still be present.
        assert_eq!(
            reg.member_role("acme", "member-2"),
            Some(WorkspaceRole::Member)
        );
    }

    #[test]
    fn admin_can_remove_member() {
        let mut reg = WorkspaceRegistry::in_memory();
        reg.create(make_profile("acme", "owner"), 0).unwrap();
        reg.add_member("owner", "acme", "member-1", WorkspaceRole::Member, 0)
            .unwrap();
        reg.remove_member("owner", "acme", "member-1").unwrap();
        assert!(reg.member_role("acme", "member-1").is_none());
    }

    #[test]
    fn member_may_remove_self_without_managing_role() {
        let mut reg = WorkspaceRegistry::in_memory();
        reg.create(make_profile("acme", "owner"), 0).unwrap();
        reg.add_member("owner", "acme", "member-1", WorkspaceRole::Member, 0)
            .unwrap();
        // Self-removal (leaving) is permitted even without a managing role.
        reg.remove_member("member-1", "acme", "member-1").unwrap();
        assert!(reg.member_role("acme", "member-1").is_none());
    }

    #[test]
    fn non_admin_cannot_set_quota() {
        let mut reg = WorkspaceRegistry::in_memory();
        reg.create(make_profile("acme", "owner"), 0).unwrap();
        reg.add_member("owner", "acme", "member-1", WorkspaceRole::Member, 0)
            .unwrap();
        let err = reg
            .set_quota("member-1", "acme", WorkspaceQuota::new(99, 99, 99, 99))
            .unwrap_err();
        assert!(matches!(err, WorkspaceError::InsufficientRole { .. }));
    }

    #[test]
    fn admin_can_set_quota() {
        let mut reg = WorkspaceRegistry::in_memory();
        reg.create(make_profile("acme", "owner"), 0).unwrap();
        let q = WorkspaceQuota::new(7, 7, 7, 7);
        reg.set_quota("owner", "acme", q.clone()).unwrap();
        assert_eq!(reg.get("acme").unwrap().quota, q);
    }

    #[test]
    fn admin_cannot_grant_admin_role() {
        // An Admin may manage membership only up to Member level: delegating
        // Admin must require the Owner, so a delegated admin cannot mint peers.
        let mut reg = WorkspaceRegistry::in_memory();
        reg.create(make_profile("acme", "owner"), 0).unwrap();
        reg.add_member("owner", "acme", "admin-1", WorkspaceRole::Admin, 0)
            .unwrap();
        let err = reg
            .add_member("admin-1", "acme", "admin-2", WorkspaceRole::Admin, 0)
            .unwrap_err();
        assert!(matches!(err, WorkspaceError::InsufficientRole { .. }));
        assert!(reg.member_role("acme", "admin-2").is_none());
        // The owner, who outranks Admin, still may.
        reg.add_member("owner", "acme", "admin-2", WorkspaceRole::Admin, 0)
            .unwrap();
    }

    #[test]
    fn admin_cannot_remove_peer_admin() {
        let mut reg = WorkspaceRegistry::in_memory();
        reg.create(make_profile("acme", "owner"), 0).unwrap();
        reg.add_member("owner", "acme", "admin-1", WorkspaceRole::Admin, 0)
            .unwrap();
        reg.add_member("owner", "acme", "admin-2", WorkspaceRole::Admin, 0)
            .unwrap();
        let err = reg.remove_member("admin-1", "acme", "admin-2").unwrap_err();
        assert!(matches!(err, WorkspaceError::InsufficientRole { .. }));
        // The owner outranks both and may remove either.
        reg.remove_member("owner", "acme", "admin-2").unwrap();
    }

    #[test]
    fn flush_with_relative_path_succeeds() {
        use std::path::Path;
        let path = Path::new("workspaces_test_temp.json");
        let mut reg = WorkspaceRegistry::open(path).unwrap();
        reg.create(make_profile("acme", "u:1"), 0).unwrap();
        reg.flush().unwrap();
        assert!(path.exists());
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file("workspaces_test_temp.tmp");
    }
}
