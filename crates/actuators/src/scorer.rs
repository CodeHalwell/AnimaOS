//! Semantic tool selection — E7 S7.3.
//!
//! Provides the [`ToolScorer`] trait and two implementations:
//!
//! - [`LexicalScorer`]: a BM25-inspired lexical scorer that tokenises query
//!   and tool descriptions to compute TF-IDF-weighted relevance scores.
//!   CI-safe: no models to download, deterministic for fixed inputs.
//! - [`FixtureScorer`]: returns pre-configured fixed scores — used in hermetic
//!   tests that need to assert exact selection behaviour without committing to
//!   the BM25 formula.
//!
//! The scorer is always applied *within* a route's tier allow-list.  It never
//! widens the set of tools available to the cortex — it only narrows it.

use std::collections::HashMap;

use praxis::{length_robust_filter, ToolCandidate};

// ── Trait ─────────────────────────────────────────────────────────────────────

/// Scores a set of tools against a query and returns [`ToolCandidate`]s.
///
/// Implementations must be deterministic for identical inputs (same query,
/// same tool descriptions in the same order).  The scores are in arbitrary
/// units; they are only compared relative to each other via
/// [`length_robust_filter`].
pub trait ToolScorer: Send + Sync {
    /// Score each `(id, description)` pair against `query`.
    ///
    /// Returns one [`ToolCandidate`] per input tool; order is preserved.
    fn score(&self, query: &str, tools: &[(&str, &str)]) -> Vec<ToolCandidate>;

    /// Convenience wrapper: score and then filter with `tau_rel`.
    ///
    /// This is the standard pipeline: score → [`length_robust_filter`].
    fn select(&self, query: &str, tools: &[(&str, &str)], tau_rel: f32) -> Vec<ToolCandidate> {
        let candidates = self.score(query, tools);
        length_robust_filter(&candidates, tau_rel)
    }
}

// ── FixtureScorer ─────────────────────────────────────────────────────────────

/// Returns pre-configured fixed scores — for hermetic tests only.
///
/// Any tool whose ID is not in the fixture map receives a score of `0.0`.
pub struct FixtureScorer {
    /// `tool_id → fixed score`.
    pub scores: HashMap<String, f32>,
}

impl FixtureScorer {
    /// Create a fixture scorer from an iterator of `(id, score)` pairs.
    pub fn new(pairs: impl IntoIterator<Item = (impl Into<String>, f32)>) -> Self {
        Self {
            scores: pairs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        }
    }
}

impl ToolScorer for FixtureScorer {
    fn score(&self, _query: &str, tools: &[(&str, &str)]) -> Vec<ToolCandidate> {
        tools
            .iter()
            .map(|(id, _)| ToolCandidate {
                id: id.to_string(),
                score: *self.scores.get(*id).unwrap_or(&0.0),
            })
            .collect()
    }
}

// ── LexicalScorer ─────────────────────────────────────────────────────────────

/// BM25-inspired lexical scorer.
///
/// # Algorithm
///
/// For a query `q` and a document corpus of tool descriptions:
///
/// 1. Tokenise both `q` and each description to lowercase ASCII words,
///    stripping punctuation.
/// 2. For each query token `t`, compute:
///    - `tf(t, d)` = raw term frequency in document `d` (count of occurrences).
///    - `idf(t)` = `log2(1 + N / (1 + df(t)))`, where `N` = total documents,
///      `df(t)` = documents containing `t`.
/// 3. Score for document `d` = `Σ_t tf(t,d) * idf(t)`.
///
/// This is a simplified TF-IDF / BM25-lite; it is sufficient for the
/// "narrow the allow-list" use case without requiring a model download.
/// A full BM25 with `k1`/`b` parameters is a drop-in upgrade.
pub struct LexicalScorer;

impl ToolScorer for LexicalScorer {
    fn score(&self, query: &str, tools: &[(&str, &str)]) -> Vec<ToolCandidate> {
        if tools.is_empty() {
            return Vec::new();
        }

        let query_terms: Vec<String> = tokenise(query);
        if query_terms.is_empty() {
            // Zero scores for all tools when the query is empty.
            return tools
                .iter()
                .map(|(id, _)| ToolCandidate {
                    id: id.to_string(),
                    score: 0.0,
                })
                .collect();
        }

        let n = tools.len() as f32;

        // Build term → document frequency table.
        let doc_texts: Vec<Vec<String>> = tools.iter().map(|(_, desc)| tokenise(desc)).collect();
        let mut df: HashMap<&str, usize> = HashMap::new();
        for terms in &doc_texts {
            let unique: std::collections::HashSet<&str> =
                terms.iter().map(String::as_str).collect();
            for t in unique {
                *df.entry(t).or_insert(0) += 1;
            }
        }

        tools
            .iter()
            .enumerate()
            .map(|(i, (id, _))| {
                let doc = &doc_texts[i];
                let mut score = 0.0_f32;
                for qt in &query_terms {
                    let tf = doc.iter().filter(|t| *t == qt).count() as f32;
                    if tf > 0.0 {
                        let df_t = *df.get(qt.as_str()).unwrap_or(&0) as f32;
                        let idf = f32::log2(1.0 + n / (1.0 + df_t));
                        score += tf * idf;
                    }
                }
                ToolCandidate {
                    id: id.to_string(),
                    score,
                }
            })
            .collect()
    }
}

// ── Tokeniser ─────────────────────────────────────────────────────────────────

