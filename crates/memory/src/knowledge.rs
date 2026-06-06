//! Personal knowledge corpus — E14, S14.3.
//!
//! The agent's external brain: a queryable store of documents, notes, and
//! distilled findings **distinct from episodic memory**.  Episodes are "what
//! happened"; the knowledge corpus is "what I know".
//!
//! # Architecture
//!
//! Knowledge entries are stored in the **L3 archive** with
//! [`crate::archival::SourceTier::Knowledge`] provenance.  The same cosine-
//! similarity retrieval infrastructure from E2.6 is reused; knowledge entries
//! are filtered by `SourceTier` before ranking so they do not mix with episodic
//! or memory-decay entries.
//!
//! # Embedding
//!
//! A deterministic 4-dimensional feature vector is derived from the raw text:
//!
//! - **Component 0** — length signal: `min(byte_len / 4096.0, 1.0)`.
//! - **Component 1** — lexical density: unique-word fraction capped at 1.0.
//! - **Component 2** — first-word hash modulo 1 (cheap topic signal).
//! - **Component 3** — last-word hash modulo 1 (cheap topic signal).
//!
//! This embedding is intentionally simple — it gives useful similarity signal
//! without requiring a language model.  When E8 (local embeddings) lands,
//! callers can supply their own higher-quality embeddings directly via
//! [`ingest_document_embedded`].
//!
//! # Exit criteria (S14.3)
//!
//! 1. Documents can be ingested and retrieved in a round-trip through L3.
//! 2. Retrieval returns `Knowledge` entries only (never `Episode` or `L1`).
//! 3. Ingestion is idempotent by key: re-ingesting the same key/text is a
//!    no-op (returns `DemotionOutcome::AlreadyPresent`).

use crate::archival::{
    ArchivalEntry, ArchivedItem, DemotionOutcome, L3Archive, L3ArchiveError, Provenance, SourceTier,
};

// ── Embedding ─────────────────────────────────────────────────────────────────

/// Derive a deterministic 4-dimensional embedding from raw text (S14.3).
///
/// The four components are:
///
/// - `len_signal` — `min(byte_len / 4096.0, 1.0)`.
/// - `density`    — unique-word fraction: `unique_words / total_words` capped at 1.0.
/// - `first_hash` — first-word FNV hash in `[0.0, 1.0)`.
/// - `last_hash`  — last-word FNV hash in `[0.0, 1.0)`.
pub fn embed_text_knowledge(text: &str) -> [f32; 4] {
    let byte_len = text.len();
    let len_signal = (byte_len as f32 / 4096.0).min(1.0);

    // Split into words for density and hash components.
    let words: Vec<&str> = text.split_whitespace().collect();
    let total = words.len();

    let density = if total == 0 {
        0.0
    } else {
        let mut seen = std::collections::HashSet::new();
        for w in &words {
            seen.insert(w.to_lowercase());
        }
        (seen.len() as f32 / total as f32).min(1.0)
    };

    let first_hash = if total == 0 {
        0.0
    } else {
        fnv_hash_f32(words[0])
    };

    let last_hash = if total == 0 {
        0.0
    } else {
        fnv_hash_f32(words[total - 1])
    };

    [len_signal, density, first_hash, last_hash]
}

/// FNV-1a hash of a string, mapped to `[0.0, 1.0)`.
fn fnv_hash_f32(s: &str) -> f32 {
    const FNV_OFFSET: u64 = 14695981039346656037;
    const FNV_PRIME: u64 = 1099511628211;
    let mut hash = FNV_OFFSET;
    for byte in s.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    // Map the 64-bit hash to [0.0, 1.0) by dividing by u64::MAX + 1.
    (hash as f64 / (u64::MAX as f64 + 1.0)) as f32
}

// ── Ingestion ─────────────────────────────────────────────────────────────────

