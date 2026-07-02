#![forbid(unsafe_code)]

//! Per-user registry with atomic JSON persistence — E17 S17.2.
//!
//! [`UserRegistry`] stores [`UserRecord`]s (profile + consent) keyed by
//! `user_id` and persists them to a JSON file under the agent's state
//! directory.  Writes are atomic: the file is written to a `.tmp` sibling
//! and then renamed, so a crash never corrupts the registry.
//!
//! Default path: `~/.anima/<agent_id>/users.json`

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    consent::ConsentRecord,
    profile::{TrustTier, UserProfile},
};

// ── UserRecord ────────────────────────────────────────────────────────────────

/// Combined profile + consent state for one user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserRecord {
    /// Identity profile.
    pub profile: UserProfile,
    /// Data-retention consent.
    #[serde(default)]
    pub consent: ConsentRecord,
}

impl UserRecord {
    /// Creates a new record with the given profile and no consented categories.
    pub fn new(profile: UserProfile) -> Self {
        Self {
            profile,
            consent: ConsentRecord::new(),
        }
    }
}

// ── Errors ────────────────────────────────────────────────────────────────────

/// Errors returned by [`UserRegistry`] operations.
#[derive(Debug, PartialEq)]
pub enum RegistryError {
    /// A user with this `user_id` already exists.
    AlreadyExists { user_id: String },
    /// No user with this `user_id` was found.
    NotFound { user_id: String },
    /// Serialisation or I/O failed.
    Io(String),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::AlreadyExists { user_id } => {
                write!(f, "user already exists: {user_id}")
            }
            RegistryError::NotFound { user_id } => {
                write!(f, "user not found: {user_id}")
            }
            RegistryError::Io(e) => write!(f, "registry I/O error: {e}"),
        }
    }
}

// ── RegistryFile (on-disk schema) ─────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct RegistryFile {
    schema_version: u32,
    users: HashMap<String, UserRecord>,
}

// ── UserRegistry ──────────────────────────────────────────────────────────────

/// An in-memory registry of [`UserRecord`]s with optional JSON persistence.
///
/// When created with [`UserRegistry::open`] or [`UserRegistry::in_memory`],
/// all mutations are reflected immediately in the in-memory map.  Call
/// [`UserRegistry::flush`] (or use the `auto_flush = true` mode via
/// [`UserRegistry::open`]) to persist after each mutation.
#[derive(Debug)]
pub struct UserRegistry {
    users: HashMap<String, UserRecord>,
    path: Option<PathBuf>,
}

impl UserRegistry {
    /// Returns the default path for a given `agent_id`:
    /// `<state_dir>/<agent_id>/users.json`.
    ///
    /// Routes through [`jsonstore::agent_state_path`] so the users store shares
    /// the one state root (`ANIMA_STATE_DIR` → `$HOME`/`$USERPROFILE`/.anima →
    /// `/var/lib/anima`) with every other per-agent store. Resolving it here
    /// independently would leave a relocated deployment reading users from the
    /// old root while sessions moved to the new one (OPS-13).
    pub fn default_path(agent_id: &str) -> PathBuf {
        jsonstore::agent_state_path(agent_id, "users.json")
    }

    /// Opens (or creates) a registry at `path`.
    ///
    /// If `path` does not exist an empty registry is returned; the file is
    /// only created on the first [`flush`](Self::flush) call.
    pub fn open(path: &Path) -> Result<Self, RegistryError> {
        if path.exists() {
            let data =
                std::fs::read_to_string(path).map_err(|e| RegistryError::Io(e.to_string()))?;
            let file: RegistryFile =
                serde_json::from_str(&data).map_err(|e| RegistryError::Io(e.to_string()))?;
            Ok(Self {
                users: file.users,
                path: Some(path.to_owned()),
            })
        } else {
            Ok(Self {
                users: HashMap::new(),
                path: Some(path.to_owned()),
            })
        }
    }

    /// Creates a transient in-memory registry with no persistence.
    pub fn in_memory() -> Self {
        Self {
            users: HashMap::new(),
            path: None,
        }
    }

    /// Returns `true` when the registry has a backing file.
    pub fn is_persistent(&self) -> bool {
        self.path.is_some()
    }

    // ── mutations ─────────────────────────────────────────────────────────────

