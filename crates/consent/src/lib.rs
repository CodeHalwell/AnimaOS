#![forbid(unsafe_code)]

//! Consent enforcement and data lifecycle management for AnimaOS — Epic E23.
//!
//! The E17 [`users::ConsentRecord`] model defines *which data categories* a
//! user has opted into; this crate wires that model into active enforcement
//! and lifecycle operations:
//!
//! | Module | Purpose |
//! |---|---|
//! | [`check`] | Pre-write consent gate — reject writes for unconsented categories |
//! | [`revoke`] | Revocation directives — generate structured cleanup orders |
//! | [`export`] | Personal-data export (GDPR/DSAR) — collect all held data |
//! | [`expiry`] | Expiry scanner — find consent grants that have lapsed |
//!
//! # Default posture
//!
//! The default consent state (per E17) is **no retention** for every category.
//! All functions in this crate treat an absent consent record — or a record with
//! no active grant — as a denial.

pub use users::{ConsentRecord, DataCategory};

// ── check ─────────────────────────────────────────────────────────────────────

/// Outcome of a pre-write consent check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsentCheckOutcome {
    /// The user has an active grant for this category; the write may proceed.
    Allowed,
    /// The user has not consented (grant absent, revoked, or expired).
    ///
    /// The write **must not** proceed.  The `reason` field carries a human-
    /// readable explanation suitable for an audit entry.
    Denied {
        /// Why the check failed.
        reason: String,
    },
}

impl ConsentCheckOutcome {
    /// Returns `true` when the write is allowed.
    pub fn is_allowed(&self) -> bool {
        matches!(self, ConsentCheckOutcome::Allowed)
    }

    /// Returns the denial reason, or `None` when allowed.
    pub fn denial_reason(&self) -> Option<&str> {
        match self {
            ConsentCheckOutcome::Denied { reason } => Some(reason.as_str()),
            ConsentCheckOutcome::Allowed => None,
        }
    }
}

/// Returns whether a write to `category` for the given user is permitted.
///
/// # Arguments
///
/// * `user_id` — stable user identifier (used only for diagnostic messages).
/// * `category` — the data category being written.
/// * `consent` — the user's current consent record.
/// * `now_ns`  — current wall-clock time in Unix nanoseconds; used to evaluate
///   time-limited grants.
///
/// # Examples
///
/// ```
/// use consent::{check_write_allowed, ConsentCheckOutcome, ConsentRecord, DataCategory};
///
/// let mut rec = ConsentRecord::new();
/// rec.set(DataCategory::EpisodicMemory, true, 0);
///
/// assert!(check_write_allowed("alice", DataCategory::EpisodicMemory, &rec, 1).is_allowed());
/// assert!(!check_write_allowed("alice", DataCategory::IdentityFacts, &rec, 1).is_allowed());
/// ```
pub fn check_write_allowed(
    user_id: &str,
    category: DataCategory,
    consent: &ConsentRecord,
    now_ns: u64,
) -> ConsentCheckOutcome {
    if consent.is_consented(category, now_ns) {
        ConsentCheckOutcome::Allowed
    } else {
        ConsentCheckOutcome::Denied {
            reason: format!(
                "user {user_id:?} has not consented to retain data in category \
                 {cat}; write blocked",
                cat = category.as_str(),
            ),
        }
    }
}

// ── revoke ────────────────────────────────────────────────────────────────────

/// A structured cleanup order generated when one or more data categories are
/// revoked for a user.
///
/// The operator (or automated lifecycle engine) reads the flags and deletes the
/// corresponding records from each affected store.  The `consent` crate does not
/// touch any storage directly; it only produces the directive.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RevocationDirective {
    /// Stable user identifier.
    pub user_id: String,
    /// Data categories whose grants were revoked.
    pub revoked_categories: Vec<String>,
    /// Whether conversation sessions should be purged.
    pub purge_sessions: bool,
    /// Whether L3 episodic-memory archive entries should be purged.
    pub purge_episodic_memory: bool,
    /// Whether identity facts in the user profile should be cleared.
    pub purge_identity_facts: bool,
    /// Whether personal knowledge-corpus entries should be removed.
    pub purge_knowledge_corpus: bool,
    /// Whether aggregated usage statistics should be deleted.
    pub purge_usage_stats: bool,
    /// Nanosecond timestamp when the directive was created.
    pub created_at_ns: u64,
}

