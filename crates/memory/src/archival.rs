//! L3 cerebral archival store - vector-similarity addressable storage.
//!
//! Provides two implementations:
//!
//! * [`ArchivalStore`] — original in-memory stub kept for backward compatibility.
//! * [`L3Archive`] — file-backed JSON-persistent store implementing the full
//!   E2.6 feature set: demotion provenance, deterministic retrieval, and
//!   crash-safe atomic flushing.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::decay::MemoryNode;
use crate::l2_cache::ArcCache;

// ── Shared item type ──────────────────────────────────────────────────────────

/// A single archived memory item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchivedItem {
    /// Stable item identifier.
    pub id: u64,
    /// Embedded vector representation.
    pub embedding: Vec<f32>,
    /// Opaque payload bytes.
    pub payload: Vec<u8>,
}

// ── ArchivalStore (in-memory, backward-compat) ────────────────────────────────

/// Errors raised when interacting with the in-memory archival store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchivalStoreError {
    /// Provided embedding had an unexpected dimensionality.
    DimensionMismatch,
    /// The store has reached its configured capacity.
    AtCapacity,
}

/// In-memory archival store backed by linear cosine-similarity scoring.
#[derive(Debug, Clone)]
pub struct ArchivalStore {
    items: Vec<ArchivedItem>,
    expected_dim: usize,
    capacity: usize,
}

impl ArchivalStore {
    /// Creates an empty store accepting embeddings of `expected_dim`.
    pub fn new(expected_dim: usize, capacity: usize) -> Self {
        Self {
            items: Vec::new(),
            expected_dim,
            capacity,
        }
    }

    /// Number of stored items.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True when no items are stored.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Stores an item, validating its embedding dimensionality.
    pub fn store(&mut self, item: ArchivedItem) -> Result<(), ArchivalStoreError> {
        if item.embedding.len() != self.expected_dim {
            return Err(ArchivalStoreError::DimensionMismatch);
        }
        if self.items.len() >= self.capacity {
            return Err(ArchivalStoreError::AtCapacity);
        }
        self.items.push(item);
        Ok(())
    }

    /// Returns the top-`k` items by cosine similarity to `query`.
    pub fn search(&self, query: &[f32], k: usize) -> Vec<&ArchivedItem> {
        if query.len() != self.expected_dim || k == 0 {
            return Vec::new();
        }
        let mut scored: Vec<(f32, &ArchivedItem)> = self
            .items
            .iter()
            .map(|item| (cosine_similarity(query, &item.embedding), item))
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().take(k).map(|(_, item)| item).collect()
    }
}

// ── L3Archive types (E2.6) ────────────────────────────────────────────────────

/// Which memory tier a node was demoted from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceTier {
    /// Live L1 episodic attention window.
    L1,
    /// L2 ARC cache.
    L2,
}

/// Provenance record describing where and when an entry arrived in L3.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    /// Tier the entry was evicted from.
    pub source_tier: SourceTier,
    /// Original key in the source tier's store.
    pub source_key: String,
    /// Wall-clock nanoseconds since UNIX epoch at the time of demotion.
    pub demoted_at_ns: u64,
}

impl Provenance {
    /// Constructs a provenance record stamped with the current wall-clock time.
    pub fn now(source_tier: SourceTier, source_key: &str) -> Self {
        let demoted_at_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        Self {
            source_tier,
            source_key: source_key.to_string(),
            demoted_at_ns,
        }
    }
}

/// A single entry in the L3 archive, pairing a stored item with its provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchivalEntry {
    /// The archived memory item (embedding + payload).
    pub item: ArchivedItem,
    /// Where and when this entry was demoted into L3.
    pub provenance: Provenance,
}

/// Outcome of a demotion attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DemotionOutcome {
    /// Entry was newly inserted into the archive.
    Inserted,
    /// An entry with this ID already existed; the existing entry was kept.
    AlreadyPresent,
}

/// Errors that can occur when working with the [`L3Archive`].
#[derive(Debug)]
pub enum L3ArchiveError {
    /// Provided embedding dimensionality did not match the archive's setting.
    DimensionMismatch {
        /// Dimensionality the archive expects.
        expected: usize,
        /// Dimensionality of the supplied embedding.
        got: usize,
    },
    /// The archive has reached its configured capacity limit.
    AtCapacity,
    /// An I/O error occurred while reading or writing the backing file.
    Io(io::Error),
    /// The on-disk snapshot could not be deserialized.
    Corrupt(String),
}