    /// Registers a new user.
    ///
    /// Returns [`RegistryError::AlreadyExists`] when a user with the same
    /// `user_id` is already present.
    pub fn register(&mut self, profile: UserProfile) -> Result<(), RegistryError> {
        let user_id = profile.user_id.clone();
        if self.users.contains_key(&user_id) {
            return Err(RegistryError::AlreadyExists { user_id });
        }
        self.users.insert(user_id, UserRecord::new(profile));
        Ok(())
    }

    /// Registers or updates a user (upsert semantics).
    ///
    /// If the user already exists the profile fields are updated; consent is
    /// preserved.  Returns `true` when a new record was created.
    pub fn upsert(&mut self, profile: UserProfile) -> bool {
        let user_id = profile.user_id.clone();
        if let Some(rec) = self.users.get_mut(&user_id) {
            rec.profile = profile;
            false
        } else {
            self.users.insert(user_id, UserRecord::new(profile));
            true
        }
    }

    /// Returns a shared reference to a user record.
    pub fn get(&self, user_id: &str) -> Option<&UserRecord> {
        self.users.get(user_id)
    }

    /// Returns a mutable reference to a user record.
    pub fn get_mut(&mut self, user_id: &str) -> Option<&mut UserRecord> {
        self.users.get_mut(user_id)
    }

    /// Removes a user and returns the removed [`UserRecord`].
    ///
    /// Returns [`RegistryError::NotFound`] when no such user exists.
    pub fn remove(&mut self, user_id: &str) -> Result<UserRecord, RegistryError> {
        self.users
            .remove(user_id)
            .ok_or_else(|| RegistryError::NotFound {
                user_id: user_id.to_owned(),
            })
    }

    /// Updates the trust tier for an existing user.
    ///
    /// Returns `(old_tier, new_tier)` on success.
    pub fn set_trust(
        &mut self,
        user_id: &str,
        tier: TrustTier,
        now_ns: u64,
    ) -> Result<(TrustTier, TrustTier), RegistryError> {
        let rec = self
            .users
            .get_mut(user_id)
            .ok_or_else(|| RegistryError::NotFound {
                user_id: user_id.to_owned(),
            })?;
        let old = rec.profile.trust_tier;
        rec.profile.trust_tier = tier;
        rec.profile.touch(now_ns);
        Ok((old, tier))
    }

    /// Updates a fact on an existing user's profile.
    pub fn set_fact(
        &mut self,
        user_id: &str,
        key: impl Into<String>,
        value: impl Into<String>,
        now_ns: u64,
    ) -> Result<Option<String>, RegistryError> {
        let rec = self
            .users
            .get_mut(user_id)
            .ok_or_else(|| RegistryError::NotFound {
                user_id: user_id.to_owned(),
            })?;
        rec.profile.touch(now_ns);
        Ok(rec.profile.set_fact(key, value))
    }

    /// Returns an iterator over all `(user_id, UserRecord)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &UserRecord)> {
        self.users.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Returns the number of registered users.
    pub fn len(&self) -> usize {
        self.users.len()
    }

    /// Returns `true` when no users are registered.
    pub fn is_empty(&self) -> bool {
        self.users.is_empty()
    }

    // ── persistence ───────────────────────────────────────────────────────────

    /// Atomically writes the registry to its backing file.
    ///
    /// Writes to a `.tmp` sibling then renames — safe against mid-write crashes.
    /// Returns `Ok(())` when `path` is `None` (in-memory mode).
    pub fn flush(&self) -> Result<(), RegistryError> {
        let path = match &self.path {
            Some(p) => p,
            None => return Ok(()),
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| RegistryError::Io(e.to_string()))?;
        }