/// Tokenise text to a sorted, lowercased list of words (ASCII-alphabetic runs).
///
/// Punctuation and digits are treated as delimiters and discarded.
fn tokenise(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphabetic())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const TOOLS: &[(&str, &str)] = &[
        (
            "clock",
            "Returns the current Unix timestamp in milliseconds",
        ),
        ("echo", "Echoes the input payload back to the caller"),
        (
            "web-search",
            "Search the web for information using a search engine query",
        ),
        (
            "text-io",
            "Read and write text files on the local filesystem",
        ),
    ];

    // ── FixtureScorer ─────────────────────────────────────────────────────────

    #[test]
    fn fixture_scorer_returns_configured_scores() {
        let scorer = FixtureScorer::new([("clock", 1.0_f32), ("web-search", 0.8_f32)]);
        let candidates = scorer.score("anything", TOOLS);
        let clock = candidates.iter().find(|c| c.id == "clock").unwrap();
        let web = candidates.iter().find(|c| c.id == "web-search").unwrap();
        let echo = candidates.iter().find(|c| c.id == "echo").unwrap();
        assert_eq!(clock.score, 1.0);
        assert_eq!(web.score, 0.8);
        assert_eq!(echo.score, 0.0); // not in fixture → zero
    }

    #[test]
    fn fixture_scorer_select_narrows_by_tau() {
        let scorer =
            FixtureScorer::new([("clock", 1.0_f32), ("echo", 0.9_f32), ("text-io", 0.3_f32)]);
        let kept = scorer.select("test", TOOLS, 0.85);
        let ids: Vec<&str> = kept.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&"clock"));
        assert!(ids.contains(&"echo"));
        assert!(!ids.contains(&"text-io")); // below 0.85 * 1.0 = 0.85 threshold
    }

    // ── LexicalScorer ─────────────────────────────────────────────────────────

    #[test]
    fn lexical_scorer_ranks_web_search_tool_higher_for_search_query() {
        let scorer = LexicalScorer;
        let candidates = scorer.score("search the web for recent news", TOOLS);
        let web_score = candidates
            .iter()
            .find(|c| c.id == "web-search")
            .map(|c| c.score)
            .unwrap_or(0.0);
        let clock_score = candidates
            .iter()
            .find(|c| c.id == "clock")
            .map(|c| c.score)
            .unwrap_or(0.0);
        assert!(
            web_score > clock_score,
            "web-search score {web_score} should exceed clock score {clock_score}"
        );
    }

    #[test]
    fn lexical_scorer_ranks_clock_tool_higher_for_time_query() {
        let scorer = LexicalScorer;
        let candidates = scorer.score("what is the current unix timestamp", TOOLS);
        let clock_score = candidates
            .iter()
            .find(|c| c.id == "clock")
            .map(|c| c.score)
            .unwrap_or(0.0);
        let web_score = candidates
            .iter()
            .find(|c| c.id == "web-search")
            .map(|c| c.score)
            .unwrap_or(0.0);
        assert!(
            clock_score > web_score,
            "clock score {clock_score} should exceed web-search score {web_score} for time query"
        );
    }

    #[test]
    fn lexical_scorer_empty_query_returns_zero_scores() {
        let scorer = LexicalScorer;
        let candidates = scorer.score("", TOOLS);
        assert_eq!(candidates.len(), TOOLS.len());
        for c in candidates {
            assert_eq!(c.score, 0.0, "expected zero score for empty query");
        }
    }

    #[test]
    fn lexical_scorer_is_deterministic_for_identical_inputs() {
        let scorer = LexicalScorer;
        let a = scorer.score("search the web", TOOLS);
        let b = scorer.score("search the web", TOOLS);
        assert_eq!(a.len(), b.len());
        for (ca, cb) in a.iter().zip(b.iter()) {
            assert_eq!(ca.id, cb.id);
            assert_eq!(ca.score, cb.score);
        }
    }

    #[test]
    fn select_never_widens_input_set() {
        let scorer = LexicalScorer;
        let subset: &[(&str, &str)] = &[
            (
                "clock",
                "Returns the current Unix timestamp in milliseconds",
            ),
            (
                "web-search",
                "Search the web for information using a search engine query",
            ),
        ];
        let kept = scorer.select("search web", subset, 0.5);
        let input_ids: std::collections::HashSet<&str> = subset.iter().map(|(id, _)| *id).collect();
        for c in &kept {
            assert!(
                input_ids.contains(c.id.as_str()),
                "select returned {}, which was not in the input set",
                c.id
            );
        }
        assert!(kept.len() <= subset.len());
    }

    #[test]
    fn tool_selection_never_widens_tier_allow_list() {
        // The tier allow-list is `["clock", "echo"]`.
        // Even with a perfect score for "web-search", it cannot enter the list.
        let tier_tools: &[(&str, &str)] = &[
            (
                "clock",
                "Returns the current Unix timestamp in milliseconds",
            ),
            ("echo", "Echoes the input payload back to the caller"),
        ];
        let scorer = FixtureScorer::new([
            ("clock", 1.0_f32),
            ("echo", 0.9_f32),
            ("web-search", 100.0_f32), // very high score — but NOT in tier_tools
        ]);
        let kept = scorer.select("search", tier_tools, 0.1);
        let ids: Vec<&str> = kept.iter().map(|c| c.id.as_str()).collect();
        assert!(
            !ids.contains(&"web-search"),
            "web-search must not appear: tier boundary"
        );
    }
}
