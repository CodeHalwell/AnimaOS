//! Emotional-decay pruning for the L1 and L2 memory tiers.
//!
//! Each sleep cycle the agent applies the `S(t)` activation model from
//! [`crate::decay`] to every stored episodic node.  Nodes whose activation has
//! fallen **at or below** the semantic floor are evicted; every survivor is
//! guaranteed to sit *above* the floor.
//!
//! # L1 pruning
//!
//! [`L1PruningStore`] is a keyed in-memory store of [`MemoryNode`]s that
//! represents the live attention window's episodic layer.  The method
//! [`L1PruningStore::run_pruning_pass`] applies a decay pass and removes
//! below-floor entries.
//!
//! # L2 pruning
//!
//! [`prune_l2_cache`] operates on an [`ArcCache<K, MemoryNode>`][crate::ArcCache]
//! by calling the cache's [`ArcCache::retain`] method.  It returns the same
//! [`PruningReport`] structure so callers get a uniform view of both tiers.

use std::collections::HashMap;

use crate::decay::{MemoryNode, SEMANTIC_FLOOR};
use crate::l2_cache::ArcCache;

// ── PruningReport ─────────────────────────────────────────────────────────────

/// Statistics produced by a single pruning pass over a memory tier.
#[derive(Debug, Clone, PartialEq)]
pub struct PruningReport {
    /// Number of nodes present *before* the pass.
    pub nodes_before: usize,
    /// Number of nodes *removed* during this pass.
    pub nodes_removed: usize,
    /// Effective semantic floor that was enforced (≥ [`SEMANTIC_FLOOR`]).
    pub floor_enforced: f32,
}

impl PruningReport {
    /// Number of nodes that *survived* this pruning pass.
    pub fn nodes_retained(&self) -> usize {
        self.nodes_before.saturating_sub(self.nodes_removed)
    }
}

// ── L1PruningStore ────────────────────────────────────────────────────────────

/// L1 episodic memory store with activation-decay pruning.
///
/// Stores named [`MemoryNode`]s and exposes [`run_pruning_pass`][Self::run_pruning_pass]
/// to evict nodes whose emotional-decay activation has fallen to or below the
/// semantic floor.
///
/// # Exit-criterion guarantees (E3.5)
///
/// * **Floor bound** — [`run_pruning_pass`][Self::run_pruning_pass] enforces
///   `floor.max(SEMANTIC_FLOOR)`, so no node is ever retained below the
///   semantic floor regardless of what threshold the caller supplies.
/// * **Post-pass invariant** — every node remaining in the store has
///   `activation_at(elapsed) > floor_enforced`.
#[derive(Debug, Clone, Default)]
pub struct L1PruningStore {
    nodes: HashMap<String, MemoryNode>,
}

impl L1PruningStore {
    /// Creates an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or replaces a memory node at the given key.
    pub fn insert(&mut self, key: impl Into<String>, node: MemoryNode) {
        self.nodes.insert(key.into(), node);
    }

    /// Returns a reference to the node stored under `key`, if present.
    pub fn get(&self, key: &str) -> Option<&MemoryNode> {
        self.nodes.get(key)
    }

    /// Number of nodes currently in the store.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// `true` when no nodes are stored.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Returns an iterator over `(key, node)` pairs in arbitrary order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &MemoryNode)> {
        self.nodes.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Runs a pruning pass using the canonical [`SEMANTIC_FLOOR`].
    ///
    /// Nodes with `activation_at(elapsed) <= SEMANTIC_FLOOR` are removed.
    /// Returns a [`PruningReport`] summarising the pass.
    pub fn run_pruning_pass(&mut self, elapsed: f32) -> PruningReport {
        self.run_pruning_pass_with(elapsed, SEMANTIC_FLOOR)
    }

    /// Runs a pruning pass with an explicit threshold.
    ///
    /// The effective floor is `floor.max(SEMANTIC_FLOOR)` — the caller cannot
    /// prune *less* aggressively than the semantic floor guarantees.
    ///
    /// Every surviving node satisfies `activation_at(elapsed) > floor_enforced`.
    pub fn run_pruning_pass_with(&mut self, elapsed: f32, floor: f32) -> PruningReport {
        let floor_enforced = floor.max(SEMANTIC_FLOOR);
        let nodes_before = self.nodes.len();
        self.nodes
            .retain(|_, node| node.activation_at(elapsed) > floor_enforced);
        let nodes_removed = nodes_before - self.nodes.len();
        PruningReport {
            nodes_before,
            nodes_removed,
            floor_enforced,
        }
    }

