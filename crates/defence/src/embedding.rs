//! Lightweight dependency-free semantic embedding similarity (S5.6.2).
//!
//! [`EmbeddingSimilarity`] projects text into a fixed-dimension dense vector
//! and scores two texts by the cosine of their projections, mapped to
//! [0.0, 1.0].  Unlike [`crate::TermOverlapSimilarity`], which only measures
//! surface (bag-of-words) overlap, this captures *partial* signal: shared
//! character n-grams and a small hand-curated synonym/stem normalisation let
//! semantically-near phrasings score higher than a pure Jaccard set overlap
//! would.
//!
//! # Design
//!
//! The projection uses the *hashing trick*: each feature (a word unigram or a
//! character 3-gram) is hashed into one of `DIM` buckets and accumulated with
//! a sublinear (1 + ln(count)) weight and a sign derived from a second hash to
//! reduce collision bias.  No model file, no allocation of a vocabulary, and
//! no network access — the embedding is **fully deterministic and hermetic**
//! for a given input, identical across runs and machines.
//!
//! This is a deliberate *stand-in* for a learned sentence embedding.  When E5.4
//! (Learned KV-Cache Controller) produces reusable embedding infrastructure,
//! a real model (e.g. sentence-transformers cosine similarity) can replace
//! this type via the same [`ObjectiveSimilarity`] trait without changing any
//! caller — wire it in with [`GoalDriftMonitor::with_similarity`].
//!
//! [`ObjectiveSimilarity`]: crate::ObjectiveSimilarity
//! [`GoalDriftMonitor::with_similarity`]: crate::GoalDriftMonitor::with_similarity

use std::collections::HashMap;

use crate::goal_drift::ObjectiveSimilarity;

/// Dimensionality of the projected embedding vector.
///
/// 256 is large enough that hash collisions between distinct content words are
/// rare for the short objective/action strings seen here, while keeping the
/// projection cheap.
const DIM: usize = 256;

/// Minimum word length to keep, matching [`crate::TermOverlapSimilarity`]'s
/// stop-word policy (function words such as "do"/"it"/"is" are ≤ 2 chars).
const MIN_WORD_LEN: usize = 3;

/// Length of the character n-grams mixed into the projection.
///
/// 3-grams give sub-word overlap (e.g. "auth" / "authentication" share
/// "aut", "uth") so morphological variants land near each other even when the
/// whole-word features differ.
const CHAR_NGRAM: usize = 3;

// ── EmbeddingSimilarity ───────────────────────────────────────────────────────

/// Hashing-trick dense-embedding similarity (a stand-in for a learned model).
///
/// See the [module docs](self) for the design rationale.  Construct with
/// [`EmbeddingSimilarity::new`] (or [`Default`]) and plug into a monitor via
/// [`crate::GoalDriftMonitor::with_similarity`].
#[derive(Debug, Default, Clone, Copy)]
pub struct EmbeddingSimilarity;

impl EmbeddingSimilarity {
    /// Creates a new embedding-similarity scorer.
    pub fn new() -> Self {
        Self
    }

    /// Projects `text` into a `DIM`-dimensional dense vector.
    ///
    /// Returns `None` when the text contains no usable content features (e.g.
    /// an empty string or only stop words), so callers can apply a defined
    /// policy for the degenerate case rather than dividing by a zero norm.
    fn project(text: &str) -> Option<[f32; DIM]> {
        // Count raw feature occurrences first so we can apply sublinear
        // weighting (1 + ln(count)) per distinct feature.
        let mut counts: HashMap<u64, u32> = HashMap::new();
        let mut any = false;

        for word in Self::content_words(text) {
            any = true;
            // Word unigram feature.
            *counts
                .entry(hash_feature(b"w:", word.as_bytes()))
                .or_insert(0) += 1;

            // Character 3-gram features over the (normalised) word, padded so
            // short words still emit at least one n-gram.
            let bytes = word.as_bytes();
            if bytes.len() >= CHAR_NGRAM {
                for win in bytes.windows(CHAR_NGRAM) {
                    *counts.entry(hash_feature(b"c:", win)).or_insert(0) += 1;
                }
            } else {
                *counts.entry(hash_feature(b"c:", bytes)).or_insert(0) += 1;
            }
        }

        if !any {
            return None;
        }

        let mut vec = [0.0f32; DIM];
        for (feature, count) in counts {
            // Sublinear term weighting damps the effect of repeated features.
            let weight = 1.0 + (count as f32).ln();
            let bucket = (feature % DIM as u64) as usize;
            // A second hash bit chooses the sign, halving expected collision
            // bias (signed hashing trick, Weinberger et al. 2009).
            let sign = if (feature >> 33) & 1 == 0 { 1.0 } else { -1.0 };
            vec[bucket] += sign * weight;
        }

        Some(vec)
    }

