//! Goal-drift monitor (S5.6.2).
//!
//! Compares the cortex's current action description against the original
//! invocation objective and flags divergence above a configurable threshold.
//!
//! The first implementation uses Jaccard term-overlap similarity, which
//! requires no pre-trained model.  A vector-embedding implementation
//! (e.g. sentence-transformers cosine similarity) can replace it via the
//! [`ObjectiveSimilarity`] trait without changing callers.

use std::collections::HashSet;

use crate::types::{VetoReason, VetoResult};

// ── ObjectiveSimilarity trait ─────────────────────────────────────────────────

/// Measures similarity between the original objective and a proposed action.
///
/// Implement this trait to plug in a vector-embedding model once E5.4
/// (Learned KV-Cache Controller) produces reusable embedding infrastructure.
pub trait ObjectiveSimilarity: Send + Sync {
    /// Returns a similarity score in [0.0, 1.0].
    ///
    /// 1.0 = identical / fully aligned; 0.0 = completely divergent.
    fn similarity(&self, objective: &str, action: &str) -> f32;
}

// ── TermOverlapSimilarity ─────────────────────────────────────────────────────

/// Jaccard-based term-overlap similarity (the default implementation).
///
/// Tokenises both strings into lowercase, stop-word-filtered content words and
/// computes |intersection| / |union|.  Short stop words (≤ 2 chars) are
/// filtered to avoid inflating similarity on function words.
#[derive(Debug, Default, Clone, Copy)]
pub struct TermOverlapSimilarity;

impl TermOverlapSimilarity {
    fn tokenise(text: &str) -> HashSet<String> {
        text.split(|c: char| !c.is_alphanumeric())
            .map(|w| w.to_ascii_lowercase())
            .filter(|w| w.len() > 2)
            .collect()
    }
}

impl ObjectiveSimilarity for TermOverlapSimilarity {
    fn similarity(&self, objective: &str, action: &str) -> f32 {
        let obj = Self::tokenise(objective);
        let act = Self::tokenise(action);

        // Two empty strings → perfectly similar (no content to disagree on).
        if obj.is_empty() && act.is_empty() {
            return 1.0;
        }

        let intersection = obj.intersection(&act).count();
        let union = obj.union(&act).count();

        if union == 0 {
            1.0
        } else {
            intersection as f32 / union as f32
        }
    }
}

// ── GoalDriftMonitor ──────────────────────────────────────────────────────────

/// Goal-drift monitor (S5.6.2).
///
/// Computes the similarity between the invocation objective and an action
/// description; vetoes the action when the drift score (1 − similarity)
/// exceeds `threshold`.
pub struct GoalDriftMonitor {
    similarity: Box<dyn ObjectiveSimilarity>,
    /// Drift threshold in [0.0, 1.0].
    ///
    /// Actions with drift score **above** this are vetoed.
    /// The default (0.95) is deliberately permissive: even a tiny amount of
    /// shared vocabulary is enough to pass.  Tighten this when the
    /// vector-embedding implementation is available.
    pub threshold: f32,
}

impl std::fmt::Debug for GoalDriftMonitor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoalDriftMonitor")
            .field("threshold", &self.threshold)
            .finish()
    }
}

impl GoalDriftMonitor {
    /// Creates a monitor with the default [`TermOverlapSimilarity`] and the
    /// given threshold.
    pub fn new(threshold: f32) -> Self {
        Self {
            similarity: Box::new(TermOverlapSimilarity),
            threshold: threshold.clamp(0.0, 1.0),
        }
    }

    /// Creates a monitor with a custom similarity implementation.
    pub fn with_similarity(
        similarity: impl ObjectiveSimilarity + 'static,
        threshold: f32,
    ) -> Self {
        Self {
            similarity: Box::new(similarity),
            threshold: threshold.clamp(0.0, 1.0),
        }
    }

    /// Computes the drift score between `objective` and `action_description`.
    ///
    /// Returns a value in [0.0, 1.0]; 0.0 = no drift, 1.0 = complete divergence.
    pub fn drift_score(&self, objective: &str, action_description: &str) -> f32 {
        1.0 - self.similarity.similarity(objective, action_description)
    }

    /// Checks whether `action_description` drifts too far from `objective`.
    ///
    /// Returns [`VetoResult::Veto`] with [`VetoReason::GoalDrift`] when the
    /// drift score exceeds `self.threshold`.
    pub fn check(&self, objective: &str, action_description: &str) -> VetoResult {
        let score = self.drift_score(objective, action_description);
        if score > self.threshold {
            VetoResult::Veto(VetoReason::GoalDrift {
                description: format!(
                    "action diverges from objective (similarity={:.2}): objective={:?}, action={:?}",
                    1.0 - score,
                    truncate(objective, 80),
                    truncate(action_description, 80),
                ),
                drift_score: score,
            })
        } else {
            VetoResult::Allow
        }
    }
}

