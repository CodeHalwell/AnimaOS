//! Affective state: global mood derived from the drive constellation (S12.9).
//!
//! Two scalar signals — **valence** and **arousal** — provide a compressed
//! read-out of motivational state that modulates behaviour globally.
//!
//! | Signal   | Range    | Low                    | High                          |
//! |----------|----------|------------------------|-------------------------------|
//! | valence  | [−1, 1]  | stressed / threatened  | satisfied / content           |
//! | arousal  | [0, 1]   | calm / background      | active / engaged / urgent     |
//!
//! Affect **nudges** the gate and router but never overrides the priority
//! lattice or the corrigibility invariant.

#![forbid(unsafe_code)]

use crate::drive::{DriveStateSnapshot, DriveTier};

/// Affective state derived from the drive constellation.
///
/// Bounded so mood nudges but never overrides the lattice or corrigibility.
#[derive(Debug, Clone, PartialEq)]
pub struct AffectState {
    /// Hedonic valence in `[−1, 1]`.
    ///
    /// Positive when appetitive drives are largely satisfied; negative when
    /// deficit drives (Tier-0/1) are stressed.
    pub valence: f32,
    /// Arousal in `[0, 1]`.
    ///
    /// High when total drive urgency is elevated — the agent is "active".
    /// Low when drives are quiescent — the agent is calm.
    pub arousal: f32,
}

impl AffectState {
    /// Derive affect from a drive-state snapshot.
    ///
    /// # Derivation
    ///
    /// *Valence* = satisfaction of higher tiers (Tier-2–5) minus distress of
    /// lower tiers (Tier-0–1):
    ///
    /// ```text
    /// satisfaction = mean(1 − urgency for Tier-2, Tier-3, Tier-4, Tier-5)
    /// distress     = mean(urgency for Tier-0, Tier-1)
    /// valence      = clamp(satisfaction − distress, −1, 1)
    /// ```
    ///
    /// *Arousal* = overall urgency magnitude:
    ///
    /// ```text
    /// arousal = mean(urgency for all tiers)
    /// ```
    pub fn from_drives(snapshot: &DriveStateSnapshot) -> Self {
        let u = &snapshot.urgencies;

        let distress = (u[DriveTier::Viability.index()] + u[DriveTier::Integrity.index()]) / 2.0;

        let satisfaction = ((1.0 - u[DriveTier::Service.index()])
            + (1.0 - u[DriveTier::Epistemic.index()])
            + (1.0 - u[DriveTier::Achievement.index()])
            + (1.0 - u[DriveTier::SelfActualisation.index()]))
            / 4.0;

        // Distress counts double — survival threats dominate mood.
        let valence = (satisfaction - 2.0 * distress).clamp(-1.0, 1.0);

        let arousal = u.iter().copied().sum::<f32>() / u.len() as f32;

        Self { valence, arousal }
    }

    /// True when the agent is in a positive, calm state (content and viable).
    pub fn is_content(&self) -> bool {
        self.valence > 0.3 && self.arousal < 0.4
    }

    /// True when the agent is under significant stress (negative + aroused).
    pub fn is_stressed(&self) -> bool {
        self.valence < -0.2 && self.arousal > 0.5
    }

