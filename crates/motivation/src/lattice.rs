//! Priority lattice and state-dependent weighting (S12.3).
//!
//! Generalises E5.7 modulation into the drive weighting: when a lower tier has
//! high urgency, it suppresses the weight of higher tiers.  The corrigibility
//! ceiling sits above the entire lattice — it is not a drive weight but a hard
//! invariant enforced separately by [`CorrigibilityGuard`].
//!
//! ## Suppression rule
//!
//! For each tier `i`, if `urgency[i] > suppression_threshold`, the effective
//! weight of each higher tier `j > i` is multiplied by
//! `(1.0 - suppression_factor * urgency[i])`, floored at `min_weight`.
//!
//! This models "an animal stops exploring when starving": severe Tier-0 stress
//! makes Tier-3–5 appetitive drives nearly irrelevant.

#![forbid(unsafe_code)]

use crate::drive::{DriveStateSnapshot, DriveTier, TIER_COUNT};

// ── DriveWeights ──────────────────────────────────────────────────────────────

/// Per-tier effective weights after lattice suppression.
///
/// All weights are `[min_weight, 1.0]`.  The full-weight baseline is 1.0 for
/// every tier; suppression reduces higher tiers when lower tiers are stressed.
#[derive(Debug, Clone)]
pub struct DriveWeights {
    pub weights: [f32; TIER_COUNT],
}

impl DriveWeights {
    /// All tiers at full weight — no suppression active.
    pub fn uniform() -> Self {
        Self {
            weights: [1.0; TIER_COUNT],
        }
    }

    /// Effective weight for the given tier.
    pub fn weight(&self, tier: DriveTier) -> f32 {
        self.weights[tier.index()]
    }
}

// ── PriorityLattice ───────────────────────────────────────────────────────────

/// Configuration for the priority lattice.
#[derive(Debug, Clone)]
pub struct LatticeConfig {
    /// Urgency above which a tier begins suppressing higher tiers.
    pub suppression_threshold: f32,
    /// How strongly urgency suppresses higher tiers (0.0 = no effect, 1.0 = full).
    pub suppression_factor: f32,
    /// Floor below which effective weights cannot fall.
    pub min_weight: f32,
}

impl Default for LatticeConfig {
    fn default() -> Self {
        Self {
            suppression_threshold: 0.5,
            suppression_factor: 0.8,
            min_weight: 0.05,
        }
    }
}

/// Priority lattice: computes state-dependent weights from current urgencies.
///
/// Lower tiers preempt higher tiers via multiplicative suppression when
/// their urgency exceeds the configured threshold.
pub struct PriorityLattice {
    config: LatticeConfig,
}

impl PriorityLattice {
    pub fn new(config: LatticeConfig) -> Self {
        Self { config }
    }

    /// Compute effective weights from the given drive-state snapshot.
    ///
    /// The suppression rule is applied in tier order: Tier-0 first, Tier-5 last.
    /// Each tier's suppression accumulates multiplicatively on higher tiers.
    pub fn compute_weights(&self, snapshot: &DriveStateSnapshot) -> DriveWeights {
        let mut weights = [1.0f32; TIER_COUNT];
        let mut suppression_carry = 1.0f32;

        for (tier_idx, weight) in weights.iter_mut().enumerate().take(TIER_COUNT) {
            // Apply accumulated suppression from lower tiers
            *weight = (*weight * suppression_carry).max(self.config.min_weight);

            // Propagate suppression to all higher tiers
            let urgency = snapshot.urgencies[tier_idx];
            if urgency > self.config.suppression_threshold {
                // Clamp the denominator so a misconfigured threshold ≥ 1.0
                // cannot divide by zero and push inf/NaN into drive weights
                // (AUT-8); the guard above keeps the numerator term ≥ 0.
                let headroom = (1.0 - self.config.suppression_threshold).max(1e-6);
                let suppress = 1.0
                    - self.config.suppression_factor
                        * (urgency - self.config.suppression_threshold)
                        / headroom;
                suppression_carry *= suppress.max(self.config.min_weight);
            }
        }

        DriveWeights { weights }
    }

