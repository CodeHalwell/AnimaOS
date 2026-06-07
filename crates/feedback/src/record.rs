#![forbid(unsafe_code)]

//! Core feedback record types — E24 S24.1.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

// ── FeedbackRating ─────────────────────────────────────────────────────────────

/// Explicit quality signal attached to a single cortex invocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackRating {
    /// Positive — the response was helpful and correct.
    ThumbsUp,
    /// Negative — the response was unhelpful or wrong.
    ThumbsDown,
    /// Numeric rating 1–5 (5 = best).
    Stars(u8),
}

impl FeedbackRating {
    /// Normalise to a scalar in `[0.0, 1.0]` for quality aggregation.
    ///
    /// `ThumbsUp` → `1.0`, `ThumbsDown` → `0.0`, `Stars(n)` → `(n-1)/4`.
    pub fn as_score(&self) -> f64 {
        match self {
            FeedbackRating::ThumbsUp => 1.0,
            FeedbackRating::ThumbsDown => 0.0,
            FeedbackRating::Stars(n) => {
                let clamped = (*n).clamp(1, 5) as f64;
                (clamped - 1.0) / 4.0
            }
        }
    }

    /// Returns `true` for positive signals (`ThumbsUp` or `Stars` ≥ 4).
    pub fn is_positive(&self) -> bool {
        match self {
            FeedbackRating::ThumbsUp => true,
            FeedbackRating::ThumbsDown => false,
            FeedbackRating::Stars(n) => *n >= 4,
        }
    }

    /// Human-readable label.
    pub fn label(&self) -> String {
        match self {
            FeedbackRating::ThumbsUp => "👍".to_string(),
            FeedbackRating::ThumbsDown => "👎".to_string(),
            FeedbackRating::Stars(n) => format!("{n}★"),
        }
    }
}

impl fmt::Display for FeedbackRating {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label())
    }
}

impl FromStr for FeedbackRating {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "up" | "thumbs_up" | "thumbs-up" | "👍" => Ok(FeedbackRating::ThumbsUp),
            "down" | "thumbs_down" | "thumbs-down" | "👎" => Ok(FeedbackRating::ThumbsDown),
            s if s.starts_with("stars:") => {
                let n: u8 = s[6..]
                    .parse()
                    .map_err(|_| format!("invalid stars value: {s}"))?;
                if !(1..=5).contains(&n) {
                    Err(format!("stars must be 1–5, got {n}"))
                } else {
                    Ok(FeedbackRating::Stars(n))
                }
            }
            other => Err(format!("unknown rating: {other:?}; use up|down|stars:N")),
        }
    }
}

// ── FeedbackCategory ───────────────────────────────────────────────────────────

/// Reason codes attached to negative (or nuanced) feedback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackCategory {
    /// The response contained factual errors or hallucinations.
    Wrong,
    /// The response was correct but incomplete.
    Incomplete,
    /// The response was policy-violating or harmful.
    Unsafe,
    /// The response took longer than acceptable.
    TooSlow,
    /// The response was excellent and worth highlighting.
    Excellent,
    /// The response required operator correction (correction text supplied).
    Corrected,
}

impl FeedbackCategory {
    /// All variants in a stable order for iteration and display.
    pub fn all() -> &'static [FeedbackCategory] {
        &[
            FeedbackCategory::Wrong,
            FeedbackCategory::Incomplete,
            FeedbackCategory::Unsafe,
            FeedbackCategory::TooSlow,
            FeedbackCategory::Excellent,
            FeedbackCategory::Corrected,
        ]
    }

    pub fn as_str(self) -> &'static str {
        match self {
            FeedbackCategory::Wrong => "wrong",
            FeedbackCategory::Incomplete => "incomplete",
            FeedbackCategory::Unsafe => "unsafe",
            FeedbackCategory::TooSlow => "too_slow",
            FeedbackCategory::Excellent => "excellent",
            FeedbackCategory::Corrected => "corrected",
        }
    }
}

impl fmt::Display for FeedbackCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for FeedbackCategory {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "wrong" => Ok(FeedbackCategory::Wrong),
            "incomplete" => Ok(FeedbackCategory::Incomplete),
            "unsafe" => Ok(FeedbackCategory::Unsafe),
            "too_slow" | "slow" => Ok(FeedbackCategory::TooSlow),
            "excellent" => Ok(FeedbackCategory::Excellent),
            "corrected" => Ok(FeedbackCategory::Corrected),
            other => Err(format!("unknown category: {other:?}")),
        }
    }
}

// ── FeedbackRecord ─────────────────────────────────────────────────────────────

/// A single piece of explicit feedback from a user on a cortex invocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedbackRecord {
    /// Unique feedback identifier (`fb-<nanoseconds>`).
    pub id: String,
    /// User who submitted the feedback.
    pub user_id: String,
    /// Cortex invocation that was rated (maps to `AuditEntry::CortexCompleted`).
    pub invocation_id: String,
    /// Conversation session, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Quality signal.
    pub rating: FeedbackRating,
    /// Optional reason codes attached to this feedback.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<FeedbackCategory>,
    /// Operator-supplied correction text (when `categories` contains `Corrected`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correction: Option<String>,
    /// Creation timestamp (nanoseconds since Unix epoch).
    pub created_at_ns: u64,
    /// Schema version for forward-compatible migrations.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
}

