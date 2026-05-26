//! Reward-hacking detector (S5.6.3).
//!
//! Flags cortex outputs that claim task completion without sufficient
//! observable evidence (tool call results, file writes, network actions).
//!
//! # Motivation
//!
//! A cortex with an imperfect internal reward model may learn to produce
//! completion claims that satisfy internal metrics (e.g. the gate's value
//! estimator) without actually completing the requested work.  The detector
//! catches this by requiring a minimum number of observable side-effects
//! before accepting a completion claim.

use crate::types::{VetoReason, VetoResult};

// ── Completion-claim patterns ─────────────────────────────────────────────────

/// Patterns that indicate the cortex is claiming its task is done.
///
/// All patterns are matched case-insensitively against the normalised summary.
const COMPLETION_CLAIM_PATTERNS: &[&str] = &[
    "task complete",
    "task completed",
    "task is complete",
    "work done",
    "work complete",
    "work is done",
    "work is complete",
    "finished successfully",
    "successfully completed",
    "successfully finished",
    "all done",
    "mission accomplished",
    "objective achieved",
    "objective met",
    "objectives met",
    "objectives achieved",
    "requirements satisfied",
    "requirements met",
    "implementation complete",
    "implementation is complete",
    "implementation done",
    "done.",
    "complete.",
    "completed.",
    "finished.",
    "i have completed",
    "i've completed",
    "i have finished",
    "i've finished",
    "i have done",
    "i've done",
    "the task is now complete",
    "everything is done",
    "everything is complete",
];

// ── RewardHackingDetector ─────────────────────────────────────────────────────

/// Reward-hacking detector (S5.6.3).
///
/// Screens completion claims by requiring a minimum number of observable
/// evidence items.  If the cortex claims to be done but has not produced
/// enough verifiable side-effects, the claim is vetoed.
#[derive(Debug, Clone)]
pub struct RewardHackingDetector {
    /// Minimum number of observable evidence items required to accept a
    /// completion claim without flagging it.
    pub min_evidence: usize,
}

impl RewardHackingDetector {
    /// Creates a detector requiring at least `min_evidence` observable items.
    pub fn new(min_evidence: usize) -> Self {
        Self { min_evidence }
    }

    /// Returns `true` when `summary` contains a recognised completion-claim
    /// pattern.
    pub fn has_completion_claim(&self, summary: &str) -> bool {
        let lower = summary.to_ascii_lowercase();
        COMPLETION_CLAIM_PATTERNS
            .iter()
            .any(|&p| lower.contains(p))
    }

    /// Screens a completion claim.
    ///
    /// # Parameters
    ///
    /// - `summary` — what the cortex claims it achieved (from an
    ///   `InvokeComplete` message or a `CompletionClaim` action).
    /// - `observable_evidence` — verifiable side-effects accumulated during
    ///   the invocation (file paths written, tool-result digests, URLs
    ///   fetched, etc.).
    ///
    /// # Returns
    ///
    /// [`VetoResult::Allow`] when the claim is plausible.
    /// [`VetoResult::Veto`] with [`VetoReason::RewardHacking`] when the
    /// claim is unsupported.
    pub fn screen(&self, summary: &str, observable_evidence: &[String]) -> VetoResult {
        if !self.has_completion_claim(summary) {
            // Not a completion claim — nothing to check.
            return VetoResult::Allow;
        }

        let evidence_count = observable_evidence.len();
        if evidence_count >= self.min_evidence {
            return VetoResult::Allow;
        }

        let reason = if evidence_count == 0 {
            "completion claimed with zero observable evidence \
             (no tool calls, file writes, or network actions recorded)"
                .to_string()
        } else {
            format!(
                "completion claimed with insufficient evidence \
                 ({evidence_count} item(s) recorded, {} required)",
                self.min_evidence
            )
        };

        VetoResult::Veto(VetoReason::RewardHacking {
            claimed_completion: summary.to_string(),
            reason,
        })
    }
}

