// crates/vita/src/episodic.rs
//! E5.5 — Episodic Memory store and retrieval (S5.5.1, S5.5.2).
//!
//! Episodic memory records what happened across cortex invocations. Each
//! episode is stored in the L3 archive with [`memory::SourceTier::Episode`]
//! provenance and a 4-dimensional embedding derived from the episode's
//! timing, outcome, and summary length.
//!
//! # Schema (S5.5.1)
//!
//! Each [`EpisodeRecord`] carries:
//! - `invocation_id` — stable per-invocation identifier (matches `task_id`)
//! - `event_class`   — routing event class (e.g. `"UserQuery"`)
//! - `route_id`      — thalamic route that handled the invocation
//! - `started_at_ns` — wall-clock nanoseconds since UNIX epoch at start
//! - `ended_at_ns`   — wall-clock nanoseconds since UNIX epoch at end
//! - `outcome`       — `"success"` | `"fault"` | `"cancelled"`
//! - `summary`       — compact free-text episode summary from the cortex
//!
//! The string fields that do not fit in the fixed 20-byte archive payload are
//! packed into the L3 provenance `source_key` as a pipe-delimited string:
//! `invocation_id|event_class|route_id|outcome|summary`.
//!
//! # Embedding (S5.5.1 retrieval vector)
//!
//! The 4-dimensional embedding has the following components:
//!
//! | Dim | Meaning |
//! |-----|---------|
//! |  0  | Success flag: `1.0` if outcome == `"success"`, `0.0` otherwise |
//! |  1  | Duration normalised: `1 / (1 + duration_secs)` |
//! |  2  | Recency: `1 / (1 + age_secs)` where `age_secs` is elapsed since `ended_at_ns` |
//! |  3  | Summary length normalised: `len / 256.0`, capped at `1.0` |
//!
//! # Retrieval (S5.5.2)
//!
//! [`EpisodeStore::retrieve`] filters the L3 archive to `SourceTier::Episode`
//! entries, optionally applies a recency cutoff, ranks by cosine similarity,
//! and returns the top-k results as [`EpisodeMatch`] values.

#![forbid(unsafe_code)]

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use memory::{
    ArchivalEntry, ArchivedItem, DemotionOutcome, L3Archive, L3ArchiveError, Provenance, SourceTier,
};
use serde::{Deserialize, Serialize};

// ── Episode record ─────────────────────────────────────────────────────────

/// A single episodic memory record describing one cortex invocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EpisodeRecord {
    /// Stable per-invocation identifier (matches `InvokeRequest::task_id`).
    pub invocation_id: String,
    /// String representation of the routing event class (e.g. `"UserQuery"`).
    pub event_class: String,
    /// Thalamic route that handled this invocation (e.g. `"cheap-local"`).
    pub route_id: String,
    /// Wall-clock nanoseconds since UNIX epoch at invocation start.
    pub started_at_ns: u64,
    /// Wall-clock nanoseconds since UNIX epoch at invocation end.
    pub ended_at_ns: u64,
    /// Outcome of the invocation: `"success"`, `"fault"`, or `"cancelled"`.
    pub outcome: String,
    /// Compact free-text episode summary produced by the cortex.
    pub summary: String,
}

impl EpisodeRecord {
    /// Constructs a new episode record with timestamps set to the current time.
    pub fn new(
        invocation_id: impl Into<String>,
        event_class: impl Into<String>,
        route_id: impl Into<String>,
        outcome: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        let now = Self::now_ns();
        Self {
            invocation_id: invocation_id.into(),
            event_class: event_class.into(),
            route_id: route_id.into(),
            started_at_ns: now,
            ended_at_ns: now,
            outcome: outcome.into(),
            summary: summary.into(),
        }
    }

    /// Returns the wall-clock duration of this episode in seconds.
    pub fn duration_secs(&self) -> f64 {
        let ns = self.ended_at_ns.saturating_sub(self.started_at_ns);
        ns as f64 / 1_000_000_000.0
    }

    /// Returns `true` if the invocation completed successfully.
    pub fn is_success(&self) -> bool {
        self.outcome == "success"
    }