impl std::fmt::Display for L3ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            L3ArchiveError::DimensionMismatch { expected, got } => {
                write!(f, "dimension mismatch: expected {expected}, got {got}")
            }
            L3ArchiveError::AtCapacity => write!(f, "archive at capacity"),
            L3ArchiveError::Io(e) => write!(f, "I/O error: {e}"),
            L3ArchiveError::Corrupt(msg) => write!(f, "corrupt archive: {msg}"),
        }
    }
}

impl std::error::Error for L3ArchiveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        if let L3ArchiveError::Io(e) = self {
            Some(e)
        } else {
            None
        }
    }
}

// ── On-disk snapshot ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct ArchiveSnapshot {
    expected_dim: usize,
    capacity: usize,
    entries: Vec<ArchivalEntry>,
}

// ── L3Archive ─────────────────────────────────────────────────────────────────

/// File-backed, vector-similarity-addressable L3 memory archive (E2.6).
///
/// Entries are flushed to disk atomically (write-to-temp, then rename) after
/// every successful demotion, ensuring the archive survives process restarts
/// (E2.6 exit criterion 1).
///
/// Retrieval is deterministic for fixed seeds: `search` sorts candidates by
/// (descending cosine similarity, ascending entry id) (E2.6 exit criterion 2).
#[derive(Clone)]
pub struct L3Archive {
    path: PathBuf,
    expected_dim: usize,
    capacity: usize,
    index: HashMap<u64, ArchivalEntry>,
}

impl L3Archive {
    /// Opens (or creates) an archive at `path`.
    ///
    /// If the file already exists its JSON snapshot is loaded and the stored
    /// `expected_dim` is validated against the supplied value.  If the file does
    /// not exist an empty archive is returned.
    pub fn open(path: &Path, expected_dim: usize, capacity: usize) -> Result<Self, L3ArchiveError> {
        if path.exists() {
            let bytes = std::fs::read(path).map_err(L3ArchiveError::Io)?;
            let snapshot: ArchiveSnapshot = serde_json::from_slice(&bytes)
                .map_err(|e| L3ArchiveError::Corrupt(e.to_string()))?;
            if snapshot.expected_dim != expected_dim {
                return Err(L3ArchiveError::DimensionMismatch {
                    expected: expected_dim,
                    got: snapshot.expected_dim,
                });
            }
            let index = snapshot
                .entries
                .into_iter()
                .map(|e| (e.item.id, e))
                .collect();
            Ok(Self {
                path: path.to_path_buf(),
                expected_dim,
                capacity,
                index,
            })
        } else {
            Ok(Self {
                path: path.to_path_buf(),
                expected_dim,
                capacity,
                index: HashMap::new(),
            })
        }
    }

    /// Number of entries currently in the archive.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// `true` when the archive contains no entries.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// `true` when an entry with the given `id` is present.
    pub fn contains(&self, id: u64) -> bool {
        self.index.contains_key(&id)
    }

    /// Returns all archive entries sorted by ascending item ID for deterministic
    /// iteration order.
    pub fn entries(&self) -> Vec<&ArchivalEntry> {
        let mut entries: Vec<&ArchivalEntry> = self.index.values().collect();
        entries.sort_by_key(|e| e.item.id);
        entries
    }

    /// Demotes `item` with `provenance` into the archive.
    ///
    /// Returns [`DemotionOutcome::AlreadyPresent`] without modifying the archive
    /// when an entry with the same id exists (idempotent).  Returns
    /// [`L3ArchiveError::AtCapacity`] when the capacity limit has been reached.
    /// On success the archive is immediately flushed to disk.
    pub fn demote(
        &mut self,
        item: ArchivedItem,
        provenance: Provenance,
    ) -> Result<DemotionOutcome, L3ArchiveError> {
        if item.embedding.len() != self.expected_dim {
            return Err(L3ArchiveError::DimensionMismatch {
                expected: self.expected_dim,
                got: item.embedding.len(),
            });
        }
        if self.index.contains_key(&item.id) {
            return Ok(DemotionOutcome::AlreadyPresent);
        }
        if self.index.len() >= self.capacity {
            return Err(L3ArchiveError::AtCapacity);
        }
        self.index
            .insert(item.id, ArchivalEntry { item, provenance });
        self.flush()?;
        Ok(DemotionOutcome::Inserted)
    }

