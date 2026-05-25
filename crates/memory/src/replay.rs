//! Generative replay validation against the L3 archive (E3.6).
//!
//! During the `GenerativeReplay` sleep phase the agent queries the L3 archive
//! using the stored embeddings of recently archived entries.  If the k=1
//! retrieval returns the expected entry (matched by ID) the entry is counted as
//! *validated*.  When the fraction of validated entries falls below the
//! configured accuracy threshold the poorly-retrievable entries are marked for
//! *rollback* — re-insertion into the L1 pruning store so that the pruning
//! decision can be revisited in the next cycle.
//!
//! # Exit criteria (E3.6)
//!
//! 1. A soak test demonstrates at least one rollback (the rollback path is
//!    exercised).
//! 2. Validation accuracy is logged in the [`ReplayReport`] returned from
//!    every cycle (populated even when accuracy is perfect).

use crate::archival::L3Archive;
use crate::decay::MemoryNode;

// ── ReplayConfig ──────────────────────────────────────────────────────────────

/// Configuration for the generative replay validator.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayConfig {
    /// Minimum acceptable fraction of queries that return the correct match.
    ///
    /// When actual accuracy falls strictly below this threshold and
    /// [`rollback_enabled`] is `true`, rollback is triggered.
    pub accuracy_threshold: f32,
    /// Maximum number of L3 entries to sample per cycle (capped at archive size).
    pub max_sample_size: usize,
    /// When `true`, entries that fail their retrieval check are returned for
    /// re-insertion into the L1 pruning store.
    pub rollback_enabled: bool,
}

impl Default for ReplayConfig {
    fn default() -> Self {
        Self {
            accuracy_threshold: 0.8,
            max_sample_size: 16,
            rollback_enabled: true,
        }
    }
}

// ── ReplayReport ──────────────────────────────────────────────────────────────

/// Statistics produced by a single generative-replay validation pass.
///
/// This is emitted for **every** cycle (E3.6 exit criterion 2).
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayReport {
    /// Total number of similarity queries issued.
    pub queries_run: usize,
    /// Queries whose top-1 result matched the expected entry by ID.
    pub queries_validated: usize,
    /// `queries_validated / queries_run`, or `1.0` when `queries_run == 0`.
    pub accuracy: f32,
    /// Number of nodes returned for rollback into the L1 store.
    pub rolled_back: usize,
    /// Accuracy threshold that was applied during this cycle.
    pub threshold: f32,
    /// `true` when accuracy fell below the threshold and rollback was triggered.
    pub triggered_rollback: bool,
}

impl ReplayReport {
    /// Returns `true` when rollback was triggered this cycle.
    pub fn rollback_was_triggered(&self) -> bool {
        self.triggered_rollback
    }
}

// ── run_replay_validation ─────────────────────────────────────────────────────