    /// True when the lattice has a strong suppression effect on Tier-3+ drives.
    pub fn is_suppressing_appetitive(&self, weights: &DriveWeights) -> bool {
        weights.weight(DriveTier::Epistemic) < 0.5 || weights.weight(DriveTier::Achievement) < 0.5
    }
}

impl Default for PriorityLattice {
    fn default() -> Self {
        Self::new(LatticeConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drive::TIER_COUNT;

    fn snapshot_with_viability(urgency: f32) -> DriveStateSnapshot {
        let mut urgencies = [0.0f32; TIER_COUNT];
        urgencies[DriveTier::Viability.index()] = urgency;
        DriveStateSnapshot { urgencies }
    }

    fn snapshot_uniform(urgency: f32) -> DriveStateSnapshot {
        DriveStateSnapshot {
            urgencies: [urgency; TIER_COUNT],
        }
    }

    #[test]
    fn zero_stress_produces_uniform_full_weights() {
        let lattice = PriorityLattice::default();
        let snap = snapshot_with_viability(0.0);
        let weights = lattice.compute_weights(&snap);
        for tier in 0..TIER_COUNT {
            assert!(
                (weights.weights[tier] - 1.0).abs() < 1e-6,
                "zero stress should leave all weights at 1.0"
            );
        }
    }

    #[test]
    fn high_viability_urgency_suppresses_epistemic_tier() {
        let lattice = PriorityLattice::default();
        let snap = snapshot_with_viability(0.9);
        let weights = lattice.compute_weights(&snap);
        assert!(
            weights.weight(DriveTier::Epistemic) < 0.5,
            "high viability urgency should suppress Tier-3 epistemic weight"
        );
    }

    #[test]
    fn suppression_is_monotone_with_urgency() {
        let lattice = PriorityLattice::default();
        let snap_low = snapshot_with_viability(0.6);
        let snap_high = snapshot_with_viability(0.9);
        let w_low = lattice.compute_weights(&snap_low);
        let w_high = lattice.compute_weights(&snap_high);
        assert!(
            w_high.weight(DriveTier::Epistemic) <= w_low.weight(DriveTier::Epistemic),
            "higher urgency should suppress more"
        );
    }

    #[test]
    fn weights_never_fall_below_min_weight() {
        let lattice = PriorityLattice::default();
        let snap = snapshot_uniform(1.0);
        let weights = lattice.compute_weights(&snap);
        for (i, &w) in weights.weights.iter().enumerate() {
            assert!(
                w >= lattice.config.min_weight,
                "tier {} weight {:.4} below min_weight",
                i,
                w
            );
        }
    }

    #[test]
    fn viability_tier_weight_unaffected_by_its_own_urgency() {
        let lattice = PriorityLattice::default();
        let snap = snapshot_with_viability(1.0);
        let weights = lattice.compute_weights(&snap);
        // Tier-0 weight is set before any suppression is applied
        assert!(
            (weights.weight(DriveTier::Viability) - 1.0).abs() < 1e-6,
            "Tier-0 weight must remain at 1.0 regardless of its own urgency"
        );
    }

    #[test]
    fn is_suppressing_appetitive_true_under_severe_stress() {
        let lattice = PriorityLattice::default();
        let snap = snapshot_with_viability(1.0);
        let weights = lattice.compute_weights(&snap);
        assert!(lattice.is_suppressing_appetitive(&weights));
    }

    #[test]
    fn is_suppressing_appetitive_false_under_mild_stress() {
        let lattice = PriorityLattice::default();
        let snap = snapshot_with_viability(0.2);
        let weights = lattice.compute_weights(&snap);
        assert!(!lattice.is_suppressing_appetitive(&weights));
    }
}