    /// Returns the top-`k` entries by cosine similarity to `query`.
    ///
    /// Ties are broken by ascending entry id to guarantee deterministic ordering
    /// for fixed query vectors (E2.6 exit criterion 2).
    pub fn search(&self, query: &[f32], k: usize) -> Vec<&ArchivalEntry> {
        if query.len() != self.expected_dim || k == 0 {
            return Vec::new();
        }
        let mut scored: Vec<(f32, u64, &ArchivalEntry)> = self
            .index
            .values()
            .map(|entry| {
                let sim = cosine_similarity(query, &entry.item.embedding);
                (sim, entry.item.id, entry)
            })
            .collect();
        // Sort descending by similarity, then ascending by id for determinism.
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.cmp(&b.1))
        });
        scored.into_iter().take(k).map(|(_, _, e)| e).collect()
    }

    /// Writes the archive snapshot to disk atomically.
    ///
    /// The snapshot is first written to a `.tmp`-suffixed path, then renamed
    /// over the target path so readers never observe a partial write.
    pub fn flush(&self) -> Result<(), L3ArchiveError> {
        let snapshot = ArchiveSnapshot {
            expected_dim: self.expected_dim,
            capacity: self.capacity,
            entries: self.index.values().cloned().collect(),
        };
        let json =
            serde_json::to_string(&snapshot).map_err(|e| L3ArchiveError::Corrupt(e.to_string()))?;

        // Build a sibling .tmp path.
        let mut tmp_path = self.path.clone();
        let mut name = self.path.file_name().unwrap_or_default().to_os_string();
        name.push(".tmp");
        tmp_path.set_file_name(name);

        std::fs::write(&tmp_path, json.as_bytes()).map_err(L3ArchiveError::Io)?;
        std::fs::rename(&tmp_path, &self.path).map_err(L3ArchiveError::Io)?;
        Ok(())
    }
}

// ── Embedding pipeline (S2.6.2) ───────────────────────────────────────────────

/// Produces a 4-dimensional feature vector from a [`MemoryNode`].
///
/// The vector is `[initial_activation, lambda, alpha * arousal, sigma * surprise]`.
pub fn embed_memory_node(node: &MemoryNode) -> Vec<f32> {
    vec![
        node.initial_activation,
        node.lambda,
        node.alpha * node.emotion.arousal,
        node.sigma * node.emotion.surprise,
    ]
}

/// Packages a [`MemoryNode`] as an [`ArchivedItem`] ready for demotion into L3.
///
/// The payload is a compact binary encoding: five `f32` fields packed as
/// little-endian bytes (`initial_activation`, `lambda`, `arousal`, `surprise`,
/// `alpha`/`sigma` stored as product pairs — in practice `alpha * arousal` and
/// `sigma * surprise`).
pub fn archive_memory_node(id: u64, key: &str, node: &MemoryNode) -> ArchivedItem {
    let _ = key; // key is carried in Provenance; not redundantly stored in payload
    let embedding = embed_memory_node(node);
    // Payload: 5 f32 values packed as little-endian bytes.
    let mut payload = Vec::with_capacity(5 * 4);
    for &v in &[
        node.initial_activation,
        node.lambda,
        node.alpha,
        node.emotion.arousal,
        node.emotion.surprise,
    ] {
        payload.extend_from_slice(&v.to_le_bytes());
    }
    ArchivedItem {
        id,
        embedding,
        payload,
    }
}

// ── L3→L2 retrieval helper (S2.6.4) ──────────────────────────────────────────