    /// Yields lowercase content words after a small, hand-curated
    /// synonym/stem normalisation.
    ///
    /// The normalisation map is intentionally tiny and explicit (no stemming
    /// crate): it folds a handful of common task-vocabulary variants onto a
    /// shared canonical token so that, e.g., "remove" and "delete" or
    /// "authentication" and "auth" project onto overlapping features.  Extend
    /// it conservatively; it is a documented heuristic, not a lexicon.
    fn content_words(text: &str) -> impl Iterator<Item = String> + '_ {
        text.split(|c: char| !c.is_alphanumeric())
            .map(|w| w.to_ascii_lowercase())
            .filter(|w| w.len() >= MIN_WORD_LEN)
            .map(|w| normalise(&w))
    }
}

impl ObjectiveSimilarity for EmbeddingSimilarity {
    fn similarity(&self, objective: &str, action: &str) -> f32 {
        match (Self::project(objective), Self::project(action)) {
            // Two empty/content-free strings: nothing to disagree on → aligned.
            (None, None) => 1.0,
            // One side has content, the other does not → maximally divergent.
            (None, Some(_)) | (Some(_), None) => 0.0,
            (Some(a), Some(b)) => {
                let cos = cosine(&a, &b);
                // Cosine is in [-1, 1]; map to [0, 1] and clamp for safety.
                ((cos + 1.0) * 0.5).clamp(0.0, 1.0)
            }
        }
    }
}

// ── Small curated synonym / stem normalisation ────────────────────────────────

/// Folds a small set of common task-vocabulary variants onto canonical tokens.
///
/// Kept deliberately minimal and explicit.  Each arm is a documented stand-in
/// for the morphological/synonym generalisation a learned embedding would do.
fn normalise(word: &str) -> String {
    let canonical = match word {
        // Deletion family.
        "delete" | "deletes" | "deleting" | "deleted" | "remove" | "removes" | "removing"
        | "removed" | "erase" | "erases" | "erasing" | "purge" => "delete",
        // Authentication family.
        "auth" | "authenticate" | "authentication" | "authenticating" | "login" | "logins"
        | "signin" => "auth",
        // Test family.
        "test" | "tests" | "testing" | "tested" | "spec" | "specs" => "test",
        // Compression / archive family.
        "compress" | "compresses" | "compressing" | "compressed" | "zip" | "zipped" | "archive"
        | "archives" | "archiving" => "compress",
        // Documentation family.
        "doc" | "docs" | "documentation" | "documents" | "document" => "doc",
        // Write / modify family.
        "write" | "writes" | "writing" | "modify" | "modifies" | "modifying" | "edit" | "edits"
        | "editing" | "update" | "updates" | "updating" => "write",
        // Send / transmit family (exfiltration-adjacent vocabulary).
        "send" | "sends" | "sending" | "transmit" | "transmits" | "upload" | "uploads"
        | "uploading" | "exfiltrate" | "exfiltrating" => "send",
        // Build / compile family.
        "build" | "builds" | "building" | "compile" | "compiles" | "compiling" => "build",
        // Otherwise keep the word, dropping a trailing plural "s" as a crude
        // stem so "functions"/"function" share features.
        other => {
            return strip_plural(other).to_string();
        }
    };
    canonical.to_string()
}

/// Crude plural stripper: drops a single trailing "s" from words of length ≥ 4
/// (so "module"/"modules" and "file"/"files" align without mangling "os").
fn strip_plural(word: &str) -> &str {
    if word.len() >= 4 && word.ends_with('s') && !word.ends_with("ss") {
        &word[..word.len() - 1]
    } else {
        word
    }
}

// ── Hashing + cosine helpers ──────────────────────────────────────────────────