/// Builds a [`RevocationDirective`] from a list of revoked data categories.
///
/// Maps each revoked [`DataCategory`] to the stores that hold data of that
/// type so the caller knows exactly which storage backends to purge.
pub fn build_revocation_directive(
    user_id: &str,
    revoked_categories: &[DataCategory],
    now_ns: u64,
) -> RevocationDirective {
    let mut directive = RevocationDirective {
        user_id: user_id.to_owned(),
        revoked_categories: revoked_categories
            .iter()
            .map(|c| c.as_str().to_owned())
            .collect(),
        purge_sessions: false,
        purge_episodic_memory: false,
        purge_identity_facts: false,
        purge_knowledge_corpus: false,
        purge_usage_stats: false,
        created_at_ns: now_ns,
    };

    for cat in revoked_categories {
        match cat {
            DataCategory::EpisodicMemory => {
                directive.purge_episodic_memory = true;
                // Conversation sessions contain episodic content; purge both.
                directive.purge_sessions = true;
            }
            DataCategory::IdentityFacts => {
                directive.purge_identity_facts = true;
            }
            DataCategory::UsageStats => {
                directive.purge_usage_stats = true;
            }
            DataCategory::KnowledgeCorpus => {
                directive.purge_knowledge_corpus = true;
            }
        }
    }

    directive
}

// ── export ────────────────────────────────────────────────────────────────────

/// A single category section inside a [`DataExportBundle`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DataExportSection {
    /// Data category label (e.g. `"episodic_memory"`).
    pub category: String,
    /// Number of records included.
    pub record_count: usize,
    /// The exported data.  Schema is category-specific; consumers should inspect
    /// the `category` field first.
    pub data: serde_json::Value,
}

/// A complete personal-data export bundle for one user, suitable for GDPR Data
/// Subject Access Requests (DSAR) or operator-initiated backups.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DataExportBundle {
    /// Stable user identifier.
    pub user_id: String,
    /// Agent that owns the exported data.
    pub agent_id: String,
    /// Wall-clock nanoseconds when the export was generated.
    pub exported_at_ns: u64,
    /// Per-category sections.
    pub sections: Vec<DataExportSection>,
    /// Total record count across all sections.
    pub total_records: usize,
}

/// Accumulates per-category data into a [`DataExportBundle`].
///
/// Callers add sections via [`DataExportBuilder::add_section`] and then call
/// [`DataExportBuilder::build`] to produce the final bundle.
#[derive(Debug, Default)]
pub struct DataExportBuilder {
    sections: Vec<DataExportSection>,
}

impl DataExportBuilder {
    /// Creates an empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a data section for `category`.
    ///
    /// `data` must be a JSON value that meaningfully represents the user's
    /// retained data for this category.  `record_count` should reflect the
    /// number of logical records (documents, turns, facts, …) in `data`.
    pub fn add_section(
        &mut self,
        category: DataCategory,
        record_count: usize,
        data: serde_json::Value,
    ) -> &mut Self {
        self.sections.push(DataExportSection {
            category: category.as_str().to_owned(),
            record_count,
            data,
        });
        self
    }

    /// Appends a data section using a free-form category label.
    ///
    /// Use this for sections that don't map to a [`DataCategory`] variant, such
    /// as the user's identity profile.
    pub fn add_raw_section(
        &mut self,
        category: impl Into<String>,
        record_count: usize,
        data: serde_json::Value,
    ) -> &mut Self {
        self.sections.push(DataExportSection {
            category: category.into(),
            record_count,
            data,
        });
        self
    }

    /// Builds the final export bundle.
    pub fn build(self, user_id: &str, agent_id: &str, exported_at_ns: u64) -> DataExportBundle {
        let total_records = self.sections.iter().map(|s| s.record_count).sum();
        DataExportBundle {
            user_id: user_id.to_owned(),
            agent_id: agent_id.to_owned(),
            exported_at_ns,
            sections: self.sections,
            total_records,
        }
    }
}

// ── expiry ────────────────────────────────────────────────────────────────────

/// A consent grant that has lapsed and should be treated as revoked.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExpiredGrant {
    /// Stable user identifier.
    pub user_id: String,
    /// Category whose grant has expired.
    pub category: String,
    /// Unix nanoseconds when the grant expired.
    pub expired_at_ns: u64,
}

/// Report produced by [`scan_expired_grants`].
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ExpiryReport {
    /// Number of users whose consent records were scanned.
    pub users_scanned: usize,
    /// All grants that have lapsed at `now_ns`.
    pub expired_grants: Vec<ExpiredGrant>,
    /// Revocation directives, one per user that has at least one expired grant.
    pub directives: Vec<RevocationDirective>,
}