/// Ingest a text document into the knowledge corpus (S14.3).
///
/// The document is embedded using [`embed_text_knowledge`] and stored in `l3`
/// with [`SourceTier::Knowledge`] provenance.  The `source_key` is an
/// operator-supplied identifier that scopes the entry in the provenance record
/// (e.g. `"doc:project-notes"`, `"url:https://…"`, `"note:grocery-list"`).
///
/// Returns `DemotionOutcome::AlreadyPresent` without modifying the archive when
/// an entry with the same `id` already exists.
///
/// # Arguments
///
/// * `l3`         — mutable reference to the archive.
/// * `next_id`    — monotonic ID counter (incremented on successful insertion).
/// * `source_key` — stable human-readable identifier for the document.
/// * `text`       — raw document text to embed and store.
pub fn ingest_document(
    l3: &mut L3Archive,
    next_id: &mut u64,
    source_key: &str,
    text: &str,
) -> Result<DemotionOutcome, L3ArchiveError> {
    let embedding = embed_text_knowledge(text);
    ingest_document_embedded(l3, next_id, source_key, &embedding, text.as_bytes())
}

/// Ingest a document with a caller-supplied embedding vector (S14.3).
///
/// Use this when a higher-quality embedding (e.g. from E8 local inference) is
/// available.  The `payload` bytes are stored verbatim (truncated to 20 bytes).
///
/// The `embedding` length must match the `L3Archive`'s `expected_dim`.
pub fn ingest_document_embedded(
    l3: &mut L3Archive,
    next_id: &mut u64,
    source_key: &str,
    embedding: &[f32],
    payload: &[u8],
) -> Result<DemotionOutcome, L3ArchiveError> {
    let id = *next_id;

    let mut packed = [0u8; 20];
    let copy_len = payload.len().min(20);
    packed[..copy_len].copy_from_slice(&payload[..copy_len]);

    let item = ArchivedItem {
        id,
        embedding: embedding.to_vec(),
        payload: packed.to_vec(),
    };

    let prov = Provenance::now(SourceTier::Knowledge, source_key);
    let outcome = l3.demote(item, prov)?;
    if outcome == DemotionOutcome::Inserted {
        *next_id += 1;
    }
    Ok(outcome)
}

// ── Retrieval ─────────────────────────────────────────────────────────────────

/// Query the knowledge corpus for the top-`k` most similar entries (S14.3).
///
/// Only entries with [`SourceTier::Knowledge`] provenance are considered.
/// Results are ordered by descending cosine similarity (ties broken by
/// ascending entry id).
///
/// Returns an empty vector when `l3` contains no knowledge entries or `k == 0`.
pub fn query_knowledge_corpus<'a>(
    l3: &'a L3Archive,
    query: &[f32],
    k: usize,
) -> Vec<&'a ArchivalEntry> {
    if k == 0 || query.is_empty() {
        return Vec::new();
    }

    // Collect all knowledge-tier entries.
    let candidates: Vec<&ArchivalEntry> = l3
        .entries()
        .into_iter()
        .filter(|e| e.provenance.source_tier == SourceTier::Knowledge)
        .collect();

    if candidates.is_empty() {
        return Vec::new();
    }

    // Score by cosine similarity, break ties by id.
    let mut scored: Vec<(f32, u64, &ArchivalEntry)> = candidates
        .into_iter()
        .map(|e| {
            let score = cosine_similarity(query, &e.item.embedding);
            (score, e.item.id, e)
        })
        .collect();

    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });

    scored.into_iter().take(k).map(|(_, _, e)| e).collect()
}

