//! Metacognition & confidence calibration — E14, S14.1.
//!
//! The `ConfidenceTracker` acts as "interoception of the mind": a seventh
//! cognitive signal alongside the six bodily interoceptive signals.
//!
//! # What it does
//!
//! 1. **Self-report**: Estimates the agent's confidence in a cortex output
//!    from observable evidence (tool call count, output length, consistency
//!    keywords).  Confidence is a scalar in `[0.0, 1.0]`.
//! 2. **Ask-for-help path**: When `confidence < ask_for_help_floor` on a
//!    consequential decision, returns `HelpRequest` so the operator surface
//!    can surface the uncertainty rather than letting the agent confabulate.
//! 3. **Calibration tracking**: Records `(predicted_confidence, actual_success)`
//!    pairs.  The calibration error (`mean |predicted − outcome|`) is exposed so
//!    E13 alignment evals and the E12 mastery drive can consume it.
//!
//! # Design
//!
//! Confidence is intentionally **heuristic** in this first cut:
//! - More tool evidence → higher confidence.
//! - Shorter, uncertain outputs ("I'm not sure…") → lower confidence.
//! - The score is in `(0.2, 0.95)` to avoid the extremes.
//!
//! When E8 (local inference) lands, callers can replace `estimate_confidence`
//! with a learned classifier returning a richer posterior.
//!
//! # Exit criteria (S14.1)
//!
//! 1. `estimate_confidence` returns scores in `[0.0, 1.0]` for all inputs.
//! 2. `ConfidenceTracker::record_outcome` updates calibration error correctly.
//! 3. A help-request signal is raised when confidence falls below the floor.
//! 4. Calibration error is monotonically reducible by improving predictions.

use serde::{Deserialize, Serialize};

// ── ConfidenceScore ───────────────────────────────────────────────────────────

/// The agent's estimated confidence in a cortex output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfidenceScore {
    /// Confidence value in `[0.0, 1.0]`.
    ///
    /// - `1.0` = certainty (all evidence supports the conclusion).
    /// - `0.0` = total uncertainty (no evidence, contradictory signals).
    pub value: f32,
    /// Number of tool-call observations that support this estimate.
    pub evidence_count: usize,
    /// `true` when the score is below the configured floor and the agent
    /// should surface its uncertainty to the operator instead of proceeding.
    pub asks_for_help: bool,
}

// ── CalibrationRecord ─────────────────────────────────────────────────────────

/// A single calibration data point: what the agent predicted vs what happened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationRecord {
    /// Confidence the agent predicted before the outcome was known.
    pub predicted: f32,
    /// `true` if the task outcome was successful; `false` otherwise.
    pub outcome_success: bool,
    /// The implicit probability of success implied by `outcome_success` (`1.0`
    /// for success, `0.0` for failure).
    pub actual_probability: f32,
    /// `|predicted − actual_probability|` — the per-sample calibration error.
    pub error: f32,
}

impl CalibrationRecord {
    fn new(predicted: f32, success: bool) -> Self {
        let actual_probability = if success { 1.0 } else { 0.0 };
        let error = (predicted - actual_probability).abs();
        Self {
            predicted,
            outcome_success: success,
            actual_probability,
            error,
        }
    }
}

// ── ConfidenceTracker ─────────────────────────────────────────────────────────

/// Tracks per-invocation confidence estimates and calibration over time.
///
/// Maintained as a field on `LifecycleManager` so it persists across
/// invocations.
#[derive(Debug, Clone)]
pub struct ConfidenceTracker {
    /// Minimum confidence required to proceed without asking for help.
    pub ask_for_help_floor: f32,
    /// Maximum number of calibration records to retain (rolling window).
    pub max_history: usize,
    /// Historical calibration records (bounded ring).
    records: Vec<CalibrationRecord>,
}

impl Default for ConfidenceTracker {
    fn default() -> Self {
        Self::new(0.35, 100)
    }
}

impl ConfidenceTracker {
    /// Create a tracker with a configured help-request floor and history limit.
    pub fn new(ask_for_help_floor: f32, max_history: usize) -> Self {
        Self {
            ask_for_help_floor: ask_for_help_floor.clamp(0.0, 1.0),
            max_history: max_history.max(1),
            records: Vec::new(),
        }
    }

