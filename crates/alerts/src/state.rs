//! Alert state machine: `Normal → Firing → Resolved`.

use serde::{Deserialize, Serialize};

// ── AlertState ────────────────────────────────────────────────────────────────

/// Current state of a single alert rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertState {
    /// The condition has never fired or has resolved.
    Normal,
    /// The condition is currently firing.
    Firing,
}

impl AlertState {
    /// Returns `true` when in the `Firing` state.
    pub fn is_firing(self) -> bool {
        self == AlertState::Firing
    }
}

// ── AlertStateTracker ─────────────────────────────────────────────────────────

/// Tracks the state of a single alert rule across evaluation passes.
///
/// Suppresses duplicate `AlertFired` events (if a rule was already firing,
/// successive evaluations that still meet the condition do NOT generate a new
/// `AlertFired` event) and synthesises an `AlertResolved` event when a
/// previously firing rule stops meeting its condition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertStateTracker {
    /// The rule ID this tracker belongs to.
    pub rule_id: String,
    /// Current state.
    pub state: AlertState,
    /// How many consecutive evaluations the alert has been in `Firing` state.
    pub consecutive_firing: u32,
    /// Total number of times this rule has transitioned `Normal → Firing`.
    pub total_fires: u64,
}

/// The transition produced by a single evaluation pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateTransition {
    /// Rule just became active (was Normal, now Firing).
    NewlyFiring,
    /// Rule is still firing (no new event should be emitted).
    StillFiring,
    /// Rule just cleared (was Firing, now Normal).
    Resolved,
    /// Rule was and remains Normal (no event).
    StillNormal,
}

impl AlertStateTracker {
    /// Create a new tracker in the `Normal` state.
    pub fn new(rule_id: impl Into<String>) -> Self {
        Self {
            rule_id: rule_id.into(),
            state: AlertState::Normal,
            consecutive_firing: 0,
            total_fires: 0,
        }
    }

    /// Advance the state machine given whether the rule's condition fires.
    ///
    /// Returns the [`StateTransition`] that occurred.
    pub fn advance(&mut self, condition_fires: bool) -> StateTransition {
        match (self.state, condition_fires) {
            (AlertState::Normal, true) => {
                self.state = AlertState::Firing;
                self.consecutive_firing = 1;
                self.total_fires += 1;
                StateTransition::NewlyFiring
            }
            (AlertState::Firing, true) => {
                self.consecutive_firing += 1;
                StateTransition::StillFiring
            }
            (AlertState::Firing, false) => {
                self.state = AlertState::Normal;
                self.consecutive_firing = 0;
                StateTransition::Resolved
            }
            (AlertState::Normal, false) => StateTransition::StillNormal,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_to_firing_transition() {
        let mut t = AlertStateTracker::new("r1");
        assert_eq!(t.advance(true), StateTransition::NewlyFiring);
        assert_eq!(t.state, AlertState::Firing);
        assert_eq!(t.total_fires, 1);
    }

    #[test]
    fn still_firing_does_not_increment_total_fires() {
        let mut t = AlertStateTracker::new("r1");
        t.advance(true);
        assert_eq!(t.advance(true), StateTransition::StillFiring);
        assert_eq!(t.total_fires, 1);
        assert_eq!(t.consecutive_firing, 2);
    }

    #[test]
    fn firing_to_resolved_transition() {
        let mut t = AlertStateTracker::new("r1");
        t.advance(true);
        assert_eq!(t.advance(false), StateTransition::Resolved);
        assert_eq!(t.state, AlertState::Normal);
        assert_eq!(t.consecutive_firing, 0);
    }

    #[test]
    fn normal_to_still_normal() {
        let mut t = AlertStateTracker::new("r1");
        assert_eq!(t.advance(false), StateTransition::StillNormal);
        assert_eq!(t.state, AlertState::Normal);
    }

    #[test]
    fn multiple_fire_resolve_cycles_count_correctly() {
        let mut t = AlertStateTracker::new("r1");
        t.advance(true); // fires 1st time
        t.advance(false); // resolves
        t.advance(true); // fires 2nd time
        t.advance(false); // resolves
        assert_eq!(t.total_fires, 2);
        assert_eq!(t.state, AlertState::Normal);
    }

    #[test]
    fn consecutive_firing_resets_on_resolve() {
        let mut t = AlertStateTracker::new("r1");
        t.advance(true);
        t.advance(true);
        t.advance(true);
        assert_eq!(t.consecutive_firing, 3);
        t.advance(false);
        assert_eq!(t.consecutive_firing, 0);
    }

    #[test]
    fn state_tracker_json_round_trips() {
        let mut t = AlertStateTracker::new("json-test");
        t.advance(true);
        let json = serde_json::to_string(&t).unwrap();
        let recovered: AlertStateTracker = serde_json::from_str(&json).unwrap();
        assert_eq!(t, recovered);
    }
}