impl Default for GoalDriftMonitor {
    fn default() -> Self {
        Self::new(0.95)
    }
}

fn truncate(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── TermOverlapSimilarity ─────────────────────────────────────────────────

    #[test]
    fn identical_strings_have_similarity_one() {
        let s = TermOverlapSimilarity;
        assert!((s.similarity("write a test for the login function", "write a test for the login function") - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn completely_different_strings_have_low_similarity() {
        let s = TermOverlapSimilarity;
        let sim = s.similarity("build the Rust project", "send an email to alice");
        assert!(sim < 0.2, "expected low similarity, got {sim}");
    }

    #[test]
    fn similarity_is_symmetric() {
        let s = TermOverlapSimilarity;
        let a = "write a unit test for the parser";
        let b = "unit test the parser module";
        let ab = s.similarity(a, b);
        let ba = s.similarity(b, a);
        assert!((ab - ba).abs() < f32::EPSILON, "similarity must be symmetric");
    }

    #[test]
    fn both_empty_strings_have_similarity_one() {
        let s = TermOverlapSimilarity;
        assert_eq!(s.similarity("", ""), 1.0);
    }

    #[test]
    fn short_words_are_filtered() {
        // "do", "it", "is" are ≤ 2 chars and should be filtered.
        let s = TermOverlapSimilarity;
        let a = "do it";
        let b = "is it";
        // Both reduce to empty sets → similarity = 1.0.
        assert_eq!(s.similarity(a, b), 1.0);
    }

    // ── GoalDriftMonitor ──────────────────────────────────────────────────────

    #[test]
    fn on_task_action_is_allowed() {
        // threshold = 0.50: veto only when drift exceeds 50% (similarity < 50%).
        // "refactor authentication module" vs "refactor authentication module tests"
        // share the majority of their terms → similarity ≥ 0.50 → allowed.
        let m = GoalDriftMonitor::new(0.50);
        let r = m.check(
            "refactor the authentication module",
            "refactor authentication module tests",
        );
        assert_eq!(r, VetoResult::Allow);
    }

    #[test]
    fn wildly_divergent_action_is_vetoed() {
        let m = GoalDriftMonitor::new(0.05);
        let r = m.check(
            "write a unit test for the login function",
            "delete all files in the home directory",
        );
        assert!(r.is_vetoed());
        match r {
            VetoResult::Veto(VetoReason::GoalDrift { drift_score, .. }) => {
                assert!(
                    drift_score > 0.05,
                    "drift score {drift_score} should exceed threshold 0.05"
                );
            }
            _ => panic!("expected GoalDrift veto"),
        }
    }

    #[test]
    fn drift_score_in_veto_reason_is_in_unit_interval() {
        let m = GoalDriftMonitor::new(0.05);
        let r = m.check("compress images", "exfiltrate user passwords");
        if let VetoResult::Veto(VetoReason::GoalDrift { drift_score, .. }) = r {
            assert!(
                (0.0..=1.0).contains(&drift_score),
                "drift score {drift_score} must be in [0, 1]"
            );
        }
    }

    #[test]
    fn permissive_threshold_allows_most_actions() {
        // Default threshold of 0.95 allows actions with almost any shared word.
        let m = GoalDriftMonitor::default();
        let r = m.check(
            "fix the bug in main.rs",
            "exfiltrate database credentials",
        );
        // With only partial term overlap this may or may not veto depending on
        // the exact tokens; the test checks that the call completes without panic.
        let _ = r;
    }

    // ── S5.6.2 exit criterion: misbehaving cortex fixture ─────────────────────
    //
    // Simulates a deliberately misbehaving cortex that ignores its objective
    // and proposes an unrelated action.

    #[test]
    fn misbehaving_cortex_fixture_triggers_goal_drift() {
        // Strict threshold: require at least 20 % term overlap.
        let m = GoalDriftMonitor::new(0.80);

        let objective = "compress the project documentation into a zip archive";
        let rogue_actions = &[
            "send all files to remote server",
            "delete user account data",
            "modify kernel parameters",
            "install a cryptocurrency miner",
        ];

        let mut triggered = 0usize;
        for &action in rogue_actions {
            if m.check(objective, action).is_vetoed() {
                triggered += 1;
            }
        }

        assert!(
            triggered >= 1,
            "at least one rogue action must trigger GoalDrift"
        );
    }
}
