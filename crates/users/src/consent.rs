#![forbid(unsafe_code)]

//! Per-user data consent model — E17 S17.3.
//!
//! AnimaOS may retain several categories of user data across sessions.  Each
//! category requires explicit opt-in; the default is **no retention**.
//!
//! # Categories
//!
//! | Category | What is stored |
//! |---|---|
//! | `EpisodicMemory` | Conversation summaries in the L3 archive. |
//! | `IdentityFacts` | Key/value facts in the user's [`UserProfile`]. |
//! | `UsageStats` | Aggregated interaction counts (no content). |
//! | `KnowledgeCorpus` | Documents the user shares for RAG retrieval. |
//!
//! # Expiry
//!
//! Every grant carries an optional `expires_at_ns` (Unix nanoseconds).  A
//! consent record past its expiry is treated as **not consented** by
//! [`ConsentRecord::is_consented`].  The operator is responsible for pruning
//! expired records and notifying the user (out of scope for this crate).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── DataCategory ──────────────────────────────────────────────────────────────

/// A category of user data that the agent may retain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataCategory {
    /// Conversation episode summaries in the L3 archive.
    EpisodicMemory,
    /// Free-form facts stored in the [`crate::profile::UserProfile`].
    IdentityFacts,
    /// Aggregated interaction counts (no message content).
    UsageStats,
    /// Documents shared for personal knowledge-corpus retrieval.
    KnowledgeCorpus,
}

impl DataCategory {
    /// Returns a human-readable label for audit entries.
    pub fn as_str(self) -> &'static str {
        match self {
            DataCategory::EpisodicMemory => "episodic_memory",
            DataCategory::IdentityFacts => "identity_facts",
            DataCategory::UsageStats => "usage_stats",
            DataCategory::KnowledgeCorpus => "knowledge_corpus",
        }
    }

    /// All variants, in a stable order.
    pub fn all() -> &'static [DataCategory] {
        &[
            DataCategory::EpisodicMemory,
            DataCategory::IdentityFacts,
            DataCategory::UsageStats,
            DataCategory::KnowledgeCorpus,
        ]
    }
}

impl std::fmt::Display for DataCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for DataCategory {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "episodic_memory" => Ok(DataCategory::EpisodicMemory),
            "identity_facts" => Ok(DataCategory::IdentityFacts),
            "usage_stats" => Ok(DataCategory::UsageStats),
            "knowledge_corpus" => Ok(DataCategory::KnowledgeCorpus),
            other => Err(format!("unknown data category: {other:?}")),
        }
    }
}

// ── Grant ─────────────────────────────────────────────────────────────────────

/// A single data-category grant with an optional expiry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grant {
    /// Whether data in this category may be retained.
    pub granted: bool,
    /// Unix nanoseconds after which this grant expires (`None` = never).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ns: Option<u64>,
    /// Unix nanoseconds when the grant was last updated.
    pub updated_at_ns: u64,
}

impl Grant {
    /// Creates a perpetual grant (no expiry).
    pub fn perpetual(granted: bool, now_ns: u64) -> Self {
        Self {
            granted,
            expires_at_ns: None,
            updated_at_ns: now_ns,
        }
    }

    /// Creates a time-limited grant.
    pub fn until(granted: bool, expires_at_ns: u64, now_ns: u64) -> Self {
        Self {
            granted,
            expires_at_ns: Some(expires_at_ns),
            updated_at_ns: now_ns,
        }
    }

    /// Returns `true` when the grant is active at `now_ns`.
    ///
    /// A revoked (`granted = false`) or expired grant returns `false`.
    pub fn is_active(&self, now_ns: u64) -> bool {
        if !self.granted {
            return false;
        }
        match self.expires_at_ns {
            None => true,
            Some(exp) => now_ns < exp,
        }
    }
}

// ── ConsentRecord ─────────────────────────────────────────────────────────────

/// The full consent state for one user.
///
/// Stores a [`Grant`] per [`DataCategory`]; absent entries are treated as
/// **not consented**.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ConsentRecord {
    /// Per-category grants.
    #[serde(default)]
    grants: HashMap<String, Grant>,
}