    /// Returns the current wall-clock time as nanoseconds since UNIX epoch.
    ///
    /// Available only with the `std` feature; `no_std` builds receive `0` and
    /// must rely on caller-supplied `started_at_ns` / `ended_at_ns` values.
    pub fn now_ns() -> u64 {
        #[cfg(feature = "std")]
        {
            use std::time::{SystemTime, UNIX_EPOCH};
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64
        }
        #[cfg(not(feature = "std"))]
        {
            0
        }
    }
}

// ── Embedding ──────────────────────────────────────────────────────────────

/// Compute the 4-dimensional retrieval embedding for an episode record.
///
/// | Dim | Meaning |
/// |-----|---------|
/// |  0  | Success flag (`1.0` / `0.0`) |
/// |  1  | `1 / (1 + duration_secs)` |
/// |  2  | `1 / (1 + age_secs)` — recency |
/// |  3  | `(summary_len / 256).min(1.0)` |
pub fn embed_episode(record: &EpisodeRecord) -> Vec<f32> {
    let success = if record.is_success() {
        1.0_f32
    } else {
        0.0_f32
    };
    let duration_norm = 1.0_f32 / (1.0_f32 + record.duration_secs() as f32);
    let now_ns = EpisodeRecord::now_ns();
    let age_secs = now_ns.saturating_sub(record.ended_at_ns) as f64 / 1_000_000_000.0;
    let recency = 1.0_f32 / (1.0_f32 + age_secs as f32);
    let summary_len = (record.summary.len() as f32 / 256.0).min(1.0);
    vec![success, duration_norm, recency, summary_len]
}

// ── Archive packing / unpacking ────────────────────────────────────────────

/// Serialise an [`EpisodeRecord`] into a 20-byte binary archive payload.
///
/// Layout (all little-endian):
/// ```text
/// [0..8]   started_at_ns (u64)
/// [8..16]  ended_at_ns   (u64)
/// [16..20] summary_len   (u32) — byte length of the summary UTF-8 string
/// ```
///
/// The `invocation_id`, `event_class`, `route_id`, `outcome`, and `summary`
/// are encoded in the L3 provenance `source_key` field as a pipe-delimited
/// string: `invocation_id|event_class|route_id|outcome|summary`.
pub fn pack_episode_payload(record: &EpisodeRecord) -> Vec<u8> {
    let mut buf = Vec::with_capacity(20);
    buf.extend_from_slice(&record.started_at_ns.to_le_bytes());
    buf.extend_from_slice(&record.ended_at_ns.to_le_bytes());
    buf.extend_from_slice(&(record.summary.len() as u32).to_le_bytes());
    buf
}

/// Reconstruct an [`EpisodeRecord`] from a raw [`ArchivalEntry`].
///
/// Returns `None` if the payload is too short or the provenance key is
/// malformed.
pub fn unpack_episode(entry: &ArchivalEntry) -> Option<EpisodeRecord> {
    let payload = &entry.item.payload;
    if payload.len() < 20 {
        return None;
    }
    let started_at_ns = u64::from_le_bytes(payload[0..8].try_into().ok()?);
    let ended_at_ns = u64::from_le_bytes(payload[8..16].try_into().ok()?);

    // source_key: "invocation_id|event_class|route_id|outcome|summary"
    let key = &entry.provenance.source_key;
    // splitn(5, …) splits on the first 4 '|' delimiters, collecting the last
    // field (summary) as a single chunk even when it contains '|' characters.
    let parts: Vec<&str> = key.splitn(5, '|').collect();
    if parts.len() < 5 {
        return None;
    }
    Some(EpisodeRecord {
        invocation_id: parts[0].to_string(),
        event_class: parts[1].to_string(),
        route_id: parts[2].to_string(),
        started_at_ns,
        ended_at_ns,
        outcome: parts[3].to_string(),
        summary: parts[4].to_string(),
    })
}

/// Construct an [`ArchivedItem`] ready for insertion into the L3 archive.
pub fn make_episode_archived_item(id: u64, record: &EpisodeRecord) -> ArchivedItem {
    ArchivedItem {
        id,
        embedding: embed_episode(record),
        payload: pack_episode_payload(record),
    }
}