        let file = RegistryFile {
            schema_version: 1,
            users: self.users.clone(),
        };
        let json =
            serde_json::to_string_pretty(&file).map_err(|e| RegistryError::Io(e.to_string()))?;

        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, json).map_err(|e| RegistryError::Io(e.to_string()))?;
        std::fs::rename(&tmp, path).map_err(|e| RegistryError::Io(e.to_string()))
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{consent::DataCategory, profile::UserProfile};

    fn make_profile(id: &str) -> UserProfile {
        UserProfile::new(id, "Test User", "telegram", 0)
    }

    #[test]
    fn empty_registry_has_zero_users() {
        let reg = UserRegistry::in_memory();
        assert_eq!(reg.len(), 0);
        assert!(reg.is_empty());
    }

    #[test]
    fn register_adds_user() {
        let mut reg = UserRegistry::in_memory();
        reg.register(make_profile("telegram:1")).unwrap();
        assert_eq!(reg.len(), 1);
        assert!(!reg.is_empty());
    }

    #[test]
    fn register_rejects_duplicate_user_id() {
        let mut reg = UserRegistry::in_memory();
        reg.register(make_profile("telegram:1")).unwrap();
        let err = reg.register(make_profile("telegram:1")).unwrap_err();
        assert_eq!(
            err,
            RegistryError::AlreadyExists {
                user_id: "telegram:1".to_owned()
            }
        );
    }

    #[test]
    fn upsert_creates_new_user() {
        let mut reg = UserRegistry::in_memory();
        let created = reg.upsert(make_profile("telegram:2"));
        assert!(created);
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn upsert_updates_existing_user() {
        let mut reg = UserRegistry::in_memory();
        reg.upsert(make_profile("telegram:2"));
        let mut updated = make_profile("telegram:2");
        updated.display_name = "Updated Name".to_owned();
        let created = reg.upsert(updated);
        assert!(!created);
        assert_eq!(
            reg.get("telegram:2").unwrap().profile.display_name,
            "Updated Name"
        );
    }

    #[test]
    fn get_returns_none_for_missing_user() {
        let reg = UserRegistry::in_memory();
        assert!(reg.get("telegram:99").is_none());
    }

    #[test]
    fn set_trust_updates_tier() {
        let mut reg = UserRegistry::in_memory();
        reg.register(make_profile("telegram:3")).unwrap();
        let (old, new) = reg
            .set_trust("telegram:3", TrustTier::Trusted, 100)
            .unwrap();
        assert_eq!(old, TrustTier::Unknown);
        assert_eq!(new, TrustTier::Trusted);
        assert_eq!(
            reg.get("telegram:3").unwrap().profile.trust_tier,
            TrustTier::Trusted
        );
    }

    #[test]
    fn set_trust_returns_error_for_missing_user() {
        let mut reg = UserRegistry::in_memory();
        let err = reg
            .set_trust("telegram:99", TrustTier::Verified, 0)
            .unwrap_err();
        assert_eq!(
            err,
            RegistryError::NotFound {
                user_id: "telegram:99".to_owned()
            }
        );
    }

    #[test]
    fn set_fact_stores_and_retrieves_value() {
        let mut reg = UserRegistry::in_memory();
        reg.register(make_profile("slack:A")).unwrap();
        reg.set_fact("slack:A", "role", "admin", 0).unwrap();
        assert_eq!(
            reg.get("slack:A").unwrap().profile.get_fact("role"),
            Some("admin")
        );
    }

    #[test]
    fn consent_defaults_to_no_categories() {
        let mut reg = UserRegistry::in_memory();
        reg.register(make_profile("telegram:4")).unwrap();
        let rec = reg.get("telegram:4").unwrap();
        assert!(!rec.consent.is_consented(DataCategory::EpisodicMemory, 0));
    }

    #[test]
    fn consent_is_mutable_via_get_mut() {
        let mut reg = UserRegistry::in_memory();
        reg.register(make_profile("telegram:5")).unwrap();
        reg.get_mut("telegram:5")
            .unwrap()
            .consent
            .set(DataCategory::UsageStats, true, 0);
        assert!(reg
            .get("telegram:5")
            .unwrap()
            .consent
            .is_consented(DataCategory::UsageStats, 0));
    }

    #[test]
    fn flush_and_reload_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("users.json");

        let mut reg = UserRegistry::open(&path).unwrap();
        reg.register(make_profile("telegram:6")).unwrap();
        reg.set_trust("telegram:6", TrustTier::Verified, 1).unwrap();
        reg.flush().unwrap();

        let restored = UserRegistry::open(&path).unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(
            restored.get("telegram:6").unwrap().profile.trust_tier,
            TrustTier::Verified
        );
    }

    #[test]
    fn open_creates_empty_registry_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("users.json");
        let reg = UserRegistry::open(&path).unwrap();
        assert!(reg.is_empty());
    }

    #[test]
    fn in_memory_registry_flush_is_no_op() {
        let reg = UserRegistry::in_memory();
        assert!(reg.flush().is_ok());
        assert!(!reg.is_persistent());
    }

    #[test]
    fn iter_returns_all_users() {
        let mut reg = UserRegistry::in_memory();
        reg.register(make_profile("a:1")).unwrap();
        reg.register(make_profile("a:2")).unwrap();
        let ids: Vec<&str> = reg.iter().map(|(id, _)| id).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"a:1"));
        assert!(ids.contains(&"a:2"));
    }
}