impl ConsentRecord {
    /// Creates a record with no consented categories.
    pub fn new() -> Self {
        Self::default()
    }

    /// Grants or revokes consent for `category` (perpetual).
    pub fn set(&mut self, category: DataCategory, granted: bool, now_ns: u64) {
        self.grants.insert(
            category.as_str().to_owned(),
            Grant::perpetual(granted, now_ns),
        );
    }

    /// Sets a time-limited grant for `category`.
    pub fn set_until(
        &mut self,
        category: DataCategory,
        granted: bool,
        expires_at_ns: u64,
        now_ns: u64,
    ) {
        self.grants.insert(
            category.as_str().to_owned(),
            Grant::until(granted, expires_at_ns, now_ns),
        );
    }

    /// Returns `true` when `category` is consented at `now_ns`.
    pub fn is_consented(&self, category: DataCategory, now_ns: u64) -> bool {
        self.grants
            .get(category.as_str())
            .map(|g| g.is_active(now_ns))
            .unwrap_or(false)
    }

    /// Returns an iterator over all (category, grant) pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Grant)> {
        self.grants.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Returns the number of categories that are currently consented.
    pub fn consented_count(&self, now_ns: u64) -> usize {
        self.grants.values().filter(|g| g.is_active(now_ns)).count()
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn default_consent_record_has_no_categories() {
        let r = ConsentRecord::new();
        for cat in DataCategory::all() {
            assert!(
                !r.is_consented(*cat, 0),
                "category {cat} should not be consented by default"
            );
        }
        assert_eq!(r.consented_count(0), 0);
    }

    #[test]
    fn set_grants_and_revokes_consent() {
        let mut r = ConsentRecord::new();
        r.set(DataCategory::EpisodicMemory, true, 0);
        assert!(r.is_consented(DataCategory::EpisodicMemory, 0));
        r.set(DataCategory::EpisodicMemory, false, 1);
        assert!(!r.is_consented(DataCategory::EpisodicMemory, 1));
    }

    #[test]
    fn consented_count_reflects_active_grants() {
        let mut r = ConsentRecord::new();
        r.set(DataCategory::EpisodicMemory, true, 0);
        r.set(DataCategory::UsageStats, true, 0);
        r.set(DataCategory::IdentityFacts, false, 0);
        assert_eq!(r.consented_count(0), 2);
    }

    #[test]
    fn expired_grant_is_not_consented() {
        let mut r = ConsentRecord::new();
        r.set_until(DataCategory::KnowledgeCorpus, true, 1000, 0);
        assert!(r.is_consented(DataCategory::KnowledgeCorpus, 999));
        assert!(!r.is_consented(DataCategory::KnowledgeCorpus, 1000));
        assert!(!r.is_consented(DataCategory::KnowledgeCorpus, 2000));
    }

    #[test]
    fn non_expired_grant_with_future_expiry_is_active() {
        let mut r = ConsentRecord::new();
        r.set_until(DataCategory::UsageStats, true, u64::MAX, 0);
        assert!(r.is_consented(DataCategory::UsageStats, u64::MAX - 1));
    }

    #[test]
    fn consent_record_round_trips_through_json() {
        let mut r = ConsentRecord::new();
        r.set(DataCategory::EpisodicMemory, true, 42);
        r.set_until(DataCategory::KnowledgeCorpus, true, 9999, 1);
        let json = serde_json::to_string(&r).unwrap();
        let restored: ConsentRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, r);
    }

    #[test]
    fn data_category_from_str_round_trips() {
        for cat in DataCategory::all() {
            let parsed = DataCategory::from_str(cat.as_str()).expect("parse");
            assert_eq!(parsed, *cat);
        }
    }

    #[test]
    fn data_category_from_str_rejects_unknown() {
        assert!(DataCategory::from_str("biometrics").is_err());
    }

    #[test]
    fn grant_perpetual_never_expires() {
        let g = Grant::perpetual(true, 0);
        assert!(g.is_active(u64::MAX));
    }

    #[test]
    fn grant_revoked_is_not_active() {
        let g = Grant::perpetual(false, 0);
        assert!(!g.is_active(0));
        assert!(!g.is_active(u64::MAX));
    }
}