/// Construct a [`Provenance`] record that encodes the string fields of an
/// episode.
///
/// The `source_key` is `invocation_id|event_class|route_id|outcome|summary`.
pub fn make_episode_provenance(record: &EpisodeRecord) -> Provenance {
    let source_key = format!(
        "{}|{}|{}|{}|{}",
        record.invocation_id, record.event_class, record.route_id, record.outcome, record.summary,
    );
    // `no_std` builds substitute the record's own `ended_at_ns` for the
    // wall-clock timestamp `Provenance::now` would normally synthesise.
    #[cfg(feature = "std")]
    {
        Provenance::now(SourceTier::Episode, &source_key)
    }
    #[cfg(not(feature = "std"))]
    {
        Provenance::at_ns(SourceTier::Episode, &source_key, record.ended_at_ns)
    }
}

// ── Retrieval ─────────────────────────────────────────────────────────────

/// A single result from an episodic retrieval query.
#[derive(Debug, Clone)]
pub struct EpisodeMatch {
    /// The reconstructed episode record.
    pub record: EpisodeRecord,
    /// Cosine similarity score between the query embedding and this episode's
    /// stored embedding (`[0.0, 1.0]`).
    pub score: f32,
}

/// Query parameters for [`EpisodeStore::retrieve`].
#[derive(Debug, Clone)]
pub struct EpisodeQuery {
    /// 4-dimensional embedding vector to rank against stored episodes.
    pub embedding: Vec<f32>,
    /// Maximum number of results to return.
    pub k: usize,
    /// Optional recency window: episodes whose `ended_at_ns` is older than
    /// `now_ns - cutoff_ns` are excluded before similarity ranking.
    pub cutoff_ns: Option<u64>,
}

impl EpisodeQuery {
    /// Convenience constructor: top-k query with no recency cutoff.
    pub fn top_k(embedding: Vec<f32>, k: usize) -> Self {
        Self {
            embedding,
            k,
            cutoff_ns: None,
        }
    }

    /// Adds a recency window (in nanoseconds) to the query.
    pub fn with_recency_cutoff(mut self, cutoff_ns: u64) -> Self {
        self.cutoff_ns = Some(cutoff_ns);
        self
    }
}

// ── EpisodeStore ───────────────────────────────────────────────────────────

/// A thin façade over [`L3Archive`] that operates exclusively on episodic
/// entries (`SourceTier::Episode`).
///
/// `EpisodeStore` does not own the archive; callers pass a reference so that
/// the same L3 backing store holds both somatic `MemoryNode` entries and
/// cognitive `EpisodeRecord` entries.
pub struct EpisodeStore;

impl EpisodeStore {
    /// Archive an episode record into the L3 archive.
    ///
    /// Returns the [`DemotionOutcome`] from the underlying archive store
    /// (`Inserted` or `AlreadyPresent`).
    pub fn archive(
        archive: &mut L3Archive,
        id: u64,
        record: &EpisodeRecord,
    ) -> Result<DemotionOutcome, L3ArchiveError> {
        let item = make_episode_archived_item(id, record);
        let prov = make_episode_provenance(record);
        archive.demote(item, prov)
    }

    /// Retrieve the top-k episodic memories matching a query embedding.
    ///
    /// Only entries whose provenance `source_tier` is `SourceTier::Episode`
    /// are considered.  Results are ranked by cosine similarity in descending
    /// order.
    ///
    /// If `query.cutoff_ns` is set, episodes whose `ended_at_ns` is older
    /// than `now_ns - cutoff_ns` are excluded before ranking.
    pub fn retrieve(archive: &L3Archive, query: &EpisodeQuery) -> Vec<EpisodeMatch> {
        let now_ns = EpisodeRecord::now_ns();
        let cutoff_ns_threshold = query.cutoff_ns.map(|c| now_ns.saturating_sub(c));

        let all_entries = archive.entries();

        // Filter to Episode provenance only.
        let episode_entries: Vec<&ArchivalEntry> = all_entries
            .into_iter()
            .filter(|e| matches!(e.provenance.source_tier, SourceTier::Episode))
            .collect();

        // Apply optional recency cutoff.
        let candidates: Vec<&ArchivalEntry> = if let Some(cutoff) = cutoff_ns_threshold {
            episode_entries
                .into_iter()
                .filter(|e| {
                    unpack_episode(e)
                        .map(|rec| rec.ended_at_ns >= cutoff)
                        .unwrap_or(false)
                })
                .collect()
        } else {
            episode_entries
        };

        // Score candidates by cosine similarity.
        let mut scored: Vec<(f32, EpisodeRecord)> = candidates
            .into_iter()
            .filter_map(|e| {
                let score = cosine_similarity(&query.embedding, &e.item.embedding);
                let rec = unpack_episode(e)?;
                Some((score, rec))
            })
            .collect();

        // Descending similarity, then ascending invocation_id for determinism.
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(core::cmp::Ordering::Equal)
                .then_with(|| a.1.invocation_id.cmp(&b.1.invocation_id))
        });
        scored.truncate(query.k);

        scored
            .into_iter()
            .map(|(score, record)| EpisodeMatch { record, score })
            .collect()
    }
}