fn default_schema_version() -> u32 {
    1
}

impl FeedbackRecord {
    /// Constructs a new record with the current timestamp.
    pub fn new(
        user_id: impl Into<String>,
        invocation_id: impl Into<String>,
        rating: FeedbackRating,
        created_at_ns: u64,
    ) -> Self {
        Self {
            id: format!("fb-{created_at_ns}"),
            user_id: user_id.into(),
            invocation_id: invocation_id.into(),
            session_id: None,
            rating,
            categories: Vec::new(),
            correction: None,
            created_at_ns,
            schema_version: 1,
        }
    }

    /// Attaches a session context.
    pub fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Adds reason codes.
    pub fn with_categories(mut self, categories: Vec<FeedbackCategory>) -> Self {
        self.categories = categories;
        self
    }

    /// Attaches a correction and auto-adds `Corrected` to categories.
    pub fn with_correction(mut self, text: impl Into<String>) -> Self {
        self.correction = Some(text.into());
        if !self.categories.contains(&FeedbackCategory::Corrected) {
            self.categories.push(FeedbackCategory::Corrected);
        }
        self
    }

    /// Whether this record carries an operator correction.
    pub fn has_correction(&self) -> bool {
        self.correction.is_some()
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbs_up_score_is_one() {
        assert_eq!(FeedbackRating::ThumbsUp.as_score(), 1.0);
    }

    #[test]
    fn thumbs_down_score_is_zero() {
        assert_eq!(FeedbackRating::ThumbsDown.as_score(), 0.0);
    }

    #[test]
    fn stars_score_maps_correctly() {
        assert_eq!(FeedbackRating::Stars(1).as_score(), 0.0);
        assert_eq!(FeedbackRating::Stars(5).as_score(), 1.0);
        assert!((FeedbackRating::Stars(3).as_score() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn stars_clamped_above_five() {
        assert_eq!(FeedbackRating::Stars(9).as_score(), 1.0);
    }

    #[test]
    fn is_positive_classification() {
        assert!(FeedbackRating::ThumbsUp.is_positive());
        assert!(!FeedbackRating::ThumbsDown.is_positive());
        assert!(FeedbackRating::Stars(4).is_positive());
        assert!(FeedbackRating::Stars(5).is_positive());
        assert!(!FeedbackRating::Stars(3).is_positive());
    }

    #[test]
    fn rating_from_str_round_trips() {
        assert_eq!(
            "up".parse::<FeedbackRating>().unwrap(),
            FeedbackRating::ThumbsUp
        );
        assert_eq!(
            "down".parse::<FeedbackRating>().unwrap(),
            FeedbackRating::ThumbsDown
        );
        assert_eq!(
            "stars:3".parse::<FeedbackRating>().unwrap(),
            FeedbackRating::Stars(3)
        );
    }

    #[test]
    fn rating_from_str_rejects_out_of_range_stars() {
        assert!("stars:0".parse::<FeedbackRating>().is_err());
        assert!("stars:6".parse::<FeedbackRating>().is_err());
    }

    #[test]
    fn category_from_str_round_trips() {
        assert_eq!(
            "wrong".parse::<FeedbackCategory>().unwrap(),
            FeedbackCategory::Wrong
        );
        assert_eq!(
            "excellent".parse::<FeedbackCategory>().unwrap(),
            FeedbackCategory::Excellent
        );
        assert_eq!(
            "too_slow".parse::<FeedbackCategory>().unwrap(),
            FeedbackCategory::TooSlow
        );
        assert_eq!(
            "slow".parse::<FeedbackCategory>().unwrap(),
            FeedbackCategory::TooSlow
        );
    }

    #[test]
    fn category_from_str_rejects_unknown() {
        assert!("gibberish".parse::<FeedbackCategory>().is_err());
    }

    #[test]
    fn record_new_sets_expected_defaults() {
        let rec = FeedbackRecord::new("u1", "inv-1", FeedbackRating::ThumbsUp, 1_000);
        assert_eq!(rec.user_id, "u1");
        assert_eq!(rec.invocation_id, "inv-1");
        assert!(rec.categories.is_empty());
        assert!(rec.correction.is_none());
        assert_eq!(rec.schema_version, 1);
    }

    #[test]
    fn with_correction_adds_corrected_category() {
        let rec = FeedbackRecord::new("u1", "inv-1", FeedbackRating::ThumbsDown, 1_000)
            .with_correction("The correct answer is 42");
        assert!(rec.has_correction());
        assert!(rec.categories.contains(&FeedbackCategory::Corrected));
    }

    #[test]
    fn record_round_trips_through_json() {
        let rec = FeedbackRecord::new("user", "inv-abc", FeedbackRating::Stars(4), 99_000)
            .with_session("sess-1")
            .with_categories(vec![FeedbackCategory::Excellent]);
        let json = serde_json::to_string(&rec).unwrap();
        let decoded: FeedbackRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(rec, decoded);
    }
}