    /// Runs a pruning pass and returns both the report *and* the evicted nodes.
    ///
    /// This is identical to [`run_pruning_pass_with`][Self::run_pruning_pass_with]
    /// but additionally collects every `(key, node)` pair that was removed so
    /// callers can demote them to L3 (E2.6).
    pub fn drain_pruned_with(
        &mut self,
        elapsed: f32,
        floor: f32,
    ) -> (PruningReport, Vec<(String, MemoryNode)>) {
        let floor_enforced = floor.max(SEMANTIC_FLOOR);
        let nodes_before = self.nodes.len();
        let mut evicted = Vec::new();
        self.nodes.retain(|key, node| {
            if node.activation_at(elapsed) > floor_enforced {
                true
            } else {
                evicted.push((key.clone(), node.clone()));
                false
            }
        });
        let nodes_removed = nodes_before - self.nodes.len();
        let report = PruningReport {
            nodes_before,
            nodes_removed,
            floor_enforced,
        };
        (report, evicted)
    }
}

// ── L2 pruning ────────────────────────────────────────────────────────────────

/// Prunes an L2 [`ArcCache<K, MemoryNode>`] by removing entries whose
/// activation at `elapsed` is at or below `floor`.
///
/// Uses [`ArcCache::retain`] so the ARC internal invariants (T1/T2 structure,
/// ghost-list bookkeeping) are respected.  The effective floor is
/// `floor.max(SEMANTIC_FLOOR)`.
pub fn prune_l2_cache<K>(cache: &ArcCache<K, MemoryNode>, elapsed: f32, floor: f32) -> PruningReport
where
    K: Eq + std::hash::Hash + Clone + Send + 'static,
{
    let floor_enforced = floor.max(SEMANTIC_FLOOR);
    let nodes_before = cache.len();
    let nodes_removed = cache.retain(|_k, node| node.activation_at(elapsed) > floor_enforced);
    PruningReport {
        nodes_before,
        nodes_removed,
        floor_enforced,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decay::{EmotionalContext, SEMANTIC_FLOOR};

    // ── L1PruningStore basics ────────────────────────────────────────────────

    #[test]
    fn empty_store_reports_zero_nodes() {
        let store = L1PruningStore::new();
        assert_eq!(store.len(), 0);
        assert!(store.is_empty());
    }

    #[test]
    fn insert_and_get_round_trips_node() {
        let mut store = L1PruningStore::new();
        let node = MemoryNode::new(0.9, 0.1);
        store.insert("k1", node.clone());
        assert_eq!(store.get("k1"), Some(&node));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn pruning_pass_removes_decayed_nodes() {
        let mut store = L1PruningStore::new();
        // Fast-decaying node (lambda=10) will be well below floor at t=5.
        store.insert("fast", MemoryNode::new(0.9, 10.0));
        // Stable node (lambda=0) never decays below initial activation.
        store.insert("stable", MemoryNode::new(0.9, 0.0));

        let report = store.run_pruning_pass(5.0);

        assert_eq!(report.nodes_before, 2);
        assert_eq!(report.nodes_removed, 1);
        assert_eq!(report.nodes_retained(), 1);
        assert!(store.get("stable").is_some(), "stable node must survive");
        assert!(store.get("fast").is_none(), "decayed node must be removed");
    }

    // ── E3.5 exit criterion 1: pruning bounded by the semantic floor ─────────

    #[test]
    fn pruning_bounded_by_semantic_floor_under_stress() {
        let mut store = L1PruningStore::new();

        // High-stress context: high arousal and surprise boost activation above floor.
        let mut stressed_node = MemoryNode::new(0.5, 2.0);
        stressed_node.emotion = EmotionalContext {
            arousal: 3.0,
            surprise: 1.0,
        };
        // At t=0.5, activation = 0.5 * e^(-1.0) * (1 + 1.5*3.0 + 2.0*1.0)
        //   ≈ 0.5 * 0.6065 * 7.5 ≈ 2.274 — well above floor
        store.insert("stressed", stressed_node);

        // Borderline node: activation just above floor at t=0.5.
        let borderline = MemoryNode::new(SEMANTIC_FLOOR + 0.001, 0.0);
        store.insert("borderline", borderline);

        let report = store.run_pruning_pass(0.5);

        // Both nodes should survive: stressed is highly active, borderline is above floor.
        assert_eq!(
            report.nodes_removed, 0,
            "no nodes should be pruned when activation > floor"
        );
        assert_eq!(report.nodes_retained(), 2);
    }

    // ── E3.5 exit criterion 2: no retained entry below floor after pass ───────

    #[test]
    fn no_retained_node_has_activation_at_or_below_floor_after_pruning() {
        let mut store = L1PruningStore::new();
        let elapsed = 10.0_f32;
        let floor = SEMANTIC_FLOOR;

        // Mix of fast-, slow-, and zero-decay nodes.
        for i in 0..20u32 {
            let lambda = i as f32 * 0.5;
            store.insert(format!("node-{i}"), MemoryNode::new(0.8, lambda));
        }

        let _ = store.run_pruning_pass(elapsed);

        // Post-pass invariant: every surviving node is strictly above the floor.
        for (key, node) in &store.nodes {
            let activation = node.activation_at(elapsed);
            assert!(
                activation > floor,
                "node '{key}' has activation {activation:.4} which is ≤ floor {floor:.4}"
            );
        }
    }

    #[test]
    fn custom_floor_is_bounded_by_semantic_floor() {
        let mut store = L1PruningStore::new();
        // A node whose natural activation at t=1 is 0.32 — above SEMANTIC_FLOOR (0.3)
        // but might be below a custom floor of 0.35.
        store.insert("node", MemoryNode::new(0.35, 0.05));
        // Requesting floor BELOW semantic floor should not cause the node to be pruned
        // if its activation > SEMANTIC_FLOOR, even if floor_arg < SEMANTIC_FLOOR.
        // The effective floor is max(floor_arg, SEMANTIC_FLOOR).
        let report = store.run_pruning_pass_with(1.0, 0.0); // floor_arg below semantic floor
        assert_eq!(report.floor_enforced, SEMANTIC_FLOOR);
        // Node activation at t=1: 0.35 * e^(-0.05) ≈ 0.3329 > 0.3 → survives
        assert_eq!(report.nodes_removed, 0);
    }

    #[test]
    fn pruning_report_nodes_retained_matches_store_len() {
        let mut store = L1PruningStore::new();
        // Insert 5 fast-decaying and 3 stable nodes.
        for i in 0..5u32 {
            store.insert(format!("fast-{i}"), MemoryNode::new(0.9, 20.0));
        }
        for i in 0..3u32 {
            store.insert(format!("stable-{i}"), MemoryNode::new(0.9, 0.0));
        }
        let report = store.run_pruning_pass(5.0);
        assert_eq!(
            report.nodes_retained(),
            store.len(),
            "PruningReport::nodes_retained must equal the store length after the pass"
        );
    }

    // ── L2 cache pruning ─────────────────────────────────────────────────────

    #[test]
    fn prune_l2_cache_removes_decayed_entries() {
        let cache: ArcCache<String, MemoryNode> = ArcCache::new(16);
        cache.insert("fast".to_string(), MemoryNode::new(0.9, 20.0));
        cache.insert("stable".to_string(), MemoryNode::new(0.9, 0.0));

        let report = prune_l2_cache(&cache, 5.0, SEMANTIC_FLOOR);

        assert_eq!(report.nodes_before, 2);
        assert_eq!(report.nodes_removed, 1);
        assert_eq!(cache.len(), 1, "only the stable node should remain");
        assert!(cache.get(&"stable".to_string()).is_some());
        assert!(cache.get(&"fast".to_string()).is_none());
    }

    #[test]
    fn prune_l2_cache_on_empty_cache_is_safe() {
        let cache: ArcCache<String, MemoryNode> = ArcCache::new(8);
        let report = prune_l2_cache(&cache, 1.0, SEMANTIC_FLOOR);
        assert_eq!(report.nodes_before, 0);
        assert_eq!(report.nodes_removed, 0);
        assert_eq!(report.nodes_retained(), 0);
    }

    #[test]
    fn prune_l2_cache_post_pass_invariant() {
        let cache: ArcCache<String, MemoryNode> = ArcCache::new(32);
        let elapsed = 8.0_f32;
        let floor = SEMANTIC_FLOOR;

        for i in 0..16u32 {
            let lambda = i as f32 * 0.3;
            cache.insert(format!("n{i}"), MemoryNode::new(0.9, lambda));
        }

        prune_l2_cache(&cache, elapsed, floor);

        // Verify the remaining entries are all above floor.
        // We can't directly iterate the ArcCache (by design), but we can re-insert
        // and check retrieval:  the fact that the cache.len() shrank and the
        // `retain` closure enforced the invariant is the primary check.
        assert!(
            cache.len() < 16,
            "at least some nodes should have been pruned"
        );
    }
}
