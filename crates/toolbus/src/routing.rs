//! Length-robust relative routing for efferent tool selection.

/// Candidate tool with an associated relevance score for a given query.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCandidate {
    /// Stable tool identifier.
    pub id: String,
    /// Relevance score in arbitrary units; higher is better.
    pub score: f32,
}

/// Filters `candidates` keeping only entries whose score is at least
/// `tau_rel` times the maximum observed score.
///
/// This implements `T_filtered = { t | score(t,q) >= tau_rel * max score }`.
pub fn length_robust_filter(candidates: &[ToolCandidate], tau_rel: f32) -> Vec<ToolCandidate> {
    if candidates.is_empty() {
        return Vec::new();
    }
    let max_score = candidates
        .iter()
        .map(|c| c.score)
        .fold(f32::NEG_INFINITY, f32::max);
    if !max_score.is_finite() || max_score <= 0.0 {
        return Vec::new();
    }
    let threshold = tau_rel * max_score;
    candidates
        .iter()
        .filter(|c| c.score >= threshold)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: &str, score: f32) -> ToolCandidate {
        ToolCandidate {
            id: id.to_string(),
            score,
        }
    }

    #[test]
    fn filter_keeps_only_top_relative_candidates() {
        let candidates = vec![cand("a", 1.0), cand("b", 0.95), cand("c", 0.5)];
        let kept = length_robust_filter(&candidates, 0.9);
        let ids: Vec<&str> = kept.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn empty_input_returns_empty() {
        let kept = length_robust_filter(&[], 0.5);
        assert!(kept.is_empty());
    }

    #[test]
    fn non_positive_max_returns_empty() {
        let kept = length_robust_filter(&[cand("a", 0.0), cand("b", -1.0)], 0.5);
        assert!(kept.is_empty());
    }
}