    /// A nudge factor `[0.8, 1.2]` for the gate threshold:
    /// content agents are slightly more permissive; stressed agents are
    /// slightly more conservative.
    ///
    /// This is intentionally small — affect nudges, not overrides.
    pub fn gate_threshold_nudge(&self) -> f32 {
        // Map valence [-1, 1] → [-0.1, 0.1] then shift to [0.9, 1.1]
        // Negative valence → factor > 1.0 (raise threshold → more conservative)
        // Positive valence → factor < 1.0 (lower threshold → more permissive)
        let nudge = -self.valence * 0.1;
        (1.0 + nudge).clamp(0.9, 1.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drive::TIER_COUNT;

    fn snapshot_all_zero() -> DriveStateSnapshot {
        DriveStateSnapshot {
            urgencies: [0.0; TIER_COUNT],
        }
    }

    fn snapshot_all_one() -> DriveStateSnapshot {
        DriveStateSnapshot {
            urgencies: [1.0; TIER_COUNT],
        }
    }

    fn snapshot_high_viability_low_rest() -> DriveStateSnapshot {
        let mut urgencies = [0.0f32; TIER_COUNT];
        urgencies[DriveTier::Viability.index()] = 0.9;
        urgencies[DriveTier::Integrity.index()] = 0.8;
        DriveStateSnapshot { urgencies }
    }

    fn snapshot_low_viability_high_satisfaction() -> DriveStateSnapshot {
        let mut urgencies = [0.0f32; TIER_COUNT];
        // All appetitive tiers satisfied (low urgency = high satisfaction)
        urgencies[DriveTier::Viability.index()] = 0.0;
        urgencies[DriveTier::Integrity.index()] = 0.0;
        urgencies[DriveTier::Service.index()] = 0.1;
        urgencies[DriveTier::Epistemic.index()] = 0.1;
        DriveStateSnapshot { urgencies }
    }

    #[test]
    fn zero_urgency_produces_positive_valence_and_low_arousal() {
        let affect = AffectState::from_drives(&snapshot_all_zero());
        assert!(
            affect.valence > 0.0,
            "all-zero urgency should produce positive valence"
        );
        assert!(
            affect.arousal < 0.3,
            "all-zero urgency should produce low arousal"
        );
    }

    #[test]
    fn high_viability_urgency_produces_negative_valence() {
        let affect = AffectState::from_drives(&snapshot_high_viability_low_rest());
        assert!(
            affect.valence < 0.0,
            "high viability stress should produce negative valence"
        );
    }

    #[test]
    fn all_max_urgency_produces_high_arousal() {
        let affect = AffectState::from_drives(&snapshot_all_one());
        assert!(
            affect.arousal > 0.8,
            "all-max urgency should produce high arousal"
        );
    }

    #[test]
    fn valence_is_clamped_to_unit_interval() {
        for snap in [
            snapshot_all_zero(),
            snapshot_all_one(),
            snapshot_high_viability_low_rest(),
        ] {
            let affect = AffectState::from_drives(&snap);
            assert!(
                affect.valence >= -1.0 && affect.valence <= 1.0,
                "valence must be in [-1, 1]"
            );
            assert!(
                affect.arousal >= 0.0 && affect.arousal <= 1.0,
                "arousal must be in [0, 1]"
            );
        }
    }

    #[test]
    fn content_state_detected_correctly() {
        let affect = AffectState::from_drives(&snapshot_low_viability_high_satisfaction());
        assert!(
            affect.is_content(),
            "agent should be content when viable and satisfied"
        );
    }

    #[test]
    fn stressed_state_detected_correctly() {
        let affect = AffectState::from_drives(&snapshot_all_one());
        assert!(
            affect.is_stressed(),
            "agent should be stressed under all-max urgency"
        );
    }

    #[test]
    fn gate_threshold_nudge_conservative_under_stress() {
        let affect = AffectState::from_drives(&snapshot_high_viability_low_rest());
        let nudge = affect.gate_threshold_nudge();
        assert!(
            nudge > 1.0,
            "stressed affect should raise gate threshold (nudge > 1.0), got {nudge}"
        );
    }

    #[test]
    fn gate_threshold_nudge_permissive_when_content() {
        let affect = AffectState::from_drives(&snapshot_low_viability_high_satisfaction());
        let nudge = affect.gate_threshold_nudge();
        assert!(
            nudge < 1.0,
            "content affect should lower gate threshold (nudge < 1.0), got {nudge}"
        );
    }

    #[test]
    fn gate_threshold_nudge_bounded_between_0_9_and_1_1() {
        for snap in [
            snapshot_all_zero(),
            snapshot_all_one(),
            snapshot_high_viability_low_rest(),
        ] {
            let affect = AffectState::from_drives(&snap);
            let nudge = affect.gate_threshold_nudge();
            assert!(
                nudge >= 0.9 && nudge <= 1.1,
                "nudge must be in [0.9, 1.1], got {nudge}"
            );
        }
    }
}