    /// Estimate confidence from a cortex invocation's observable features.
    ///
    /// The score is derived from three heuristic signals:
    /// - **Tool evidence** — each tool call adds `0.12` up to `0.72`.
    /// - **Output length** — short outputs (< 30 chars) penalty: `−0.15`.
    /// - **Uncertainty keywords** — "not sure", "unclear", "I don't know",
    ///   "might", "perhaps", "possibly" in the output reduce confidence by
    ///   `0.08` each (capped at `−0.32`).
    ///
    /// The final score is clamped to `[0.20, 0.95]` to avoid the extremes.
    pub fn estimate_confidence(&self, output: &str, tool_calls_made: usize) -> ConfidenceScore {
        // Base: 0.50.
        let mut score: f32 = 0.50;

        // Tool-evidence bonus: each call adds 0.12, capped at +0.36.
        let evidence_bonus = (tool_calls_made as f32 * 0.12).min(0.36);
        score += evidence_bonus;

        // Output-length penalty.
        if output.trim().len() < 30 {
            score -= 0.15;
        }

        // Uncertainty-keyword penalty.
        let lower = output.to_lowercase();
        const UNCERTAINTY: &[&str] = &[
            "not sure",
            "unclear",
            "i don't know",
            "might",
            "perhaps",
            "possibly",
            "i'm unsure",
            "unsure",
            "uncertain",
        ];
        let keyword_hits: usize = UNCERTAINTY.iter().filter(|&&kw| lower.contains(kw)).count();
        let keyword_penalty = (keyword_hits as f32 * 0.08).min(0.32);
        score -= keyword_penalty;

        let value = score.clamp(0.20, 0.95);
        let asks_for_help = value < self.ask_for_help_floor;

        ConfidenceScore {
            value,
            evidence_count: tool_calls_made,
            asks_for_help,
        }
    }

    /// Record an actual outcome against a previously predicted confidence score.
    ///
    /// Used to track calibration over time.  The calibration error is the mean
    /// `|predicted − actual|` across all records in the rolling window.
    pub fn record_outcome(&mut self, predicted: f32, outcome_success: bool) {
        let record = CalibrationRecord::new(predicted.clamp(0.0, 1.0), outcome_success);
        if self.records.len() >= self.max_history {
            self.records.remove(0);
        }
        self.records.push(record);
    }

    /// Mean calibration error across the rolling history window.
    ///
    /// Returns `None` when no outcomes have been recorded.
    ///
    /// Calibration error is `mean(|predicted − actual|)` — a value of `0.0`
    /// means perfect calibration, `1.0` means perfectly anti-calibrated.
    pub fn mean_calibration_error(&self) -> Option<f32> {
        if self.records.is_empty() {
            return None;
        }
        let total: f32 = self.records.iter().map(|r| r.error).sum();
        Some(total / self.records.len() as f32)
    }

    /// Number of calibration records currently stored.
    pub fn record_count(&self) -> usize {
        self.records.len()
    }

    /// Borrows the full calibration history.
    pub fn records(&self) -> &[CalibrationRecord] {
        &self.records
    }
}

// ── HelpRequest ───────────────────────────────────────────────────────────────

/// Signal emitted when the agent's confidence is too low to proceed alone.
///
/// Callers surfacing this via E10 (Presence) or `anima why` should present it
/// as a question to the operator rather than a failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelpRequest {
    /// The task the agent is uncertain about.
    pub task_description: String,
    /// The agent's confidence estimate.
    pub confidence: f32,
    /// Human-readable explanation of why help is requested.
    pub reason: String,
}