/// Runs a generative-replay validation pass against `l3`.
///
/// For each sampled entry the method uses the entry's own embedding as the
/// query vector and checks whether `l3.search(query, 1)` returns the same
/// entry by ID.  When retrieval accuracy falls strictly below
/// [`ReplayConfig::accuracy_threshold`] and rollback is enabled, the entries
/// that failed retrieval are decoded back into [`MemoryNode`]s and returned
/// as the rollback list.
///
/// Entries are sampled in ascending ID order (deterministic) up to
/// [`ReplayConfig::max_sample_size`].
///
/// # Payload layout
///
/// The archived payload is expected to be the 20-byte encoding produced by
/// [`crate::archival::archive_memory_node`]:
/// `[initial_activation, lambda, alpha, arousal, surprise]` as five
/// little-endian `f32` values.
///
/// # Return value
///
/// `(report, rollback_nodes)` — `rollback_nodes` is empty when rollback was
/// not triggered or when no entries failed retrieval.
pub fn run_replay_validation(
    l3: &L3Archive,
    config: &ReplayConfig,
) -> (ReplayReport, Vec<(String, MemoryNode)>) {
    // Sample entries in deterministic (ascending ID) order.
    let entries: Vec<_> = l3
        .entries()
        .into_iter()
        .take(config.max_sample_size)
        .collect();
    let queries_run = entries.len();

    if queries_run == 0 {
        return (
            ReplayReport {
                queries_run: 0,
                queries_validated: 0,
                accuracy: 1.0,
                rolled_back: 0,
                threshold: config.accuracy_threshold,
                triggered_rollback: false,
            },
            Vec::new(),
        );
    }

    let mut validated: usize = 0;
    let mut failed_entries: Vec<(String, MemoryNode)> = Vec::new();

    for entry in &entries {
        let results = l3.search(&entry.item.embedding, 1);
        let matched = results
            .first()
            .map(|r| r.item.id == entry.item.id)
            .unwrap_or(false);

        if matched {
            validated += 1;
        } else if config.rollback_enabled {
            if let Some(node) = decode_node_from_payload(&entry.item.payload) {
                failed_entries.push((entry.provenance.source_key.clone(), node));
            }
        }
    }

    let accuracy = validated as f32 / queries_run as f32;
    let triggered_rollback = config.rollback_enabled && accuracy < config.accuracy_threshold;
    let rolled_back = if triggered_rollback {
        failed_entries.len()
    } else {
        0
    };

    let rollback_nodes = if triggered_rollback {
        failed_entries
    } else {
        Vec::new()
    };

    (
        ReplayReport {
            queries_run,
            queries_validated: validated,
            accuracy,
            rolled_back,
            threshold: config.accuracy_threshold,
            triggered_rollback,
        },
        rollback_nodes,
    )
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Decodes a [`MemoryNode`] from the 20-byte payload format produced by
/// [`crate::archival::archive_memory_node`].
///
/// Payload layout: `[initial_activation, lambda, alpha, arousal, surprise]`
/// as five little-endian `f32` values (20 bytes total).
///
/// Returns `None` when the payload is too short or malformed.
fn decode_node_from_payload(payload: &[u8]) -> Option<MemoryNode> {
    if payload.len() < 20 {
        return None;
    }
    let read_f32 = |offset: usize| -> f32 {
        let bytes: [u8; 4] = payload[offset..offset + 4]
            .try_into()
            .expect("slice is 4 bytes");
        f32::from_le_bytes(bytes)
    };
    let initial_activation = read_f32(0);
    let lambda = read_f32(4);
    let alpha = read_f32(8);
    let arousal = read_f32(12);
    let surprise = read_f32(16);

    let mut node = MemoryNode::new(initial_activation, lambda);
    node.alpha = alpha;
    node.emotion.arousal = arousal;
    node.emotion.surprise = surprise;
    Some(node)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archival::{archive_memory_node, L3Archive, Provenance, SourceTier};
    use std::path::Path;

    fn tmp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(name)
    }

    fn open_l3(name: &str) -> L3Archive {
        let path = tmp_path(name);
        let _ = std::fs::remove_file(&path);
        L3Archive::open(&path, 4, 100).unwrap()
    }

    fn cleanup(name: &str) {
        let _ = std::fs::remove_file(tmp_path(name));
    }

    fn add_node(l3: &mut L3Archive, id: u64, key: &str, node: &MemoryNode) {
        let item = archive_memory_node(id, key, node);
        let prov = Provenance::now(SourceTier::L1, key);
        l3.demote(item, prov).unwrap();
    }

    // ── Empty archive ────────────────────────────────────────────────────────

    #[test]
    fn empty_archive_returns_perfect_accuracy_and_no_rollback() {
        let l3 = open_l3("replay_empty.json");
        let config = ReplayConfig::default();
        let (report, rollback) = run_replay_validation(&l3, &config);

        assert_eq!(report.queries_run, 0);
        assert_eq!(report.queries_validated, 0);
        assert!(
            (report.accuracy - 1.0).abs() < f32::EPSILON,
            "empty archive → accuracy 1.0"
        );
        assert!(!report.triggered_rollback);
        assert!(rollback.is_empty());

        cleanup("replay_empty.json");
    }

    // ── Perfect accuracy ─────────────────────────────────────────────────────

    #[test]
    fn perfect_accuracy_when_all_embeddings_are_unique() {
        let mut l3 = open_l3("replay_perfect.json");
        // Orthogonal embeddings — each query uniquely retrieves its own entry.
        let n1 = MemoryNode::new(0.9, 0.1);
        let mut n2 = MemoryNode::new(0.1, 0.9);
        n2.emotion.arousal = 5.0;
        let mut n3 = MemoryNode::new(0.5, 0.5);
        n3.emotion.surprise = 5.0;

        add_node(&mut l3, 1, "k1", &n1);
        add_node(&mut l3, 2, "k2", &n2);
        add_node(&mut l3, 3, "k3", &n3);

        let config = ReplayConfig::default();
        let (report, rollback) = run_replay_validation(&l3, &config);

        assert_eq!(report.queries_run, 3);
        assert_eq!(report.queries_validated, 3);
        assert!((report.accuracy - 1.0).abs() < f32::EPSILON);
        assert!(!report.triggered_rollback);
        assert!(rollback.is_empty(), "no rollback when accuracy is perfect");

        cleanup("replay_perfect.json");
    }

    // ── Rollback triggered by duplicate embeddings ────────────────────────────

    /// E3.6 exit criterion 1: soak test demonstrates at least one rollback.
    ///
    /// Three entries share the same embedding.  The `search(q, 1)` call always
    /// returns ID=1 (lowest ID wins ties).  Queries for IDs 2 and 3 fail →
    /// accuracy = 1/3 < threshold 0.5 → rollback triggered.
    #[test]
    fn rollback_triggered_when_duplicate_embeddings_cause_low_accuracy() {
        let mut l3 = open_l3("replay_rollback.json");
        // Three nodes with identical embeddings.
        let node = MemoryNode::new(0.9, 0.1);
        add_node(&mut l3, 1, "key-1", &node);
        add_node(&mut l3, 2, "key-2", &node);
        add_node(&mut l3, 3, "key-3", &node);

        let config = ReplayConfig {
            accuracy_threshold: 0.5,
            max_sample_size: 16,
            rollback_enabled: true,
        };
        let (report, rollback) = run_replay_validation(&l3, &config);

        assert_eq!(report.queries_run, 3);
        assert_eq!(
            report.queries_validated, 1,
            "only ID=1 retrieves itself (tie broken by ascending ID)"
        );
        assert!((report.accuracy - 1.0 / 3.0).abs() < 1e-5);
        assert!(
            report.triggered_rollback,
            "rollback must be triggered when accuracy < threshold"
        );
        assert_eq!(
            report.rolled_back, 2,
            "two failed entries should be rolled back"
        );
        assert_eq!(rollback.len(), 2);

        // Rolled-back nodes must carry the correct source keys.
        let keys: std::collections::HashSet<&str> =
            rollback.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains("key-2"));
        assert!(keys.contains("key-3"));

        cleanup("replay_rollback.json");
    }

    /// Rollback disabled: even when accuracy is low, no nodes are returned.
    #[test]
    fn rollback_disabled_returns_no_nodes_even_when_accuracy_is_low() {
        let mut l3 = open_l3("replay_no_rollback.json");
        let node = MemoryNode::new(0.9, 0.1);
        add_node(&mut l3, 1, "key-1", &node);
        add_node(&mut l3, 2, "key-2", &node);

        let config = ReplayConfig {
            accuracy_threshold: 0.9, // would trigger if rollback_enabled
            max_sample_size: 16,
            rollback_enabled: false, // disabled
        };
        let (report, rollback) = run_replay_validation(&l3, &config);

        assert!(
            !report.triggered_rollback,
            "rollback must not trigger when disabled"
        );
        assert!(
            rollback.is_empty(),
            "no nodes returned when rollback is disabled"
        );

        cleanup("replay_no_rollback.json");
    }

    /// Accuracy exactly at the threshold does NOT trigger rollback (strict <).
    #[test]
    fn accuracy_at_threshold_does_not_trigger_rollback() {
        let mut l3 = open_l3("replay_at_threshold.json");
        // Two entries with different embeddings → 2/2 = 1.0 accuracy.
        let n1 = MemoryNode::new(0.9, 0.1);
        let mut n2 = MemoryNode::new(0.1, 0.9);
        n2.emotion.arousal = 5.0;
        add_node(&mut l3, 1, "k1", &n1);
        add_node(&mut l3, 2, "k2", &n2);

        let config = ReplayConfig {
            accuracy_threshold: 1.0, // exactly at this threshold (accuracy == threshold)
            max_sample_size: 16,
            rollback_enabled: true,
        };
        let (report, rollback) = run_replay_validation(&l3, &config);

        // accuracy == 1.0 == threshold → NOT strictly less than → no rollback.
        assert!(
            !report.triggered_rollback,
            "accuracy == threshold must not trigger rollback"
        );
        assert!(rollback.is_empty());

        cleanup("replay_at_threshold.json");
    }

    /// max_sample_size is respected — only the first N entries (by ascending ID)
    /// are sampled.
    #[test]
    fn max_sample_size_limits_queries_to_configured_count() {
        let mut l3 = open_l3("replay_sample_size.json");
        let node = MemoryNode::new(0.5, 0.5);
        for i in 1..=10 {
            add_node(&mut l3, i, &format!("key-{i}"), &node);
        }

        let config = ReplayConfig {
            accuracy_threshold: 0.8,
            max_sample_size: 4, // only 4 of the 10 entries
            rollback_enabled: false,
        };
        let (report, _) = run_replay_validation(&l3, &config);

        assert_eq!(report.queries_run, 4, "must respect max_sample_size");

        cleanup("replay_sample_size.json");
    }

    /// E3.6 exit criterion 2: accuracy is logged even when perfect.
    #[test]
    fn accuracy_is_logged_for_every_cycle_even_when_perfect() {
        let mut l3 = open_l3("replay_log_accuracy.json");
        // Two uniquely-embedded nodes → perfect accuracy.
        let n1 = MemoryNode::new(0.9, 0.0);
        let mut n2 = MemoryNode::new(0.1, 0.0);
        n2.emotion.arousal = 10.0;
        add_node(&mut l3, 1, "k1", &n1);
        add_node(&mut l3, 2, "k2", &n2);

        let config = ReplayConfig::default();

        // Run twice — both should report.
        let (r1, _) = run_replay_validation(&l3, &config);
        let (r2, _) = run_replay_validation(&l3, &config);

        assert_eq!(r1.queries_run, 2, "first cycle must log queries");
        assert_eq!(r2.queries_run, 2, "second cycle must log queries");
        assert!((r1.accuracy - 1.0).abs() < f32::EPSILON);
        assert!((r2.accuracy - 1.0).abs() < f32::EPSILON);

        cleanup("replay_log_accuracy.json");
    }

    /// decode_node_from_payload round-trips correctly.
    #[test]
    fn decode_node_from_payload_round_trips() {
        let mut node = MemoryNode::new(0.7, 0.3);
        node.alpha = 1.5;
        node.emotion.arousal = 2.0;
        node.emotion.surprise = 0.5;

        let item = archive_memory_node(1, "test-key", &node);
        let decoded = decode_node_from_payload(&item.payload).expect("payload must decode");

        assert!((decoded.initial_activation - node.initial_activation).abs() < f32::EPSILON);
        assert!((decoded.lambda - node.lambda).abs() < f32::EPSILON);
        assert!((decoded.alpha - node.alpha).abs() < f32::EPSILON);
        assert!((decoded.emotion.arousal - node.emotion.arousal).abs() < f32::EPSILON);
        assert!((decoded.emotion.surprise - node.emotion.surprise).abs() < f32::EPSILON);
    }

    /// decode_node_from_payload returns None for a short payload.
    #[test]
    fn decode_node_from_payload_returns_none_for_short_payload() {
        assert!(decode_node_from_payload(&[0u8; 10]).is_none());
        assert!(decode_node_from_payload(&[]).is_none());
    }
}