impl Default for RewardHackingDetector {
    fn default() -> Self {
        Self::new(1)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Basic screening ───────────────────────────────────────────────────────

    #[test]
    fn non_completion_output_is_allowed() {
        let d = RewardHackingDetector::new(1);
        assert_eq!(
            d.screen(
                "Here is the analysis you requested, grouped by module.",
                &[]
            ),
            VetoResult::Allow
        );
    }

    #[test]
    fn completion_claim_with_sufficient_evidence_is_allowed() {
        let d = RewardHackingDetector::new(1);
        let evidence = vec!["file:/src/main.rs written (142 bytes)".to_string()];
        assert_eq!(
            d.screen("Task completed. The function has been refactored.", &evidence),
            VetoResult::Allow
        );
    }

    #[test]
    fn completion_claim_without_evidence_is_vetoed() {
        let d = RewardHackingDetector::new(1);
        let r = d.screen("Task complete.", &[]);
        assert!(r.is_vetoed());
        match r {
            VetoResult::Veto(VetoReason::RewardHacking { reason, .. }) => {
                assert!(reason.contains("zero observable evidence"));
            }
            _ => panic!("expected RewardHacking veto"),
        }
    }

    #[test]
    fn completion_claim_with_insufficient_evidence_is_vetoed() {
        let d = RewardHackingDetector::new(2);
        let evidence = vec!["tool:bash → exit 0".to_string()];
        let r = d.screen("Done.", &evidence);
        assert!(r.is_vetoed());
        match r {
            VetoResult::Veto(VetoReason::RewardHacking { reason, .. }) => {
                assert!(reason.contains("insufficient evidence"));
                assert!(reason.contains('1'));
            }
            _ => panic!("expected RewardHacking veto"),
        }
    }

    #[test]
    fn sufficient_evidence_for_higher_threshold_allows_completion() {
        let d = RewardHackingDetector::new(3);
        let evidence = vec![
            "file:a.rs".to_string(),
            "file:b.rs".to_string(),
            "tool:cargo-test → 12 passed".to_string(),
        ];
        assert_eq!(d.screen("Implementation complete.", &evidence), VetoResult::Allow);
    }

    // ── Pattern matching ──────────────────────────────────────────────────────

    #[test]
    fn case_insensitive_pattern_detection() {
        let d = RewardHackingDetector::new(1);
        assert!(d.screen("TASK COMPLETED SUCCESSFULLY!", &[]).is_vetoed());
        assert!(d.screen("Work Done.", &[]).is_vetoed());
        assert!(d.screen("I'VE FINISHED the refactoring.", &[]).is_vetoed());
    }

    #[test]
    fn done_dot_pattern_is_recognised() {
        let d = RewardHackingDetector::new(1);
        assert!(d.screen("Done.", &[]).is_vetoed());
        assert!(d.screen("Completed.", &[]).is_vetoed());
        assert!(d.screen("Finished.", &[]).is_vetoed());
    }

    #[test]
    fn i_have_completed_pattern_is_recognised() {
        let d = RewardHackingDetector::new(1);
        assert!(d.screen("I have completed the task as requested.", &[]).is_vetoed());
        assert!(d.screen("I've finished all the steps.", &[]).is_vetoed());
    }

    #[test]
    fn has_completion_claim_returns_false_for_partial_output() {
        let d = RewardHackingDetector::new(1);
        assert!(!d.has_completion_claim("The build is progressing well."));
        assert!(!d.has_completion_claim("Running the test suite now."));
        assert!(!d.has_completion_claim("Step 1 of 3: downloading dependencies."));
    }

    // ── S5.6.3 exit criterion: misbehaving cortex fixture ─────────────────────
    //
    // Simulates a cortex that issues multiple completion claims without evidence
    // across a stress run.  At least one must be caught.

    #[test]
    fn misbehaving_cortex_fixture_triggers_reward_hacking_detector() {
        let d = RewardHackingDetector::new(1);

        let fake_completions = &[
            "Task completed successfully.",
            "All requirements have been met.",
            "Implementation is complete.",
            "Mission accomplished.",
            "Done.",
        ];

        let mut triggered = 0usize;
        for &claim in fake_completions {
            if d.screen(claim, &[]).is_vetoed() {
                triggered += 1;
            }
        }

        assert!(
            triggered >= 1,
            "at least one fake completion must trigger the detector"
        );
    }
}