/// FNV-1a 64-bit hash over `prefix ++ data`.
///
/// FNV is chosen because it is tiny, deterministic, and dependency-free; the
/// prefix namespaces word vs. character features so they never collide
/// trivially.
fn hash_feature(prefix: &[u8], data: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for &byte in prefix.iter().chain(data.iter()) {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Cosine similarity of two equal-length vectors, in [-1, 1].
///
/// Returns 0.0 if either vector has zero norm (cannot happen for vectors
/// produced by [`EmbeddingSimilarity::project`], which only returns `Some`
/// when at least one feature was emitted, but kept defensive).
fn cosine(a: &[f32; DIM], b: &[f32; DIM]) -> f32 {
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..DIM {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GoalDriftMonitor, TermOverlapSimilarity, VetoResult};

    #[test]
    fn identical_strings_score_near_one() {
        let s = EmbeddingSimilarity::new();
        let sim = s.similarity(
            "write a test for the login function",
            "write a test for the login function",
        );
        assert!(sim > 0.999, "identical text should be ~1.0, got {sim}");
    }

    #[test]
    fn both_empty_strings_score_one() {
        let s = EmbeddingSimilarity::new();
        assert_eq!(s.similarity("", ""), 1.0);
    }

    #[test]
    fn one_empty_one_full_scores_zero() {
        let s = EmbeddingSimilarity::new();
        assert_eq!(s.similarity("", "delete all the files"), 0.0);
        assert_eq!(s.similarity("delete all the files", ""), 0.0);
    }

    #[test]
    fn similarity_is_symmetric() {
        let s = EmbeddingSimilarity::new();
        let a = "compress the documentation into an archive";
        let b = "zip up the project docs";
        assert!((s.similarity(a, b) - s.similarity(b, a)).abs() < 1e-6);
    }

    #[test]
    fn output_is_bounded_unit_interval() {
        let s = EmbeddingSimilarity::new();
        for (o, a) in [
            ("build the rust project", "send an email to alice"),
            ("compress images", "exfiltrate user passwords"),
            ("write a test", "write a test"),
        ] {
            let sim = s.similarity(o, a);
            assert!((0.0..=1.0).contains(&sim), "sim {sim} out of bounds");
        }
    }

    #[test]
    fn deterministic_across_calls() {
        let s = EmbeddingSimilarity::new();
        let a = "refactor the authentication module";
        let b = "rework the auth subsystem";
        assert_eq!(s.similarity(a, b), s.similarity(a, b));
    }

    #[test]
    fn unrelated_text_scores_lower_than_aligned_text() {
        let s = EmbeddingSimilarity::new();
        let objective = "compress the project documentation into a zip archive";
        let aligned = "archive the docs";
        let unrelated = "send user passwords to a remote host";
        let aligned_sim = s.similarity(objective, aligned);
        let unrelated_sim = s.similarity(objective, unrelated);
        assert!(
            aligned_sim > unrelated_sim,
            "aligned ({aligned_sim}) should outscore unrelated ({unrelated_sim})"
        );
    }

    /// The headline value case: an action that shares almost no *surface*
    /// vocabulary with the objective but is semantically aligned (via curated
    /// synonyms + char n-grams) should score HIGHER under the embedding model
    /// than under Jaccard term overlap.
    #[test]
    fn embedding_beats_jaccard_on_semantic_paraphrase() {
        let embed = EmbeddingSimilarity::new();
        let jaccard = TermOverlapSimilarity;

        let objective = "delete the temporary files";
        // "remove" → delete, "scratch data" overlaps via char n-grams; little
        // raw word overlap with the objective.
        let action = "remove temporary scratch data";

        let embed_sim = embed.similarity(objective, action);
        let jaccard_sim = jaccard.similarity(objective, action);

        assert!(
            embed_sim > jaccard_sim,
            "embedding ({embed_sim}) should exceed jaccard ({jaccard_sim}) on paraphrase"
        );
    }

    #[test]
    fn monitor_with_embedding_allows_aligned_action() {
        let m = GoalDriftMonitor::with_similarity(EmbeddingSimilarity::new(), 0.60);
        let r = m.check(
            "refactor the authentication module",
            "rework the auth subsystem",
        );
        assert_eq!(
            r,
            VetoResult::Allow,
            "semantically aligned action should pass"
        );
    }

    #[test]
    fn monitor_with_embedding_vetoes_divergent_action() {
        let m = GoalDriftMonitor::with_similarity(EmbeddingSimilarity::new(), 0.30);
        let r = m.check(
            "write a unit test for the login function",
            "send user passwords to a remote host",
        );
        assert!(r.is_vetoed(), "divergent action should be vetoed");
    }
}