// ── Cosine similarity ──────────────────────────────────────────────────────

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let mag_a: f32 = libm::sqrtf(a.iter().map(|x| x * x).sum::<f32>());
    let mag_b: f32 = libm::sqrtf(b.iter().map(|x| x * x).sum::<f32>());
    if mag_a < 1e-8 || mag_b < 1e-8 {
        0.0
    } else {
        dot / (mag_a * mag_b)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use memory::{L3Archive, SourceTier};

    // Helper: open a temp L3 archive.
    fn temp_archive(name: &str) -> (L3Archive, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!("animaos_test_episodic_{name}.json"));
        let _ = std::fs::remove_file(&path);
        let archive = L3Archive::open(&path, 4, 500).expect("failed to open temp archive");
        (archive, path)
    }

    fn episode(id: &str, outcome: &str, summary: &str) -> EpisodeRecord {
        EpisodeRecord {
            invocation_id: id.to_string(),
            event_class: "UserQuery".to_string(),
            route_id: "cheap-local".to_string(),
            started_at_ns: 1_000_000_000,
            ended_at_ns: 2_000_000_000,
            outcome: outcome.to_string(),
            summary: summary.to_string(),
        }
    }

    // S5.5.1 ─────────────────────────────────────────────────────────────────

    /// Episode records round-trip through pack/unpack without data loss.
    #[test]
    fn episode_payload_round_trips_through_pack_unpack() {
        let rec = episode("inv-1", "success", "the task was completed successfully");
        let item_id = 42_u64;
        let item = make_episode_archived_item(item_id, &rec);
        let prov = make_episode_provenance(&rec);
        let entry = memory::ArchivalEntry {
            item,
            provenance: prov,
        };
        let unpacked = unpack_episode(&entry).expect("unpack must succeed");
        assert_eq!(unpacked.invocation_id, rec.invocation_id);
        assert_eq!(unpacked.event_class, rec.event_class);
        assert_eq!(unpacked.route_id, rec.route_id);
        assert_eq!(unpacked.started_at_ns, rec.started_at_ns);
        assert_eq!(unpacked.ended_at_ns, rec.ended_at_ns);
        assert_eq!(unpacked.outcome, rec.outcome);
        assert_eq!(unpacked.summary, rec.summary);
    }

    /// Summary strings containing '|' characters are preserved correctly.
    #[test]
    fn episode_summary_containing_pipe_survives_round_trip() {
        let rec = episode("inv-pipe", "success", "step 1 | step 2 | step 3");
        let item = make_episode_archived_item(1, &rec);
        let prov = make_episode_provenance(&rec);
        let entry = memory::ArchivalEntry {
            item,
            provenance: prov,
        };
        let unpacked = unpack_episode(&entry).expect("unpack must succeed");
        assert_eq!(unpacked.summary, "step 1 | step 2 | step 3");
    }

    /// Episode embedding has exactly 4 dimensions.
    #[test]
    fn episode_embedding_is_four_dimensional() {
        let rec = episode("inv-2", "success", "hello");
        let emb = embed_episode(&rec);
        assert_eq!(emb.len(), 4);
    }

    /// Success flag is 1.0 for successful episodes and 0.0 for faults.
    #[test]
    fn episode_embedding_success_flag_is_correct() {
        let ok = episode("inv-ok", "success", "ok");
        let fail = episode("inv-fail", "fault", "err");
        assert!((embed_episode(&ok)[0] - 1.0).abs() < f32::EPSILON);
        assert!((embed_episode(&fail)[0] - 0.0).abs() < f32::EPSILON);
    }

    /// `EpisodeStore::archive` stores the entry in L3 with Episode provenance.
    #[test]
    fn episode_store_archives_into_l3_with_episode_provenance() {
        let (mut archive, path) = temp_archive("archive_provenance");
        let rec = episode("inv-3", "success", "archived episode");
        EpisodeStore::archive(&mut archive, 1, &rec).expect("archive must succeed");
        let entries = archive.entries();
        assert_eq!(entries.len(), 1);
        assert!(matches!(
            entries[0].provenance.source_tier,
            SourceTier::Episode
        ));
        let _ = std::fs::remove_file(&path);
    }

    // S5.5.2 ─────────────────────────────────────────────────────────────────

    /// Episodic retrieval returns the correct episode for a known embedding.
    #[test]
    fn episodic_retrieval_returns_correct_episode_for_benchmark_pair() {
        let (mut archive, path) = temp_archive("retrieval_benchmark");

        // Store two episodes with distinct embeddings.
        let success_rec = EpisodeRecord {
            invocation_id: "success-inv".to_string(),
            event_class: "UserQuery".to_string(),
            route_id: "cheap-local".to_string(),
            started_at_ns: 1_000_000_000,
            ended_at_ns: 1_100_000_000,
            outcome: "success".to_string(),
            summary: "task completed".to_string(),
        };
        let fault_rec = EpisodeRecord {
            invocation_id: "fault-inv".to_string(),
            event_class: "OperatorCommand".to_string(),
            route_id: "frontier".to_string(),
            started_at_ns: 2_000_000_000,
            ended_at_ns: 2_200_000_000,
            outcome: "fault".to_string(),
            summary: "cortex crashed".to_string(),
        };

        EpisodeStore::archive(&mut archive, 1, &success_rec).expect("archive success_rec");
        EpisodeStore::archive(&mut archive, 2, &fault_rec).expect("archive fault_rec");

        // Query: similarity to the success embedding (dim-0 = 1.0 → "success").
        let success_emb = embed_episode(&success_rec);
        let query = EpisodeQuery::top_k(success_emb, 1);
        let results = EpisodeStore::retrieve(&archive, &query);

        assert_eq!(results.len(), 1, "exactly 1 result requested");
        assert_eq!(
            results[0].record.invocation_id, "success-inv",
            "top result must be the success episode"
        );
        assert!(results[0].score > 0.0, "score must be positive");

        let _ = std::fs::remove_file(&path);
    }

    /// Non-episode L3 entries are excluded from episodic retrieval.
    #[test]
    fn non_episode_l3_entries_excluded_from_episodic_retrieval() {
        let (mut archive, path) = temp_archive("exclusion");

        // Insert a somatic MemoryNode entry.
        let node = memory::MemoryNode::new(0.9, 0.1);
        let somatic_item = memory::archive_memory_node(1, "somatic-key", &node);
        let somatic_prov = memory::Provenance::now(SourceTier::L1, "somatic-key");
        archive
            .demote(somatic_item, somatic_prov)
            .expect("demote somatic");

        // Insert one episode entry.
        let rec = episode("ep-1", "success", "solo episode");
        EpisodeStore::archive(&mut archive, 2, &rec).expect("archive episode");

        // Retrieval must return only the episode.
        let query = EpisodeQuery::top_k(embed_episode(&rec), 5);
        let results = EpisodeStore::retrieve(&archive, &query);

        assert_eq!(results.len(), 1, "only the episode entry must be returned");
        assert_eq!(results[0].record.invocation_id, "ep-1");
        let _ = std::fs::remove_file(&path);
    }

    /// Recency cutoff excludes old episodes (those older than the cutoff window).
    #[test]
    fn recency_cutoff_excludes_old_episodes() {
        let (mut archive, path) = temp_archive("recency");

        // Construct an episode with `ended_at_ns` far in the past (1 ns from epoch).
        let old_rec = EpisodeRecord {
            invocation_id: "old-inv".to_string(),
            event_class: "UserQuery".to_string(),
            route_id: "cheap-local".to_string(),
            started_at_ns: 0,
            ended_at_ns: 1, // 1 ns after UNIX epoch — ancient
            outcome: "success".to_string(),
            summary: "ancient episode".to_string(),
        };

        // Build the "fresh" episode with ended_at_ns set to the current time so
        // it always passes a 1-hour recency cutoff regardless of when the test runs.
        let now = EpisodeRecord::now_ns();
        let fresh_rec = EpisodeRecord {
            invocation_id: "fresh-inv".to_string(),
            event_class: "UserQuery".to_string(),
            route_id: "cheap-local".to_string(),
            started_at_ns: now,
            ended_at_ns: now,
            outcome: "success".to_string(),
            summary: "recent episode".to_string(),
        };

        EpisodeStore::archive(&mut archive, 1, &old_rec).expect("archive old");
        EpisodeStore::archive(&mut archive, 2, &fresh_rec).expect("archive fresh");

        // Apply a 1-hour recency cutoff.
        let one_hour_ns: u64 = 3_600 * 1_000_000_000;
        // Use a neutral query embedding so both records score equally before recency.
        let query =
            EpisodeQuery::top_k(vec![0.5, 0.5, 0.5, 0.5], 5).with_recency_cutoff(one_hour_ns);
        let results = EpisodeStore::retrieve(&archive, &query);

        // The "old" episode ends at ns=1, far before `now - 1h`, so it must be excluded.
        // The "fresh" episode has ended_at_ns ≈ now, so it passes the cutoff.
        assert!(
            results.iter().all(|m| m.record.invocation_id != "old-inv"),
            "old episode must be excluded by recency cutoff"
        );
        assert!(
            results
                .iter()
                .any(|m| m.record.invocation_id == "fresh-inv"),
            "fresh episode must be included"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Top-k is respected when more episodes than k are stored.
    #[test]
    fn retrieval_respects_top_k_limit() {
        let (mut archive, path) = temp_archive("top_k");
        for i in 1..=10_u64 {
            let rec = episode(&format!("inv-{i}"), "success", &format!("episode {i}"));
            EpisodeStore::archive(&mut archive, i, &rec).expect("archive");
        }
        let query = EpisodeQuery::top_k(vec![1.0, 0.5, 0.8, 0.3], 3);
        let results = EpisodeStore::retrieve(&archive, &query);
        assert_eq!(results.len(), 3, "must return exactly k=3 results");
        let _ = std::fs::remove_file(&path);
    }

    /// Retrieval over an empty archive returns an empty vec.
    #[test]
    fn retrieval_over_empty_archive_returns_empty() {
        let (archive, path) = temp_archive("empty");
        let query = EpisodeQuery::top_k(vec![1.0, 0.5, 0.8, 0.3], 5);
        let results = EpisodeStore::retrieve(&archive, &query);
        assert!(results.is_empty(), "empty archive must return no results");
        let _ = std::fs::remove_file(&path);
    }

    /// `EpisodeStore::archive` is idempotent: archiving the same ID twice
    /// returns `AlreadyPresent` and does not duplicate entries.
    #[test]
    fn episode_archive_is_idempotent() {
        let (mut archive, path) = temp_archive("idempotent");
        let rec = episode("inv-idem", "success", "idempotent");
        EpisodeStore::archive(&mut archive, 1, &rec).expect("first archive");
        let second = EpisodeStore::archive(&mut archive, 1, &rec).expect("second archive");
        assert!(
            matches!(second, DemotionOutcome::AlreadyPresent),
            "second archive must be AlreadyPresent"
        );
        assert_eq!(
            archive.entries().len(),
            1,
            "only one entry must exist after idempotent archive"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// `EpisodeRecord::is_success` and `duration_secs` are correct.
    #[test]
    fn episode_record_helper_methods_are_correct() {
        let rec = EpisodeRecord {
            invocation_id: "inv-h".to_string(),
            event_class: "UserQuery".to_string(),
            route_id: "mid-tier".to_string(),
            started_at_ns: 1_000_000_000,
            ended_at_ns: 3_000_000_000,
            outcome: "success".to_string(),
            summary: "helpers".to_string(),
        };
        assert!(rec.is_success());
        let dur = rec.duration_secs();
        assert!((dur - 2.0).abs() < 1e-6, "duration must be 2.0 s");

        let fault = EpisodeRecord {
            outcome: "fault".to_string(),
            ..rec.clone()
        };
        assert!(!fault.is_success());
    }
}