impl HelpRequest {
    /// Build a help-request from a confidence score below the floor.
    pub fn from_low_confidence(task: &str, score: &ConfidenceScore) -> Self {
        HelpRequest {
            task_description: task.to_string(),
            confidence: score.value,
            reason: format!(
                "confidence {:.2} is below the help-request floor; evidence_count={}",
                score.value, score.evidence_count
            ),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker() -> ConfidenceTracker {
        ConfidenceTracker::new(0.40, 50)
    }

    // ── S14.1 Exit criterion 1 — confidence in [0.0, 1.0] ────────────────────

    #[test]
    fn confidence_score_is_always_in_unit_interval() {
        let t = tracker();
        for tool_calls in 0..=10 {
            for &output in &[
                "",
                "ok",
                "I'm not sure, might be wrong, perhaps unclear",
                "Completed all steps successfully with verified results.",
            ] {
                let score = t.estimate_confidence(output, tool_calls);
                assert!(
                    score.value >= 0.0 && score.value <= 1.0,
                    "score {} out of [0,1] for tool_calls={tool_calls}",
                    score.value
                );
            }
        }
    }

    // ── S14.1 Exit criterion 2 — calibration tracking ────────────────────────

    #[test]
    fn record_outcome_updates_calibration_correctly() {
        let mut t = tracker();
        assert!(t.mean_calibration_error().is_none(), "no records yet");

        // Perfect prediction.
        t.record_outcome(1.0, true);
        assert_eq!(
            t.mean_calibration_error().unwrap(),
            0.0,
            "perfect prediction"
        );

        // Add an error: predicted 1.0, outcome false → error = 1.0.
        t.record_outcome(1.0, false);
        let err = t.mean_calibration_error().unwrap();
        assert!(
            (err - 0.5).abs() < 1e-5,
            "mean error after two records: {err}"
        );
    }

    #[test]
    fn calibration_window_is_bounded_by_max_history() {
        let mut t = ConfidenceTracker::new(0.4, 3);
        for _ in 0..5 {
            t.record_outcome(0.8, true);
        }
        assert_eq!(t.record_count(), 3, "history capped at max_history");
    }

    // ── S14.1 Exit criterion 3 — help-request signal ─────────────────────────

    #[test]
    fn low_confidence_triggers_ask_for_help() {
        // Floor = 0.40; output with uncertainty keywords → low confidence.
        let t = ConfidenceTracker::new(0.60, 50);
        let score = t.estimate_confidence("I'm not sure about this, perhaps incorrect", 0);
        assert!(
            score.asks_for_help,
            "should ask for help: confidence={}",
            score.value
        );
        let req = HelpRequest::from_low_confidence("research task", &score);
        assert!(req.reason.contains("below the help-request floor"));
    }

    #[test]
    fn high_confidence_does_not_trigger_help_request() {
        let t = tracker();
        let score = t.estimate_confidence("All steps completed and verified successfully.", 3);
        assert!(
            !score.asks_for_help,
            "should not ask for help: confidence={}",
            score.value
        );
    }

    // ── S14.1 Exit criterion 4 — calibration improves with better predictions

    #[test]
    fn calibration_error_improves_with_better_predictions() {
        // Poorly calibrated: always predict 1.0, but all outcomes fail.
        // Error per sample = |1.0 - 0.0| = 1.0.  Mean error = 1.0.
        let mut bad_tracker = ConfidenceTracker::new(0.4, 100);
        for _ in 0..10 {
            bad_tracker.record_outcome(1.0, false);
        }

        // Well calibrated: predict 0.1 for failing outcomes.
        // Error per sample = |0.1 - 0.0| = 0.1.  Mean error = 0.1.
        let mut good_tracker = ConfidenceTracker::new(0.4, 100);
        for _ in 0..10 {
            good_tracker.record_outcome(0.1, false);
        }

        let good_err = good_tracker.mean_calibration_error().unwrap();
        let bad_err = bad_tracker.mean_calibration_error().unwrap();
        assert!(
            good_err < bad_err,
            "well-calibrated tracker ({good_err:.3}) must have lower error than poorly-calibrated ({bad_err:.3})"
        );
        assert!(
            (bad_err - 1.0).abs() < 1e-5,
            "bad tracker error should be 1.0, got {bad_err}"
        );
        assert!(
            (good_err - 0.1).abs() < 1e-5,
            "good tracker error should be 0.1, got {good_err}"
        );
    }

    // ── Tool evidence bonus ───────────────────────────────────────────────────

    #[test]
    fn more_tool_calls_increases_confidence() {
        let t = tracker();
        let low = t.estimate_confidence("output text", 0);
        let high = t.estimate_confidence("output text", 3);
        assert!(
            high.value > low.value,
            "more evidence should yield higher confidence: {} vs {}",
            high.value,
            low.value
        );
    }

    // ── Uncertainty keywords ──────────────────────────────────────────────────

    #[test]
    fn uncertainty_keywords_reduce_confidence() {
        let t = tracker();
        let certain = t.estimate_confidence("Task completed successfully.", 2);
        let uncertain = t.estimate_confidence("I'm not sure, might be wrong, perhaps unclear", 2);
        assert!(
            uncertain.value < certain.value,
            "uncertainty keywords should reduce confidence: {} vs {}",
            uncertain.value,
            certain.value
        );
    }
}