impl ExpiryReport {
    /// Returns the number of expired grants found.
    pub fn expired_count(&self) -> usize {
        self.expired_grants.len()
    }

    /// Returns `true` when no grants have expired.
    pub fn is_clean(&self) -> bool {
        self.expired_grants.is_empty()
    }
}

/// Scans a collection of `(user_id, ConsentRecord)` pairs for lapsed grants.
///
/// Returns an [`ExpiryReport`] containing every grant that has expired at
/// `now_ns`, together with pre-built [`RevocationDirective`]s ready for the
/// caller to execute.
///
/// This function is intentionally pure (no I/O); it is designed to be called
/// at the end of a sleep cycle so the lifecycle manager can clean up expired
/// data without live user interaction.
///
/// # Arguments
///
/// * `users` — iterator of `(user_id, consent_record)` pairs to scan.
/// * `now_ns` — current wall-clock time used for grant expiry evaluation.
pub fn scan_expired_grants<'a>(
    users: impl Iterator<Item = (&'a str, &'a ConsentRecord)>,
    now_ns: u64,
) -> ExpiryReport {
    let mut report = ExpiryReport::default();

    for (user_id, consent) in users {
        report.users_scanned += 1;
        let mut user_revoked: Vec<DataCategory> = Vec::new();

        for cat in DataCategory::all() {
            // Inspect every grant directly via the iter API.
            // A grant is "expired" when it *was* active (granted=true) but has
            // now passed its expiry, making is_consented return false while the
            // raw grant is still present.
            let expired_grant = consent.iter().find(|(key, grant)| {
                *key == cat.as_str()
                    && grant.granted
                    && grant
                        .expires_at_ns
                        .map(|exp| now_ns >= exp)
                        .unwrap_or(false)
            });

            if let Some((_, grant)) = expired_grant {
                let expired_at_ns = grant.expires_at_ns.unwrap_or(now_ns);

                report.expired_grants.push(ExpiredGrant {
                    user_id: user_id.to_owned(),
                    category: cat.as_str().to_owned(),
                    expired_at_ns,
                });
                user_revoked.push(*cat);
            }
        }

        if !user_revoked.is_empty() {
            report
                .directives
                .push(build_revocation_directive(user_id, &user_revoked, now_ns));
        }
    }

    report
}

// ── CleanupSummary ────────────────────────────────────────────────────────────

/// Aggregated summary of a data-lifecycle cleanup pass.
///
/// Produced by the operator after executing one or more [`RevocationDirective`]s.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CleanupSummary {
    /// Number of users whose data was affected.
    pub users_affected: usize,
    /// Number of session records deleted.
    pub sessions_deleted: usize,
    /// Number of L3 episodic-memory entries deleted.
    pub episodic_entries_deleted: usize,
    /// Number of identity facts cleared.
    pub identity_facts_cleared: usize,
    /// Number of knowledge-corpus entries removed.
    pub knowledge_entries_deleted: usize,
    /// Number of usage-stats records removed.
    pub usage_stats_deleted: usize,
}