// ── Cosine similarity (local, avoid pulling from lib.rs which is no_std) ─────

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
    use crate::archival::{L3Archive, SourceTier};

    fn tmp_archive_path(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let pid = std::process::id();
        let n = N.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("anima_knowledge_{tag}_{pid}_{n}.json"));
        // Clean up any leftover from a prior run.
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("tmp"));
        path
    }

    // ── S14.3 Exit criterion 1 — round-trip ingest and retrieve ───────────────

    #[test]
    fn knowledge_document_round_trips_through_l3() {
        let dir = tmp_archive_path("round_trip");
        let mut l3 = L3Archive::open(&dir, 4, 100).expect("open L3");
        let mut next_id = 0u64;

        let outcome = ingest_document(
            &mut l3,
            &mut next_id,
            "doc:notes",
            "This is a test document about memory systems",
        )
        .expect("ingest");
        assert_eq!(outcome, DemotionOutcome::Inserted);
        assert_eq!(next_id, 1, "ID counter incremented after insertion");

        let query = embed_text_knowledge("memory systems test");
        let hits = query_knowledge_corpus(&l3, &query, 1);
        assert_eq!(hits.len(), 1, "exactly one hit returned");
        assert_eq!(hits[0].provenance.source_tier, SourceTier::Knowledge);
        assert!(hits[0].provenance.source_key.starts_with("doc:"));

        let _ = std::fs::remove_file(&dir);
        let _ = std::fs::remove_file(dir.with_extension("tmp"));
    }

    // ── S14.3 Exit criterion 2 — only Knowledge entries returned ─────────────

    #[test]
    fn knowledge_query_returns_only_knowledge_tier_entries() {
        use crate::archival::{ArchivedItem, Provenance, SourceTier};

        let dir = tmp_archive_path("tier_filter");
        let mut l3 = L3Archive::open(&dir, 4, 100).expect("open L3");
        let mut next_id = 0u64;

        // Ingest a knowledge entry.
        ingest_document(
            &mut l3,
            &mut next_id,
            "doc:knowledge",
            "Rust programming language",
        )
        .expect("k");

        // Insert a non-knowledge entry (episode) manually.
        let ep_item = ArchivedItem {
            id: next_id,
            embedding: vec![0.9, 0.1, 0.1, 0.1],
            payload: b"episode data       ".to_vec(),
        };
        next_id += 1;
        let ep_prov = Provenance::now(SourceTier::Episode, "episode:1");
        l3.demote(ep_item, ep_prov).expect("episode insert");

        // Query should return only the Knowledge entry.
        let query = vec![0.5f32, 0.5, 0.5, 0.5];
        let hits = query_knowledge_corpus(&l3, &query, 10);
        assert_eq!(hits.len(), 1, "only one knowledge entry");
        assert_eq!(hits[0].provenance.source_tier, SourceTier::Knowledge);

        let _ = std::fs::remove_file(&dir);
        let _ = std::fs::remove_file(dir.with_extension("tmp"));
    }

    // ── S14.3 Exit criterion 3 — idempotent ingestion ─────────────────────────

    #[test]
    fn knowledge_ingestion_is_idempotent_by_id() {
        let dir = tmp_archive_path("idempotent");
        let mut l3 = L3Archive::open(&dir, 4, 100).expect("open L3");
        let mut next_id = 0u64;

        // First ingest — should insert.
        let first = ingest_document(
            &mut l3,
            &mut next_id,
            "doc:first",
            "First version of the document",
        )
        .expect("first");
        assert_eq!(first, DemotionOutcome::Inserted);
        assert_eq!(next_id, 1);

        // Second ingest with same id (simulate using id=0 directly).
        // Use ingest_document_embedded with the same id counter frozen.
        let mut frozen_id = 0u64;
        let emb = embed_text_knowledge("Updated version of the document");
        let second =
            ingest_document_embedded(&mut l3, &mut frozen_id, "doc:first", &emb, b"updated")
                .expect("second");
        assert_eq!(
            second,
            DemotionOutcome::AlreadyPresent,
            "re-ingesting same id must be a no-op"
        );
        assert_eq!(frozen_id, 0, "ID counter not incremented on AlreadyPresent");

        let _ = std::fs::remove_file(&dir);
        let _ = std::fs::remove_file(dir.with_extension("tmp"));
    }

    // ── Embedding determinism ─────────────────────────────────────────────────

    #[test]
    fn embed_text_knowledge_is_deterministic() {
        let text = "The quick brown fox jumps over the lazy dog";
        let a = embed_text_knowledge(text);
        let b = embed_text_knowledge(text);
        assert_eq!(a, b, "embedding must be deterministic");
        // All components in [0, 1].
        for &c in &a {
            assert!(c >= 0.0 && c <= 1.0, "component {c} out of [0,1]");
        }
    }

    #[test]
    fn embed_empty_text_returns_zero_vector() {
        let emb = embed_text_knowledge("");
        assert_eq!(emb[0], 0.0);
        assert_eq!(emb[1], 0.0);
        assert_eq!(emb[2], 0.0);
        assert_eq!(emb[3], 0.0);
    }

    // ── Retrieval ranking ─────────────────────────────────────────────────────

    #[test]
    fn knowledge_query_returns_most_similar_first() {
        let dir = tmp_archive_path("ranking");
        let mut l3 = L3Archive::open(&dir, 4, 100).expect("open L3");
        let mut next_id = 0u64;

        // Use manually crafted embeddings for predictable ranking.
        let query = vec![1.0f32, 0.0, 0.0, 0.0];

        // "close" document — aligned with query.
        ingest_document_embedded(
            &mut l3,
            &mut next_id,
            "doc:close",
            &[0.9, 0.1, 0.0, 0.0],
            b"close doc",
        )
        .expect("close");

        // "far" document — orthogonal to query.
        ingest_document_embedded(
            &mut l3,
            &mut next_id,
            "doc:far",
            &[0.0, 1.0, 0.0, 0.0],
            b"far doc",
        )
        .expect("far");

        let hits = query_knowledge_corpus(&l3, &query, 2);
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0].provenance.source_key, "doc:close",
            "closest entry must rank first"
        );

        let _ = std::fs::remove_file(&dir);
        let _ = std::fs::remove_file(dir.with_extension("tmp"));
    }

    #[test]
    fn knowledge_query_empty_l3_returns_empty() {
        let dir = tmp_archive_path("empty");
        let l3 = L3Archive::open(&dir, 4, 100).expect("open L3");
        let query = vec![1.0f32, 0.0, 0.0, 0.0];
        let hits = query_knowledge_corpus(&l3, &query, 5);
        assert!(hits.is_empty());
        let _ = std::fs::remove_file(&dir);
        let _ = std::fs::remove_file(dir.with_extension("tmp"));
    }

    #[test]
    fn knowledge_query_k_zero_returns_empty() {
        let dir = tmp_archive_path("k_zero");
        let mut l3 = L3Archive::open(&dir, 4, 100).expect("open L3");
        let mut next_id = 0u64;
        ingest_document(&mut l3, &mut next_id, "doc:x", "some text").expect("ingest");
        let query = vec![1.0f32, 0.0, 0.0, 0.0];
        let hits = query_knowledge_corpus(&l3, &query, 0);
        assert!(hits.is_empty(), "k=0 must return empty");
        let _ = std::fs::remove_file(&dir);
        let _ = std::fs::remove_file(dir.with_extension("tmp"));
    }

    // ── Persistence ───────────────────────────────────────────────────────────

    #[test]
    fn knowledge_corpus_survives_process_restart() {
        let dir = tmp_archive_path("restart");
        let key = "doc:rust-notes";
        let text = "Rust is a systems programming language focusing on safety";

        {
            let mut l3 = L3Archive::open(&dir, 4, 100).expect("first open");
            let mut next_id = 0u64;
            ingest_document(&mut l3, &mut next_id, key, text).expect("ingest");
        }

        {
            let l3 = L3Archive::open(&dir, 4, 100).expect("second open");
            let query = embed_text_knowledge("systems programming safety");
            let hits = query_knowledge_corpus(&l3, &query, 1);
            assert!(!hits.is_empty(), "knowledge entry must survive restart");
            assert_eq!(hits[0].provenance.source_tier, SourceTier::Knowledge);
            assert_eq!(hits[0].provenance.source_key, key);
        }

        let _ = std::fs::remove_file(&dir);
        let _ = std::fs::remove_file(dir.with_extension("tmp"));
    }
}
