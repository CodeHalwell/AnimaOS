#![forbid(unsafe_code)]

//! Quality report aggregation over a collection of feedback records — E24 S24.3.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::record::{FeedbackRating, FeedbackRecord};

// ── QualityReport ──────────────────────────────────────────────────────────────

/// Aggregated quality metrics derived from a slice of [`FeedbackRecord`]s.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityReport {
    /// Total number of feedback records included in this report.
    pub total_feedback: usize,
    /// Number of `ThumbsUp` ratings (or `Stars` ≥ 4).
    pub positive_count: usize,
    /// Number of `ThumbsDown` ratings (or `Stars` < 4).
    pub negative_count: usize,
    /// Mean star rating, or `None` if no `Stars` records exist.
    pub avg_stars: Option<f64>,
    /// Mean normalised quality score across all records (`[0.0, 1.0]`).
    pub avg_score: f64,
    /// Per-category feedback counts.
    pub category_counts: HashMap<String, usize>,
    /// Invocations that received the most correction-feedback (up to 10).
    pub top_corrected_invocations: Vec<(String, usize)>,
    /// Invocations with the most total feedback (up to 10).
    pub top_rated_invocations: Vec<(String, usize)>,
}

impl QualityReport {
    /// Generates a quality report from a slice of records.
    ///
    /// The slice may be the full store or a filtered subset (e.g. per-user).
    pub fn generate(records: &[FeedbackRecord]) -> Self {
        let total = records.len();
        if total == 0 {
            return Self {
                total_feedback: 0,
                positive_count: 0,
                negative_count: 0,
                avg_stars: None,
                avg_score: 0.0,
                category_counts: HashMap::new(),
                top_corrected_invocations: Vec::new(),
                top_rated_invocations: Vec::new(),
            };
        }

        let mut positive = 0usize;
        let mut negative = 0usize;
        let mut star_sum = 0u64;
        let mut star_count = 0usize;
        let mut score_sum = 0.0f64;
        let mut category_map: HashMap<String, usize> = HashMap::new();
        let mut correction_map: HashMap<String, usize> = HashMap::new();
        let mut rated_map: HashMap<String, usize> = HashMap::new();

        for rec in records {
            if rec.rating.is_positive() {
                positive += 1;
            } else {
                negative += 1;
            }

            if let FeedbackRating::Stars(n) = &rec.rating {
                star_sum += *n as u64;
                star_count += 1;
            }

            score_sum += rec.rating.as_score();

            for cat in &rec.categories {
                *category_map.entry(cat.as_str().to_string()).or_insert(0) += 1;
            }

            if rec.has_correction() {
                *correction_map.entry(rec.invocation_id.clone()).or_insert(0) += 1;
            }

            *rated_map.entry(rec.invocation_id.clone()).or_insert(0) += 1;
        }

        let avg_stars = if star_count > 0 {
            Some(star_sum as f64 / star_count as f64)
        } else {
            None
        };

        let mut top_corrected: Vec<(String, usize)> = correction_map.into_iter().collect();
        top_corrected.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        top_corrected.truncate(10);

        let mut top_rated: Vec<(String, usize)> = rated_map.into_iter().collect();
        top_rated.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        top_rated.truncate(10);

        Self {
            total_feedback: total,
            positive_count: positive,
            negative_count: negative,
            avg_stars,
            avg_score: score_sum / total as f64,
            category_counts: category_map,
            top_corrected_invocations: top_corrected,
            top_rated_invocations: top_rated,
        }
    }

    /// Fraction of positive ratings out of all non-neutral ratings (`[0.0, 1.0]`).
    ///
    /// Returns `None` when there are no positive-or-negative records.
    pub fn satisfaction_rate(&self) -> Option<f64> {
        let denom = self.positive_count + self.negative_count;
        if denom == 0 {
            None
        } else {
            Some(self.positive_count as f64 / denom as f64)
        }
    }

