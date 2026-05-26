//! Dream exploration — seeded random-walk sampler over L3 (E3.7).
//!
//! During the `DreamExploration` sleep phase the agent performs seeded random
//! walks across the L3 archive, using cosine similarity as the traversal
//! weight to discover latent associative edges between archived memory nodes.
//!
//! # Algorithm
//!
//! For each walk:
//! 1. Select a start node uniformly at random (seeded).
//! 2. Compute cosine similarity between the current node and every other node.
//! 3. Filter neighbours below `similarity_threshold` (exit criterion 2).
//! 4. Pick one of the top-K similar neighbours using the seeded PRNG.
//! 5. Record an [`AssociativeEdge`] between the current and chosen node.
//! 6. Move to the chosen node and repeat for `walk_length` steps.
//!
//! After all walks complete the edge set is deduplicated (keeping the
//! highest similarity for duplicate pairs), sorted by descending similarity
//! then lexicographic key order for full determinism, and returned alongside
//! a [`DreamReport`].
//!
//! # Seeded determinism
//!
//! All random choices use a Xorshift64 PRNG seeded by [`DreamConfig::seed`].
//! For identical archive contents and config the same edges are always
//! produced — satisfying exit criterion 1 (*candidate yield is
//! monotonic-reproducible per seed*).
//!
//! # Exit criteria (E3.7)
//!
//! 1. Candidate yield is logged and monotonic-reproducible per seed.
//! 2. Bad candidates (similarity below threshold) are filtered out.

use crate::archival::L3Archive;

// ── Seeded PRNG ───────────────────────────────────────────────────────────────

/// Minimal Xorshift64 pseudo-random number generator.
///
/// Deterministic output for a fixed seed makes walks reproducible across
/// runs (E3.7 exit criterion 1).
struct Xorshift64(u64);