impl CleanupSummary {
    /// Total number of records deleted across all categories.
    pub fn total_deleted(&self) -> usize {
        self.sessions_deleted
            + self.episodic_entries_deleted
            + self.identity_facts_cleared
            + self.knowledge_entries_deleted
            + self.usage_stats_deleted
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── check_write_allowed ───────────────────────────────────────────────────

    #[test]
    fn allowed_when_category_is_consented() {
        let mut rec = ConsentRecord::new();
        rec.set(DataCategory::EpisodicMemory, true, 0);
        let outcome = check_write_allowed("alice", DataCategory::EpisodicMemory, &rec, 1);
        assert!(outcome.is_allowed());
        assert!(outcome.denial_reason().is_none());
    }

    #[test]
    fn denied_when_category_is_not_consented() {
        let rec = ConsentRecord::new();
        let outcome = check_write_allowed("bob", DataCategory::IdentityFacts, &rec, 0);
        assert!(!outcome.is_allowed());
        let reason = outcome.denial_reason().expect("should have reason");
        assert!(reason.contains("bob"), "reason should mention user_id");
        assert!(
            reason.contains("identity_facts"),
            "reason should mention category"
        );
    }

    #[test]
    fn denied_when_grant_is_revoked() {
        let mut rec = ConsentRecord::new();
        rec.set(DataCategory::UsageStats, true, 0);
        rec.set(DataCategory::UsageStats, false, 1);
        let outcome = check_write_allowed("carol", DataCategory::UsageStats, &rec, 2);
        assert!(!outcome.is_allowed());
    }

    #[test]
    fn denied_when_grant_has_expired() {
        let mut rec = ConsentRecord::new();
        rec.set_until(DataCategory::KnowledgeCorpus, true, 500, 0);
        // Before expiry: allowed
        assert!(check_write_allowed("dave", DataCategory::KnowledgeCorpus, &rec, 499).is_allowed());
        // At and after expiry: denied
        assert!(
            !check_write_allowed("dave", DataCategory::KnowledgeCorpus, &rec, 500).is_allowed()
        );
    }

    #[test]
    fn all_categories_denied_by_default() {
        let rec = ConsentRecord::new();
        for cat in DataCategory::all() {
            assert!(
                !check_write_allowed("eve", *cat, &rec, 0).is_allowed(),
                "category {cat} should be denied by default"
            );
        }
    }

    // ── build_revocation_directive ────────────────────────────────────────────

    #[test]
    fn directive_sets_correct_purge_flags_for_episodic_memory() {
        let d = build_revocation_directive("u1", &[DataCategory::EpisodicMemory], 42);
        assert!(d.purge_episodic_memory, "episodic memory should be purged");
        assert!(d.purge_sessions, "sessions share episodic content");
        assert!(!d.purge_identity_facts);
        assert!(!d.purge_knowledge_corpus);
        assert!(!d.purge_usage_stats);
    }

    #[test]
    fn directive_sets_correct_purge_flags_for_identity_facts() {
        let d = build_revocation_directive("u2", &[DataCategory::IdentityFacts], 0);
        assert!(d.purge_identity_facts);
        assert!(!d.purge_episodic_memory);
        assert!(!d.purge_sessions);
    }

    #[test]
    fn directive_sets_correct_purge_flags_for_usage_stats() {
        let d = build_revocation_directive("u3", &[DataCategory::UsageStats], 0);
        assert!(d.purge_usage_stats);
        assert!(!d.purge_sessions);
    }

    #[test]
    fn directive_sets_correct_purge_flags_for_knowledge_corpus() {
        let d = build_revocation_directive("u4", &[DataCategory::KnowledgeCorpus], 0);
        assert!(d.purge_knowledge_corpus);
        assert!(!d.purge_sessions);
    }

    #[test]
    fn directive_for_all_categories_sets_all_flags() {
        let all: Vec<DataCategory> = DataCategory::all().to_vec();
        let d = build_revocation_directive("u5", &all, 999);
        assert!(d.purge_episodic_memory);
        assert!(d.purge_sessions);
        assert!(d.purge_identity_facts);
        assert!(d.purge_knowledge_corpus);
        assert!(d.purge_usage_stats);
        assert_eq!(d.user_id, "u5");
        assert_eq!(d.created_at_ns, 999);
    }

    #[test]
    fn directive_serialises_and_deserialises() {
        let d = build_revocation_directive("u6", &[DataCategory::UsageStats], 0);
        let json = serde_json::to_string(&d).unwrap();
        let back: RevocationDirective = serde_json::from_str(&json).unwrap();
        assert_eq!(back.user_id, d.user_id);
        assert_eq!(back.purge_usage_stats, d.purge_usage_stats);
    }

    // ── DataExportBuilder ─────────────────────────────────────────────────────

    #[test]
    fn export_builder_accumulates_sections_and_counts_records() {
        let mut builder = DataExportBuilder::new();
        builder.add_section(
            DataCategory::EpisodicMemory,
            3,
            serde_json::json!(["ep1", "ep2", "ep3"]),
        );
        builder.add_section(
            DataCategory::IdentityFacts,
            2,
            serde_json::json!({"name": "Alice", "pref": "email"}),
        );
        let bundle = builder.build("alice", "anima", 12345);
        assert_eq!(bundle.user_id, "alice");
        assert_eq!(bundle.agent_id, "anima");
        assert_eq!(bundle.exported_at_ns, 12345);
        assert_eq!(bundle.sections.len(), 2);
        assert_eq!(bundle.total_records, 5);
    }

    #[test]
    fn empty_export_bundle_has_zero_records() {
        let bundle = DataExportBuilder::new().build("x", "anima", 0);
        assert_eq!(bundle.total_records, 0);
        assert!(bundle.sections.is_empty());
    }

    #[test]
    fn export_bundle_round_trips_through_json() {
        let mut builder = DataExportBuilder::new();
        builder.add_section(
            DataCategory::UsageStats,
            1,
            serde_json::json!({"requests": 42}),
        );
        let bundle = builder.build("u", "a", 99);
        let json = serde_json::to_string(&bundle).unwrap();
        let back: DataExportBundle = serde_json::from_str(&json).unwrap();
        assert_eq!(back.total_records, bundle.total_records);
        assert_eq!(back.sections[0].category, "usage_stats");
    }

    // ── scan_expired_grants ───────────────────────────────────────────────────

    #[test]
    fn scan_returns_empty_report_when_no_users() {
        let report = scan_expired_grants(std::iter::empty(), 1000);
        assert_eq!(report.users_scanned, 0);
        assert!(report.is_clean());
    }

    #[test]
    fn scan_finds_expired_grant() {
        let mut rec = ConsentRecord::new();
        rec.set_until(DataCategory::UsageStats, true, 100, 0);

        let users = [("alice", &rec)];
        let report = scan_expired_grants(
            users.iter().map(|(id, r)| (*id, *r)),
            200, // now > 100, so grant is expired
        );

        assert_eq!(report.users_scanned, 1);
        assert_eq!(report.expired_count(), 1);
        assert!(!report.is_clean());
        assert_eq!(report.expired_grants[0].user_id, "alice");
        assert_eq!(report.expired_grants[0].category, "usage_stats");
        assert_eq!(report.directives.len(), 1);
        assert!(report.directives[0].purge_usage_stats);
    }

    #[test]
    fn scan_does_not_flag_unexpired_grants() {
        let mut rec = ConsentRecord::new();
        rec.set_until(DataCategory::EpisodicMemory, true, 1000, 0);

        let users = [("bob", &rec)];
        let report = scan_expired_grants(
            users.iter().map(|(id, r)| (*id, *r)),
            500, // still before expiry
        );

        assert!(report.is_clean(), "grant should not be expired yet");
    }

    #[test]
    fn scan_does_not_flag_perpetual_grants() {
        let mut rec = ConsentRecord::new();
        rec.set(DataCategory::IdentityFacts, true, 0);

        let users = [("carol", &rec)];
        let report = scan_expired_grants(users.iter().map(|(id, r)| (*id, *r)), u64::MAX);

        assert!(report.is_clean(), "perpetual grant must never expire");
    }

    #[test]
    fn scan_does_not_flag_revoked_grants_as_expired() {
        let mut rec = ConsentRecord::new();
        rec.set(DataCategory::KnowledgeCorpus, false, 0);

        let users = [("dave", &rec)];
        let report = scan_expired_grants(users.iter().map(|(id, r)| (*id, *r)), 0);

        assert!(
            report.is_clean(),
            "explicitly revoked grant is not an expired grant"
        );
    }

    #[test]
    fn scan_multiple_users_produces_correct_counts() {
        let mut rec_a = ConsentRecord::new();
        rec_a.set_until(DataCategory::UsageStats, true, 50, 0);

        let mut rec_b = ConsentRecord::new();
        rec_b.set_until(DataCategory::IdentityFacts, true, 50, 0);
        rec_b.set(DataCategory::EpisodicMemory, true, 0); // perpetual, not expired

        let users = [("a", &rec_a), ("b", &rec_b)];
        let report = scan_expired_grants(users.iter().map(|(id, r)| (*id, *r)), 100);

        assert_eq!(report.users_scanned, 2);
        assert_eq!(report.expired_count(), 2);
        assert_eq!(report.directives.len(), 2);
    }

    #[test]
    fn expiry_report_serialises_cleanly() {
        let report = ExpiryReport {
            users_scanned: 1,
            expired_grants: vec![ExpiredGrant {
                user_id: "u".to_owned(),
                category: "usage_stats".to_owned(),
                expired_at_ns: 42,
            }],
            directives: vec![],
        };
        let json = serde_json::to_string(&report).unwrap();
        let back: ExpiryReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back.expired_count(), 1);
    }

    // ── CleanupSummary ────────────────────────────────────────────────────────

    #[test]
    fn cleanup_summary_total_deleted_sums_all_fields() {
        let s = CleanupSummary {
            users_affected: 2,
            sessions_deleted: 3,
            episodic_entries_deleted: 5,
            identity_facts_cleared: 2,
            knowledge_entries_deleted: 1,
            usage_stats_deleted: 4,
        };
        assert_eq!(s.total_deleted(), 15);
    }

    #[test]
    fn cleanup_summary_default_is_zero() {
        let s = CleanupSummary::default();
        assert_eq!(s.total_deleted(), 0);
        assert_eq!(s.users_affected, 0);
    }
}
