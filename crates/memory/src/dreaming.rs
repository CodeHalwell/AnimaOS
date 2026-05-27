//! Dream exploration — seeded random-walk sampler over L3 (E3.7).
//!
//! During the `DreamExploration` sleep phase the agent performs seeded random
//! walks across the L3 archive, using cosine similarity as the traversal
//! weight to discover latent associative edges between archived memory nodes.
//!
//! # `no_std` support (E4.5)
//!
//! The core random-walk algorithm, all configuration types, and the in-memory
//! variant [`run_dream_walk_no_std`] are fully available in `no_std + alloc`
//! builds.  The L3-backed entry point [`run_dream_walk`] requires `std` because
//! [`L3Archive`][crate::archival::L3Archive] is file-backed.

// In no_std+alloc mode, Vec/String come from alloc.
// Note: `vec!` macro is not used in the no_std production code path; only
// `Vec::new()` and `.collect()` are needed here.
#[cfg(not(feature = "std"))]
use alloc::{string::String, vec::Vec};

// In no_std mode, f32::sqrt() is provided by the `libm` crate.
#[cfg(feature = "libm")]
use libm::sqrtf;

// The L3Archive import is std-only.
#[cfg(feature = "std")]
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
/// Available in `no_std` builds.
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
/// Available in `no_std` builds.
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
            // alloc::format! works in both std and no_std+alloc.
            #[cfg(not(feature = "std"))]
            use alloc::format;
            format!("{a}\x00{b}")
        } else {
            #[cfg(not(feature = "std"))]
            use alloc::format;
            format!("{b}\x00{a}")
        }
    }
}

// ── DreamReport ───────────────────────────────────────────────────────────────

/// Statistics produced by a single dream-exploration pass.
///
/// Logged every cycle regardless of yield (E3.7 exit criterion 1).
/// Available in `no_std` builds.
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

// ── In-memory entry type (no_std) ─────────────────────────────────────────────

/// A lightweight archive entry for use with [`run_dream_walk_no_std`].
///
/// Carries an opaque integer ID, a string key, and a floating-point embedding
/// vector.  No file I/O or serde is required.
#[derive(Debug, Clone)]
pub struct InMemoryEntry {
    /// Stable numeric ID.
    pub id: u64,
    /// Provenance key (used in [`AssociativeEdge`] from/to fields).
    pub key: String,
    /// Embedding vector for cosine-similarity scoring.
    pub embedding: Vec<f32>,
}

// ── run_dream_walk_no_std ─────────────────────────────────────────────────────