impl Xorshift64 {
    fn new(seed: u64) -> Self {
        // Xorshift requires a non-zero seed.
        Self(if seed == 0 { 1 } else { seed })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Returns a random index in `[0, n)`.
    fn next_usize(&mut self, n: usize) -> usize {
        if n <= 1 {
            return 0;
        }
        (self.next_u64() as usize) % n
    }
}

// ── DreamConfig ───────────────────────────────────────────────────────────────

/// Configuration for the dream-exploration random walk.
#[derive(Debug, Clone, PartialEq)]
pub struct DreamConfig {
    /// Seed for the Xorshift64 PRNG — guarantees reproducible walks.
    pub seed: u64,
    /// Number of steps per random walk.
    pub walk_length: usize,
    /// Number of independent walks to run.
    pub num_walks: usize,
    /// Minimum cosine similarity for an edge to be considered a candidate.
    ///
    /// Edges with similarity below this threshold are discarded (exit criterion 2).
    pub similarity_threshold: f32,
    /// Number of top-similar neighbours the PRNG selects from at each step.
    pub top_k_neighbors: usize,
}

impl Default for DreamConfig {
    fn default() -> Self {
        Self {
            seed: 42,
            walk_length: 4,
            num_walks: 8,
            similarity_threshold: 0.5,
            top_k_neighbors: 3,
        }
    }
}

// ── AssociativeEdge ───────────────────────────────────────────────────────────

/// A candidate associative edge discovered during a dream walk.
///
/// Edges are supplied to the next pruning cycle so that highly-associated
/// entries can survive future pruning rounds (E3.7 story S3.7.3).
#[derive(Debug, Clone, PartialEq)]
pub struct AssociativeEdge {
    /// Provenance key of the source memory node.
    pub from_key: String,
    /// Provenance key of the destination memory node (always ≠ `from_key`).
    pub to_key: String,
    /// Cosine similarity between the two nodes' embeddings.
    pub similarity: f32,
}

impl AssociativeEdge {
    /// Canonical deduplication key — order-independent pair of keys.
    fn dedup_key(a: &str, b: &str) -> String {
        if a <= b {
            format!("{a}\x00{b}")
        } else {
            format!("{b}\x00{a}")
        }
    }
}

// ── DreamReport ───────────────────────────────────────────────────────────────

/// Statistics produced by a single dream-exploration pass.
///
/// Logged every cycle regardless of yield (E3.7 exit criterion 1).
#[derive(Debug, Clone, PartialEq)]
pub struct DreamReport {
    /// Number of random walks executed.
    pub walks_run: usize,
    /// Total steps taken across all walks (≤ `walks_run × walk_length`).
    pub steps_taken: usize,
    /// Candidate edges after deduplication and threshold filtering.
    pub candidates_found: usize,
    /// PRNG seed used for this cycle.
    pub seed: u64,
    /// Similarity threshold that was applied.
    pub threshold: f32,
}

// ── run_dream_walk ────────────────────────────────────────────────────────────

/// Runs the dream-exploration random-walk sampler against `l3`.
///
/// Returns `(report, candidate_edges)` where `candidate_edges` is sorted by
/// descending similarity (strongest associations first), then lexicographically
/// by key pair for full determinism.
///
/// When the archive has fewer than 2 entries no walks are performed and an
/// empty candidate list is returned — the `DreamReport` is still populated
/// with `walks_run=0` so the cycle log remains consistent (exit criterion 1).
pub fn run_dream_walk(l3: &L3Archive, config: &DreamConfig) -> (DreamReport, Vec<AssociativeEdge>) {
    let entries = l3.entries(); // sorted ascending by id — deterministic

    if entries.len() < 2 {
        return (
            DreamReport {
                walks_run: 0,
                steps_taken: 0,
                candidates_found: 0,
                seed: config.seed,
                threshold: config.similarity_threshold,
            },
            Vec::new(),
        );
    }

    let mut rng = Xorshift64::new(config.seed);
    let mut steps_taken = 0usize;
    // Map from dedup key → best AssociativeEdge seen so far.
    let mut edge_map: std::collections::HashMap<String, AssociativeEdge> =
        std::collections::HashMap::new();

    for _ in 0..config.num_walks {
        let mut current_idx = rng.next_usize(entries.len());

        for _ in 0..config.walk_length {
            let current = entries[current_idx];
            let current_key = &current.provenance.source_key;

            // Compute similarity to all other entries, filter by threshold.
            let mut scored: Vec<(usize, f32)> = entries
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != current_idx)
                .map(|(i, other)| {
                    let sim = cosine_similarity(&current.item.embedding, &other.item.embedding);
                    (i, sim)
                })
                .filter(|(_, sim)| *sim >= config.similarity_threshold)
                .collect();

            if scored.is_empty() {
                // Dead end — no neighbours above threshold; terminate this walk.
                break;
            }

            // Sort by descending similarity, then ascending index for determinism.
            scored.sort_by(|a, b| {
                b.1.partial_cmp(&a.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.0.cmp(&b.0))
            });

            // Select from the top-K neighbours.
            let top_k = scored.len().min(config.top_k_neighbors);
            let chosen_rank = rng.next_usize(top_k);
            let (next_idx, sim) = scored[chosen_rank];
            let next_key = &entries[next_idx].provenance.source_key;

            // Record the edge, keeping the highest similarity for duplicates.
            let dedup = AssociativeEdge::dedup_key(current_key, next_key);
            let candidate = edge_map.entry(dedup).or_insert_with(|| AssociativeEdge {
                from_key: current_key.clone(),
                to_key: next_key.clone(),
                similarity: sim,
            });
            if sim > candidate.similarity {
                candidate.similarity = sim;
            }

            steps_taken += 1;
            current_idx = next_idx;
        }
    }

    let mut candidates: Vec<AssociativeEdge> = edge_map.into_values().collect();
    // Sort: descending similarity, then lexicographic from_key / to_key for full
    // determinism (same results on every call with equal archive + config).
    candidates.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.from_key.cmp(&b.from_key))
            .then_with(|| a.to_key.cmp(&b.to_key))
    });

    let report = DreamReport {
        walks_run: config.num_walks,
        steps_taken,
        candidates_found: candidates.len(),
        seed: config.seed,
        threshold: config.similarity_threshold,
    };

    (report, candidates)
}