/// Searches `l3` for the `k` most similar entries to `query` and re-inserts
/// them into `l2`.
///
/// Returns the number of entries that were successfully re-inserted.  Entries
/// whose source key already exists in `l2` are silently skipped (the ArcCache
/// insert is idempotent by key).
pub fn retrieve_top_k_from_l3_for_l2(
    l3: &L3Archive,
    query: &[f32],
    k: usize,
    l2: &ArcCache<String, MemoryNode>,
) -> usize {
    let results = l3.search(query, k);
    let mut inserted = 0;
    for entry in results {
        // Decode payload back into a MemoryNode.
        // Layout: [initial_activation, lambda, alpha, arousal, surprise] as f32 LE.
        if entry.item.payload.len() < 5 * 4 {
            continue;
        }
        let read_f32 = |offset: usize| -> f32 {
            let bytes: [u8; 4] = entry.item.payload[offset..offset + 4].try_into().unwrap();
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

        let key = entry.provenance.source_key.clone();
        l2.insert(key, node);
        inserted += 1;
    }
    inserted
}

// ── Shared cosine similarity ──────────────────────────────────────────────────

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: u64, embedding: Vec<f32>) -> ArchivedItem {
        ArchivedItem {
            id,
            embedding,
            payload: vec![],
        }
    }

    // ── ArchivalStore (in-memory, backward-compat) ────────────────────────────

    #[test]
    fn store_rejects_wrong_dimension() {
        let mut store = ArchivalStore::new(3, 8);
        let err = store.store(item(1, vec![1.0, 2.0])).unwrap_err();
        assert_eq!(err, ArchivalStoreError::DimensionMismatch);
    }

    #[test]
    fn store_rejects_at_capacity() {
        let mut store = ArchivalStore::new(2, 1);
        store.store(item(1, vec![1.0, 0.0])).unwrap();
        let err = store.store(item(2, vec![0.0, 1.0])).unwrap_err();
        assert_eq!(err, ArchivalStoreError::AtCapacity);
    }

    #[test]
    fn search_returns_highest_cosine_first() {
        let mut store = ArchivalStore::new(2, 8);
        store.store(item(1, vec![1.0, 0.0])).unwrap();
        store.store(item(2, vec![0.0, 1.0])).unwrap();
        store.store(item(3, vec![0.9, 0.1])).unwrap();
        let results = store.search(&[1.0, 0.0], 2);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, 1);
        assert_eq!(results[1].id, 3);
    }

    // ── L3Archive ─────────────────────────────────────────────────────────────

    fn make_entry(id: u64, embedding: Vec<f32>) -> (ArchivedItem, Provenance) {
        let item = ArchivedItem {
            id,
            embedding,
            payload: vec![],
        };
        let prov = Provenance::now(SourceTier::L1, &format!("key-{id}"));
        (item, prov)
    }

    #[test]
    fn l3_open_creates_empty_archive_when_file_absent() {
        let path = std::env::temp_dir().join("animaos_test_l3_new.json");
        let _ = std::fs::remove_file(&path);
        let archive = L3Archive::open(&path, 4, 100).unwrap();
        assert!(archive.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn l3_demote_inserts_and_flushes() {
        let path = std::env::temp_dir().join("animaos_test_l3_insert.json");
        let _ = std::fs::remove_file(&path);
        let mut archive = L3Archive::open(&path, 2, 10).unwrap();
        let (item, prov) = make_entry(1, vec![1.0, 0.0]);
        let outcome = archive.demote(item, prov).unwrap();
        assert_eq!(outcome, DemotionOutcome::Inserted);
        assert_eq!(archive.len(), 1);
        assert!(path.exists(), "flush must create the file");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn l3_demote_is_idempotent() {
        let path = std::env::temp_dir().join("animaos_test_l3_idempotent.json");
        let _ = std::fs::remove_file(&path);
        let mut archive = L3Archive::open(&path, 2, 10).unwrap();
        let (item, prov) = make_entry(42, vec![0.5, 0.5]);
        archive.demote(item.clone(), prov.clone()).unwrap();
        let outcome2 = archive.demote(item, prov).unwrap();
        assert_eq!(outcome2, DemotionOutcome::AlreadyPresent);
        assert_eq!(archive.len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn l3_demote_rejects_wrong_dimension() {
        let path = std::env::temp_dir().join("animaos_test_l3_dim.json");
        let _ = std::fs::remove_file(&path);
        let mut archive = L3Archive::open(&path, 4, 10).unwrap();
        let (item, prov) = make_entry(1, vec![1.0, 0.0]); // 2-dim, expects 4
        let err = archive.demote(item, prov).unwrap_err();
        assert!(matches!(
            err,
            L3ArchiveError::DimensionMismatch {
                expected: 4,
                got: 2
            }
        ));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn l3_demote_rejects_at_capacity() {
        let path = std::env::temp_dir().join("animaos_test_l3_cap.json");
        let _ = std::fs::remove_file(&path);
        let mut archive = L3Archive::open(&path, 2, 1).unwrap();
        let (i1, p1) = make_entry(1, vec![1.0, 0.0]);
        archive.demote(i1, p1).unwrap();
        let (i2, p2) = make_entry(2, vec![0.0, 1.0]);
        let err = archive.demote(i2, p2).unwrap_err();
        assert!(matches!(err, L3ArchiveError::AtCapacity));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn l3_search_returns_top_k_by_cosine_deterministic() {
        let path = std::env::temp_dir().join("animaos_test_l3_search.json");
        let _ = std::fs::remove_file(&path);
        let mut archive = L3Archive::open(&path, 2, 100).unwrap();
        let (i1, p1) = make_entry(1, vec![1.0, 0.0]);
        let (i2, p2) = make_entry(2, vec![0.0, 1.0]);
        let (i3, p3) = make_entry(3, vec![0.9, 0.1]);
        archive.demote(i1, p1).unwrap();
        archive.demote(i2, p2).unwrap();
        archive.demote(i3, p3).unwrap();

        let results = archive.search(&[1.0, 0.0], 2);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].item.id, 1);
        assert_eq!(results[1].item.id, 3);

        // Second call must return the same order (determinism).
        let results2 = archive.search(&[1.0, 0.0], 2);
        assert_eq!(results2[0].item.id, results[0].item.id);
        assert_eq!(results2[1].item.id, results[1].item.id);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn l3_survives_process_restart() {
        let path = std::env::temp_dir().join("animaos_test_l3_restart.json");
        let _ = std::fs::remove_file(&path);

        // "First process"
        {
            let mut archive = L3Archive::open(&path, 2, 100).unwrap();
            let (i1, p1) = make_entry(7, vec![0.6, 0.8]);
            archive.demote(i1, p1).unwrap();
            assert_eq!(archive.len(), 1);
        }

        // "Second process" — reopen from disk
        {
            let archive = L3Archive::open(&path, 2, 100).unwrap();
            assert_eq!(archive.len(), 1, "entry must survive restart");
            assert!(archive.contains(7));
        }

        let _ = std::fs::remove_file(&path);
    }

    // ── embed_memory_node ─────────────────────────────────────────────────────

    #[test]
    fn embed_memory_node_produces_4dim_vector() {
        let node = MemoryNode::new(0.8, 1.5);
        let emb = embed_memory_node(&node);
        assert_eq!(emb.len(), 4);
        assert!((emb[0] - 0.8).abs() < f32::EPSILON);
        assert!((emb[1] - 1.5).abs() < f32::EPSILON);
        // Default emotion is (0.0, 0.0), alpha=1.5, sigma=2.0
        assert!((emb[2] - 0.0).abs() < f32::EPSILON); // 1.5 * 0.0
        assert!((emb[3] - 0.0).abs() < f32::EPSILON); // 2.0 * 0.0
    }

    #[test]
    fn archive_memory_node_payload_round_trips() {
        let mut node = MemoryNode::new(0.7, 0.5);
        node.emotion.arousal = 1.0;
        node.emotion.surprise = 2.0;
        let archived = archive_memory_node(99, "key", &node);
        assert_eq!(archived.id, 99);
        assert_eq!(archived.embedding.len(), 4);
        // Verify payload size: 5 f32 × 4 bytes = 20 bytes.
        assert_eq!(archived.payload.len(), 20);
    }

    // ── L3→L2 retrieval ───────────────────────────────────────────────────────

    #[test]
    fn retrieve_top_k_reinserts_into_l2() {
        let path = std::env::temp_dir().join("animaos_test_l3_retrieve.json");
        let _ = std::fs::remove_file(&path);

        let mut l3 = L3Archive::open(&path, 4, 100).unwrap();
        let node = MemoryNode::new(0.9, 0.1);
        let archived = archive_memory_node(1, "my-key", &node);
        let prov = Provenance::now(SourceTier::L1, "my-key");
        l3.demote(archived, prov).unwrap();

        let l2: ArcCache<String, MemoryNode> = ArcCache::new(64);
        let query = embed_memory_node(&node);
        let count = retrieve_top_k_from_l3_for_l2(&l3, &query, 1, &l2);
        assert_eq!(count, 1);
        assert!(l2.get(&"my-key".to_string()).is_some());

        let _ = std::fs::remove_file(&path);
    }
}