    /// Satisfaction rate as a percentage (0–100), rounded.
    pub fn satisfaction_pct(&self) -> Option<u32> {
        self.satisfaction_rate().map(|r| (r * 100.0).round() as u32)
    }
}

// ── WeightedTrainingHint ────────────────────────────────────────────────────────

/// Links a cortex invocation to a training signal derived from user feedback.
///
/// Positive feedback produces a reinforcement hint; negative feedback with
/// a correction produces a correction hint that the fine-tuning pipeline
/// can use to build a contrastive training pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeightedTrainingHint {
    /// Cortex invocation this hint applies to.
    pub invocation_id: String,
    /// Weight in `[0.0, 1.0]` — mean score across all feedback for this invocation.
    pub weight: f64,
    /// Whether this hint should be treated as reinforcement (positive) or
    /// a contrastive correction (negative with correction text).
    pub is_reinforcement: bool,
    /// Operator-supplied correction text, if any.
    pub correction_text: Option<String>,
    /// Number of feedback records that contributed to this hint.
    pub feedback_count: usize,
}

/// Builds [`WeightedTrainingHint`]s from a store's records.
///
/// One hint is emitted per unique `invocation_id`. Hints are sorted by
/// `weight` descending so callers can easily take the top-N most informative
/// pairs.
pub fn build_training_hints(records: &[FeedbackRecord]) -> Vec<WeightedTrainingHint> {
    let mut inv_map: HashMap<String, Vec<&FeedbackRecord>> = HashMap::new();
    for rec in records {
        inv_map
            .entry(rec.invocation_id.clone())
            .or_default()
            .push(rec);
    }

    let mut hints: Vec<WeightedTrainingHint> = inv_map
        .into_iter()
        .map(|(inv_id, recs)| {
            let mean_weight =
                recs.iter().map(|r| r.rating.as_score()).sum::<f64>() / recs.len() as f64;
            let correction: Option<String> = recs
                .iter()
                .filter_map(|r| r.correction.as_deref())
                .next()
                .map(|s| s.to_string());
            WeightedTrainingHint {
                invocation_id: inv_id,
                weight: mean_weight,
                is_reinforcement: mean_weight >= 0.5,
                correction_text: correction,
                feedback_count: recs.len(),
            }
        })
        .collect();

    hints.sort_by(|a, b| {
        b.weight
            .partial_cmp(&a.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.invocation_id.cmp(&b.invocation_id))
    });
    hints
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::FeedbackCategory;

    fn rec(inv: &str, user: &str, rating: FeedbackRating, ts: u64) -> FeedbackRecord {
        FeedbackRecord::new(user, inv, rating, ts)
    }

    #[test]
    fn empty_records_produces_zero_report() {
        let report = QualityReport::generate(&[]);
        assert_eq!(report.total_feedback, 0);
        assert!(report.avg_stars.is_none());
        assert_eq!(report.avg_score, 0.0);
        assert!(report.satisfaction_rate().is_none());
    }

    #[test]
    fn all_thumbs_up_has_full_satisfaction() {
        let records = vec![
            rec("inv-1", "u1", FeedbackRating::ThumbsUp, 1),
            rec("inv-2", "u2", FeedbackRating::ThumbsUp, 2),
        ];
        let report = QualityReport::generate(&records);
        assert_eq!(report.positive_count, 2);
        assert_eq!(report.negative_count, 0);
        assert_eq!(report.satisfaction_rate(), Some(1.0));
        assert_eq!(report.satisfaction_pct(), Some(100));
    }

    #[test]
    fn all_thumbs_down_has_zero_satisfaction() {
        let records = vec![rec("inv-1", "u1", FeedbackRating::ThumbsDown, 1)];
        let report = QualityReport::generate(&records);
        assert_eq!(report.satisfaction_rate(), Some(0.0));
        assert_eq!(report.satisfaction_pct(), Some(0));
    }

    #[test]
    fn avg_stars_computed_correctly() {
        let records = vec![
            rec("inv-1", "u1", FeedbackRating::Stars(2), 1),
            rec("inv-2", "u2", FeedbackRating::Stars(4), 2),
        ];
        let report = QualityReport::generate(&records);
        assert!((report.avg_stars.unwrap() - 3.0).abs() < 1e-9);
    }

    #[test]
    fn category_counts_are_correct() {
        let r1 = FeedbackRecord::new("u1", "inv-1", FeedbackRating::ThumbsDown, 1)
            .with_categories(vec![FeedbackCategory::Wrong]);
        let r2 = FeedbackRecord::new("u2", "inv-2", FeedbackRating::ThumbsDown, 2)
            .with_categories(vec![FeedbackCategory::Wrong, FeedbackCategory::Incomplete]);
        let report = QualityReport::generate(&[r1, r2]);
        assert_eq!(report.category_counts["wrong"], 2);
        assert_eq!(report.category_counts["incomplete"], 1);
    }

    #[test]
    fn top_corrected_invocations_sorted_by_count() {
        let r1 = FeedbackRecord::new("u1", "inv-a", FeedbackRating::ThumbsDown, 1)
            .with_correction("fix 1");
        let r2 = FeedbackRecord::new("u2", "inv-a", FeedbackRating::ThumbsDown, 2)
            .with_correction("fix 2");
        let r3 = FeedbackRecord::new("u3", "inv-b", FeedbackRating::ThumbsDown, 3)
            .with_correction("fix 3");
        let report = QualityReport::generate(&[r1, r2, r3]);
        assert_eq!(report.top_corrected_invocations[0].0, "inv-a");
        assert_eq!(report.top_corrected_invocations[0].1, 2);
    }

    #[test]
    fn report_round_trips_through_json() {
        let records = vec![
            rec("inv-1", "u1", FeedbackRating::ThumbsUp, 1),
            rec("inv-2", "u2", FeedbackRating::Stars(3), 2),
        ];
        let report = QualityReport::generate(&records);
        let json = serde_json::to_string(&report).unwrap();
        let decoded: QualityReport = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.total_feedback, report.total_feedback);
    }

    #[test]
    fn build_training_hints_weights_correctly() {
        let records = vec![
            rec("inv-a", "u1", FeedbackRating::ThumbsUp, 1),
            rec("inv-a", "u2", FeedbackRating::ThumbsUp, 2),
            rec("inv-b", "u3", FeedbackRating::ThumbsDown, 3),
        ];
        let hints = build_training_hints(&records);
        assert_eq!(hints.len(), 2);
        // inv-a has weight 1.0 (all positive), inv-b has weight 0.0
        assert_eq!(hints[0].invocation_id, "inv-a");
        assert!(hints[0].is_reinforcement);
        assert!(!hints[1].is_reinforcement);
    }

    #[test]
    fn build_training_hints_carries_correction_text() {
        let r = FeedbackRecord::new("u1", "inv-x", FeedbackRating::ThumbsDown, 1)
            .with_correction("The correct answer is 42");
        let hints = build_training_hints(&[r]);
        assert!(hints[0].correction_text.is_some());
        assert_eq!(
            hints[0].correction_text.as_deref(),
            Some("The correct answer is 42")
        );
    }

    #[test]
    fn build_training_hints_sorted_descending_by_weight() {
        let records = vec![
            rec("inv-low", "u1", FeedbackRating::ThumbsDown, 1),
            rec("inv-high", "u2", FeedbackRating::ThumbsUp, 2),
            rec("inv-mid", "u3", FeedbackRating::Stars(3), 3),
        ];
        let hints = build_training_hints(&records);
        let weights: Vec<f64> = hints.iter().map(|h| h.weight).collect();
        for i in 0..weights.len() - 1 {
            assert!(weights[i] >= weights[i + 1]);
        }
    }
}