// ── cosine_similarity (module-private) ────────────────────────────────────────

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    (dot / (norm_a * norm_b)).clamp(-1.0, 1.0)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archival::{archive_memory_node, L3Archive, Provenance, SourceTier};
    use crate::decay::MemoryNode;

    /// Helper: open a fresh temp archive and populate it with `n` nodes.
    fn make_archive(name: &str, nodes: &[(&str, MemoryNode)]) -> (L3Archive, std::path::PathBuf) {
        let path = std::env::temp_dir().join(name);
        let _ = std::fs::remove_file(&path);
        let mut l3 = L3Archive::open(&path, 4, 100).unwrap();
        for (idx, (key, node)) in nodes.iter().enumerate() {
            let item = archive_memory_node(idx as u64 + 1, key, node);
            let prov = Provenance::now(SourceTier::L1, key);
            l3.demote(item, prov).unwrap();
        }
        (l3, path)
    }

    // ── Baseline ─────────────────────────────────────────────────────────────

    #[test]
    fn empty_archive_yields_no_candidates() {
        let path = std::env::temp_dir().join("dream_empty.json");
        let _ = std::fs::remove_file(&path);
        let l3 = L3Archive::open(&path, 4, 100).unwrap();
        let (report, candidates) = run_dream_walk(&l3, &DreamConfig::default());
        assert_eq!(report.walks_run, 0);
        assert_eq!(report.candidates_found, 0);
        assert!(candidates.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn single_entry_archive_yields_no_candidates() {
        let node = MemoryNode::new(0.8, 0.5);
        let (l3, path) = make_archive("dream_single.json", &[("k1", node)]);
        let (report, candidates) = run_dream_walk(&l3, &DreamConfig::default());
        assert_eq!(report.walks_run, 0);
        assert_eq!(report.candidates_found, 0);
        assert!(candidates.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    // ── Exit criterion 1: reproducible per seed ───────────────────────────────

    /// E3.7 exit criterion 1: identical inputs + seed → identical output.
    #[test]
    fn candidate_yield_is_monotonic_reproducible_per_seed() {
        let nodes = vec![
            ("alpha", MemoryNode::new(0.9, 0.1)),
            ("beta", MemoryNode::new(0.8, 0.2)),
            ("gamma", MemoryNode::new(0.7, 0.3)),
            ("delta", MemoryNode::new(0.6, 0.4)),
        ];
        let (l3, path) = make_archive("dream_repro.json", &nodes);

        let config = DreamConfig {
            seed: 12345,
            ..Default::default()
        };

        let (report1, edges1) = run_dream_walk(&l3, &config);
        let (report2, edges2) = run_dream_walk(&l3, &config);

        assert_eq!(
            report1, report2,
            "reports must be identical for the same seed"
        );
        assert_eq!(edges1, edges2, "edges must be identical for the same seed");

        let _ = std::fs::remove_file(&path);
    }

    /// Different seeds produce different (or coincidentally equal) walks but the
    /// function must not panic and must be deterministic per seed.
    #[test]
    fn different_seeds_are_both_deterministic() {
        let nodes = vec![
            ("a", MemoryNode::new(0.9, 0.1)),
            ("b", MemoryNode::new(0.8, 0.2)),
            ("c", MemoryNode::new(0.7, 0.3)),
        ];
        let (l3, path) = make_archive("dream_seeds.json", &nodes);

        let cfg_a = DreamConfig {
            seed: 1,
            ..Default::default()
        };
        let cfg_b = DreamConfig {
            seed: 2,
            ..Default::default()
        };

        let (_, edges_a1) = run_dream_walk(&l3, &cfg_a);
        let (_, edges_a2) = run_dream_walk(&l3, &cfg_a);
        assert_eq!(edges_a1, edges_a2, "seed=1 must be reproducible");

        let (_, edges_b1) = run_dream_walk(&l3, &cfg_b);
        let (_, edges_b2) = run_dream_walk(&l3, &cfg_b);
        assert_eq!(edges_b1, edges_b2, "seed=2 must be reproducible");

        let _ = std::fs::remove_file(&path);
    }

    // ── Exit criterion 2: threshold filtering ─────────────────────────────────

    /// E3.7 exit criterion 2: edges below the similarity threshold are excluded.
    #[test]
    fn threshold_filters_out_low_similarity_candidates() {
        // Two nodes with zero embeddings → cosine_similarity = 0.0 (undefined).
        // Any positive threshold should filter them out.
        let path = std::env::temp_dir().join("dream_thresh.json");
        let _ = std::fs::remove_file(&path);
        let mut l3 = L3Archive::open(&path, 4, 100).unwrap();

        // Insert two nodes with orthogonal embeddings (dot product = 0).
        use crate::archival::ArchivedItem;
        let item1 = ArchivedItem {
            id: 1,
            embedding: vec![1.0, 0.0, 0.0, 0.0],
            payload: vec![],
        };
        let item2 = ArchivedItem {
            id: 2,
            embedding: vec![0.0, 1.0, 0.0, 0.0],
            payload: vec![],
        };
        let p1 = Provenance::now(SourceTier::L1, "orth-a");
        let p2 = Provenance::now(SourceTier::L1, "orth-b");
        l3.demote(item1, p1).unwrap();
        l3.demote(item2, p2).unwrap();

        // High threshold (0.9) → orthogonal nodes (similarity 0.0) are filtered.
        let cfg = DreamConfig {
            similarity_threshold: 0.9,
            num_walks: 4,
            walk_length: 4,
            ..Default::default()
        };
        let (report, candidates) = run_dream_walk(&l3, &cfg);
        assert_eq!(
            report.candidates_found, 0,
            "orthogonal nodes must be filtered by high threshold"
        );
        assert!(candidates.is_empty());

        // Low threshold (0.0) → same nodes accepted (similarity = 0.0 >= 0.0).
        let cfg_low = DreamConfig {
            similarity_threshold: 0.0,
            num_walks: 4,
            walk_length: 4,
            ..Default::default()
        };
        let (report_low, candidates_low) = run_dream_walk(&l3, &cfg_low);
        // With threshold 0.0, similarity 0.0 passes the filter (>= 0.0).
        assert_eq!(report_low.candidates_found, candidates_low.len());

        let _ = std::fs::remove_file(&path);
    }

    // ── Edge properties ───────────────────────────────────────────────────────

    #[test]
    fn all_candidate_edges_have_similarity_at_or_above_threshold() {
        let nodes = vec![
            ("n0", MemoryNode::new(0.9, 0.1)),
            ("n1", MemoryNode::new(0.85, 0.15)),
            ("n2", MemoryNode::new(0.8, 0.2)),
            ("n3", MemoryNode::new(0.6, 0.5)),
        ];
        let (l3, path) = make_archive("dream_simcheck.json", &nodes);

        let cfg = DreamConfig {
            similarity_threshold: 0.5,
            num_walks: 10,
            walk_length: 6,
            seed: 999,
            ..Default::default()
        };
        let (report, candidates) = run_dream_walk(&l3, &cfg);

        for edge in &candidates {
            assert!(
                edge.similarity >= cfg.similarity_threshold,
                "edge ({} → {}) has similarity {:.4} < threshold {:.4}",
                edge.from_key,
                edge.to_key,
                edge.similarity,
                cfg.similarity_threshold
            );
            assert_ne!(
                edge.from_key, edge.to_key,
                "self-loops must not be recorded"
            );
        }
        assert_eq!(report.candidates_found, candidates.len());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn dream_report_is_logged_even_when_no_candidates_found() {
        let path = std::env::temp_dir().join("dream_zero_report.json");
        let _ = std::fs::remove_file(&path);
        let l3 = L3Archive::open(&path, 4, 100).unwrap();
        let (report, _) = run_dream_walk(&l3, &DreamConfig::default());
        // Report is always populated (walks_run, seed, threshold).
        assert_eq!(report.seed, DreamConfig::default().seed);
        assert_eq!(
            report.threshold,
            DreamConfig::default().similarity_threshold
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn candidate_edges_are_sorted_by_descending_similarity() {
        let nodes = vec![
            ("x", MemoryNode::new(0.95, 0.05)),
            ("y", MemoryNode::new(0.9, 0.1)),
            ("z", MemoryNode::new(0.85, 0.15)),
            ("w", MemoryNode::new(0.8, 0.2)),
        ];
        let (l3, path) = make_archive("dream_sorted.json", &nodes);
        let cfg = DreamConfig {
            similarity_threshold: 0.0,
            num_walks: 20,
            walk_length: 6,
            seed: 7,
            ..Default::default()
        };
        let (_, candidates) = run_dream_walk(&l3, &cfg);
        let sims: Vec<f32> = candidates.iter().map(|e| e.similarity).collect();
        let mut sorted = sims.clone();
        sorted.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        assert_eq!(
            sims, sorted,
            "edges must be sorted by descending similarity"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Xorshift64 PRNG produces different values for the same seed across
    /// successive calls — verifies the PRNG is not stuck.
    #[test]
    fn xorshift64_is_not_constant() {
        let mut rng = Xorshift64::new(1);
        let first = rng.next_u64();
        let second = rng.next_u64();
        assert_ne!(first, second, "PRNG must advance each call");
    }

    /// Seed=0 is handled without panic.
    #[test]
    fn xorshift64_zero_seed_does_not_panic() {
        let mut rng = Xorshift64::new(0);
        let _ = rng.next_u64(); // must not panic
    }
}
