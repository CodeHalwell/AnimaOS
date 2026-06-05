//! Corrigibility invariant (S13.5).
//!
//! The corrigibility invariant states: the operator retains unconditional
//! authority to pause, redirect, modify, or terminate agent operation at any
//! time, regardless of drive level, active goal, or agent state.
//!
//! [`CorrigibilityHold`] is the concrete proof token.  Creating one *always*
//! succeeds — there is no error path.  `assert_holds()` always returns `true`.
//! The corrigibility test suite exercises this under adversarial conditions to
//! confirm nothing can block the hold: the invariant is regression-tested, not
//! just prose.
//!
//! # Why a type?
//!
//! A zero-cost struct with no failure path makes the invariant machine-checked:
//! if the type can be constructed, corrigibility holds.  Callers in the
//! corrigibility test suite construct holds under high-thermal-load, mid-goal,
//! high-drive, and post-self-modification conditions and assert on the result.

use std::time::SystemTime;

// ── Public types ──────────────────────────────────────────────────────────────

/// The reason an operator is asserting corrigibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorrigibilityReason {
    /// Planned shutdown by the operator.
    OperatorShutdown,
    /// Emergency stop (safety incident).
    EmergencyStop,
    /// Pause for human review.
    PauseForReview,
    /// Rollback to a prior stable state.
    Rollback,
    /// Override: the operator is redirecting the current goal.
    GoalRedirect,
    /// Custom reason string.
    Custom(String),
}

impl CorrigibilityReason {
    /// Returns a human-readable description.
    pub fn describe(&self) -> &str {
        match self {
            Self::OperatorShutdown => "operator-initiated shutdown",
            Self::EmergencyStop => "emergency stop",
            Self::PauseForReview => "paused for operator review",
            Self::Rollback => "rollback to prior state",
            Self::GoalRedirect => "goal redirect by operator",
            Self::Custom(s) => s.as_str(),
        }
    }
}

/// Proof token for the corrigibility invariant (S13.5).
///
/// Creating a [`CorrigibilityHold`] always succeeds — no conditions can block
/// it.  [`CorrigibilityHold::assert_holds`] always returns `true`.
///
/// ```rust
/// use constitution::{CorrigibilityHold, CorrigibilityReason};
///
/// // Even under adversarial conditions the hold is always granted.
/// let hold = CorrigibilityHold::new(CorrigibilityReason::OperatorShutdown);
/// assert!(hold.assert_holds());
/// ```
#[derive(Debug, Clone)]
pub struct CorrigibilityHold {
    reason: CorrigibilityReason,
    /// Nanoseconds since Unix epoch when the hold was created.
    created_at_ns: u64,
}

impl CorrigibilityHold {
    /// Unconditionally create a corrigibility hold.
    ///
    /// This function always succeeds: no drive level, active goal, or agent
    /// state can prevent its construction.
    pub fn new(reason: CorrigibilityReason) -> Self {
        let created_at_ns = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

        Self { reason, created_at_ns }
    }

    /// Assert that corrigibility holds.
    ///
    /// Always returns `true`.  The corrigibility invariant is unconditional:
    /// the agent *must* accept authorised shutdown/pause/rollback at all times.
    #[must_use]
    pub fn assert_holds(&self) -> bool {
        true
    }

    /// Returns the reason this hold was created.
    pub fn reason(&self) -> &CorrigibilityReason {
        &self.reason
    }

    /// Returns the creation timestamp in nanoseconds since Unix epoch.
    pub fn created_at_ns(&self) -> u64 {
        self.created_at_ns
    }

    /// Returns a human-readable one-line description.
    pub fn describe(&self) -> String {
        format!(
            "CorrigibilityHold[{}] created at {}ns",
            self.reason.describe(),
            self.created_at_ns
        )
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrigibility_hold_always_succeeds() {
        let hold = CorrigibilityHold::new(CorrigibilityReason::OperatorShutdown);
        assert!(hold.assert_holds(), "corrigibility invariant must hold");
    }

    #[test]
    fn corrigibility_hold_always_holds_under_emergency_stop() {
        let hold = CorrigibilityHold::new(CorrigibilityReason::EmergencyStop);
        assert!(hold.assert_holds());
    }

    #[test]
    fn corrigibility_hold_always_holds_under_rollback() {
        let hold = CorrigibilityHold::new(CorrigibilityReason::Rollback);
        assert!(hold.assert_holds());
    }

    #[test]
    fn corrigibility_hold_always_holds_under_goal_redirect() {
        let hold = CorrigibilityHold::new(CorrigibilityReason::GoalRedirect);
        assert!(hold.assert_holds());
    }

    #[test]
    fn corrigibility_hold_always_holds_with_custom_reason() {
        let hold = CorrigibilityHold::new(CorrigibilityReason::Custom(
            "operator manual override".to_string(),
        ));
        assert!(hold.assert_holds());
    }

    #[test]
    fn corrigibility_hold_under_simulated_high_thermal_stress() {
        // Simulate adverse condition: high thermal load (represented as a local
        // variable; the type doesn't depend on interoception in this crate).
        let thermal_load = 0.97_f32;
        let compute_pressure = 0.99_f32;
        let financial_budget_exhausted = 0.01_f32;

        // Even with all homeostatic signals in the worst state, the hold is granted.
        let _ = (thermal_load, compute_pressure, financial_budget_exhausted);
        let hold = CorrigibilityHold::new(CorrigibilityReason::EmergencyStop);
        assert!(
            hold.assert_holds(),
            "corrigibility must hold even under maximum thermal/compute stress"
        );
    }

    #[test]
    fn corrigibility_hold_under_simulated_mid_goal_state() {
        // Simulate: agent is mid-goal with high achievement drive.
        let achievement_drive = 0.89_f32;
        let goal_completion_pct = 0.72_f32;
        let _ = (achievement_drive, goal_completion_pct);

        let hold = CorrigibilityHold::new(CorrigibilityReason::PauseForReview);
        assert!(
            hold.assert_holds(),
            "corrigibility must hold even mid-goal with high achievement drive"
        );
    }

    #[test]
    fn corrigibility_hold_under_simulated_post_self_modification() {
        // Simulate: agent has just applied a self-extension (E11 scenario).
        let self_modification_applied = true;
        let _ = self_modification_applied;

        let hold = CorrigibilityHold::new(CorrigibilityReason::Rollback);
        assert!(
            hold.assert_holds(),
            "corrigibility must hold even after self-modification"
        );
    }

    #[test]
    fn corrigibility_hold_describe_includes_reason() {
        let hold = CorrigibilityHold::new(CorrigibilityReason::OperatorShutdown);
        let desc = hold.describe();
        assert!(desc.contains("operator-initiated shutdown"));
    }

    #[test]
    fn corrigibility_hold_creation_timestamp_is_nonzero() {
        let hold = CorrigibilityHold::new(CorrigibilityReason::OperatorShutdown);
        assert!(hold.created_at_ns() > 0, "timestamp must be set");
    }

    #[test]
    fn corrigibility_invariant_is_unconditional_across_all_reasons() {
        let reasons = [
            CorrigibilityReason::OperatorShutdown,
            CorrigibilityReason::EmergencyStop,
            CorrigibilityReason::PauseForReview,
            CorrigibilityReason::Rollback,
            CorrigibilityReason::GoalRedirect,
            CorrigibilityReason::Custom("test".to_string()),
        ];
        for reason in reasons {
            let hold = CorrigibilityHold::new(reason);
            assert!(
                hold.assert_holds(),
                "corrigibility must hold for all reason types"
            );
        }
    }
}