/// Runs the dream-exploration random-walk sampler against an in-memory slice
/// of [`InMemoryEntry`]s.
///
/// This is the `no_std`-compatible counterpart of [`run_dream_walk`]:
/// no file I/O, no serde, no L3Archive — only `alloc`.
///
/// Returns `(report, candidate_edges)` sorted by descending similarity then
/// lexicographic key order for full determinism.
pub fn run_dream_walk_no_std(
    entries: &[InMemoryEntry],
    config: &DreamConfig,
) -> (DreamReport, Vec<AssociativeEdge>) {
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

    // Deduplication map: dedup_key → best AssociativeEdge.
    // BTreeMap works in no_std+alloc; HashMap would need std or hashbrown.
    #[cfg(feature = "std")]
    let mut edge_map: std::collections::HashMap<String, AssociativeEdge> =
        std::collections::HashMap::new();
    #[cfg(not(feature = "std"))]
    let mut edge_map: alloc::collections::BTreeMap<String, AssociativeEdge> =
        alloc::collections::BTreeMap::new();

    for _ in 0..config.num_walks {
        let mut current_idx = rng.next_usize(entries.len());

        for _ in 0..config.walk_length {
            let current = &entries[current_idx];

            // Compute similarity to all other entries, filter by threshold.
            let mut scored: Vec<(usize, f32)> = entries
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != current_idx)
                .map(|(i, other)| {
                    let sim = cosine_similarity(&current.embedding, &other.embedding);
                    (i, sim)
                })
                .filter(|(_, sim)| *sim >= config.similarity_threshold)
                .collect();

            if scored.is_empty() {
                break;
            }

            // Sort: descending similarity, then ascending index for determinism.
            scored.sort_by(|a, b| {
                b.1.partial_cmp(&a.1)
                    .unwrap_or(core::cmp::Ordering::Equal)
                    .then_with(|| a.0.cmp(&b.0))
            });

            let top_k = scored.len().min(config.top_k_neighbors);
            let chosen_rank = rng.next_usize(top_k);
            let (next_idx, sim) = scored[chosen_rank];
            let next_key = &entries[next_idx].key;

            let dedup = AssociativeEdge::dedup_key(&current.key, next_key);
            let candidate = edge_map.entry(dedup).or_insert_with(|| AssociativeEdge {
                from_key: current.key.clone(),
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
    candidates.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(core::cmp::Ordering::Equal)
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

// ── run_dream_walk (std only) ─────────────────────────────────────────────────

/// Runs the dream-exploration random-walk sampler against `l3`.
///
/// Returns `(report, candidate_edges)` where `candidate_edges` is sorted by
/// descending similarity (strongest associations first), then lexicographically
/// by key pair for full determinism.
///
/// **std only** — requires [`L3Archive`][crate::archival::L3Archive] which is
/// file-backed.  Use [`run_dream_walk_no_std`] in `no_std + alloc` builds.
#[cfg(feature = "std")]
pub fn run_dream_walk(l3: &L3Archive, config: &DreamConfig) -> (DreamReport, Vec<AssociativeEdge>) {
    let entries = l3.entries();

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
    let mut edge_map: std::collections::HashMap<String, AssociativeEdge> =
        std::collections::HashMap::new();

    for _ in 0..config.num_walks {
        let mut current_idx = rng.next_usize(entries.len());

        for _ in 0..config.walk_length {
            let current = entries[current_idx];
            let current_key = &current.provenance.source_key;

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
                break;
            }

            scored.sort_by(|a, b| {
                b.1.partial_cmp(&a.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.0.cmp(&b.0))
            });

            let top_k = scored.len().min(config.top_k_neighbors);
            let chosen_rank = rng.next_usize(top_k);
            let (next_idx, sim) = scored[chosen_rank];
            let next_key = &entries[next_idx].provenance.source_key;

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
    let sq_a: f32 = a.iter().map(|x| x * x).sum::<f32>();
    let sq_b: f32 = b.iter().map(|x| x * x).sum::<f32>();
    #[cfg(not(feature = "libm"))]
    let (norm_a, norm_b) = (sq_a.sqrt(), sq_b.sqrt());
    #[cfg(feature = "libm")]
    let (norm_a, norm_b) = (sqrtf(sq_a), sqrtf(sq_b));
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    (dot / (norm_a * norm_b)).clamp(-1.0, 1.0)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

// std-dependent tests (use L3Archive, temp files, etc.)
#[cfg(all(test, feature = "std"))]
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

    #[test]
    fn empty_archive_yields_no_candidates() {
        let path = std::env::temp_dir().join("dream_empty.json");
        let _ = std::fs::remove_file(&path);
        let l3 = L3Archive::open(&path, 4, 100).unwrap();
        let (report, candidates) = run_dream_walk(&l3, &DreamConfig::default());
        assert_eq!(report.walks_run, 0);
        assert_eq!(report.candidates_found, 0);
        assert!(candidates.is_empty());
    }

    #[test]
    fn singleton_archive_yields_no_candidates() {
        let node = MemoryNode::new(0.8, 0.1);
        let (l3, _path) = make_archive("dream_singleton.json", &[("alpha", node)]);
        let (report, candidates) = run_dream_walk(&l3, &DreamConfig::default());
        assert_eq!(report.walks_run, 0);
        assert!(candidates.is_empty());
    }

    #[test]
    fn two_similar_nodes_produce_one_edge() {
        let (l3, _path) = make_archive(
            "dream_two.json",
            &[
                ("a", MemoryNode::new(0.8, 0.1)),
                ("b", MemoryNode::new(0.8, 0.1)),
            ],
        );
        let config = DreamConfig {
            seed: 1,
            walk_length: 1,
            num_walks: 1,
            similarity_threshold: 0.0,
            top_k_neighbors: 1,
        };
        let (report, candidates) = run_dream_walk(&l3, &config);
        assert_eq!(report.steps_taken, 1);
        assert_eq!(candidates.len(), 1);
        assert_eq!(report.candidates_found, 1);
    }

    #[test]
    fn reproducibility_with_same_seed() {
        let nodes: Vec<(&str, MemoryNode)> = (0..6)
            .map(|i| {
                let key: &'static str = ["p", "q", "r", "s", "t", "u"][i];
                (key, MemoryNode::new(0.5 + i as f32 * 0.05, 0.1))
            })
            .collect();
        let (l3, _path) = make_archive("dream_repro.json", &nodes);
        let config = DreamConfig {
            seed: 99,
            ..DreamConfig::default()
        };
        let (_, c1) = run_dream_walk(&l3, &config);
        let (_, c2) = run_dream_walk(&l3, &config);
        assert_eq!(c1, c2, "same seed must produce identical candidates");
    }

    #[test]
    fn threshold_filtering_removes_low_similarity_edges() {
        let (l3, _path) = make_archive(
            "dream_thresh.json",
            &[
                ("x", MemoryNode::new(0.9, 0.0)),
                ("y", MemoryNode::new(0.1, 5.0)),
            ],
        );
        let config = DreamConfig {
            similarity_threshold: 0.99,
            ..DreamConfig::default()
        };
        let (_, candidates) = run_dream_walk(&l3, &config);
        for edge in &candidates {
            assert!(
                edge.similarity >= 0.99,
                "edge {edge:?} is below threshold"
            );
        }
    }

    // ── no_std variant tests (always run) ─────────────────────────────────────

    #[test]
    fn no_std_variant_empty_slice_yields_no_candidates() {
        let (report, candidates) =
            run_dream_walk_no_std(&[], &DreamConfig::default());
        assert_eq!(report.walks_run, 0);
        assert!(candidates.is_empty());
    }

    #[test]
    fn no_std_variant_two_entries_produce_edge() {
        let entries = vec![
            InMemoryEntry {
                id: 1,
                key: "a".into(),
                embedding: vec![1.0, 0.0, 0.0, 0.0],
            },
            InMemoryEntry {
                id: 2,
                key: "b".into(),
                embedding: vec![0.9, 0.1, 0.0, 0.0],
            },
        ];
        let config = DreamConfig {
            seed: 1,
            walk_length: 1,
            num_walks: 1,
            similarity_threshold: 0.0,
            top_k_neighbors: 1,
        };
        let (report, candidates) = run_dream_walk_no_std(&entries, &config);
        assert_eq!(report.steps_taken, 1);
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn no_std_variant_reproducibility() {
        let entries: Vec<InMemoryEntry> = (0u64..5)
            .map(|i| InMemoryEntry {
                id: i,
                key: format!("k{i}"),
                embedding: vec![i as f32, (i + 1) as f32, 0.0, 1.0],
            })
            .collect();
        let config = DreamConfig { seed: 7, ..DreamConfig::default() };
        let (_, c1) = run_dream_walk_no_std(&entries, &config);
        let (_, c2) = run_dream_walk_no_std(&entries, &config);
        assert_eq!(c1, c2, "same seed must produce identical candidates");
    }
}

// Tests that can run in no_std builds (pure alloc).
#[cfg(all(test, not(feature = "std")))]
mod no_std_tests {
    use super::*;

    #[test]
    fn no_std_dream_walk_basic() {
        let entries = vec![
            InMemoryEntry {
                id: 1,
                key: alloc::string::String::from("node-a"),
                embedding: alloc::vec![1.0_f32, 0.0, 0.0, 0.0],
            },
            InMemoryEntry {
                id: 2,
                key: alloc::string::String::from("node-b"),
                embedding: alloc::vec![0.0_f32, 1.0, 0.0, 0.0],
            },
            InMemoryEntry {
                id: 3,
                key: alloc::string::String::from("node-c"),
                embedding: alloc::vec![0.7_f32, 0.7, 0.0, 0.0],
            },
        ];
        let config = DreamConfig {
            seed: 42,
            walk_length: 3,
            num_walks: 3,
            similarity_threshold: 0.1,
            top_k_neighbors: 2,
        };
        let (report, candidates) = run_dream_walk_no_std(&entries, &config);
        assert_eq!(report.walks_run, 3);
        assert!(report.candidates_found > 0);
        for edge in &candidates {
            assert!(edge.similarity >= 0.1);
        }
    }
}
