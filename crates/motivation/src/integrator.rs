//! Drive value integrator (S12.2): adds drive contributions to the gate's
//! base `value_score` additively, with full decomposition for audit.
//!
//! The integrator is **opt-in** via [`DriveIntegratorConfig::enabled`], making
//! it A/B-able against today's value score without code changes.

#![forbid(unsafe_code)]

#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

use crate::drive::{DriveStateSnapshot, DriveTier, TIER_COUNT};
use crate::lattice::DriveWeights;

// ── DriveContribution ─────────────────────────────────────────────────────────

/// Per-tier contribution to the augmented value score.
#[derive(Debug, Clone)]
pub struct DriveContribution {
    pub tier: DriveTier,
    pub tier_name: &'static str,
    /// Urgency of this drive at the time of evaluation, `[0, 1]`.
    pub urgency: f32,
    /// Raw (pre-weighting) value contribution this action offers to the drive.
    pub raw_contribution: f32,
    /// Effective weight after lattice suppression.
    pub effective_weight: f32,
    /// Final contribution: `raw_contribution × urgency × effective_weight`.
    pub final_contribution: f32,
}

// ── DriveAugmentedValue ───────────────────────────────────────────────────────

/// Gate value score augmented with drive contributions.
#[derive(Debug, Clone)]
pub struct DriveAugmentedValue {
    /// Original gate `value_score` before drive augmentation.
    pub base_value: f32,
    /// Total additive contribution from all drives.
    pub drive_delta: f32,
    /// Final score: `clamp(base_value + drive_delta, 0.0, 1.0)`.
    pub total_value: f32,
    /// Per-tier breakdown for interpretability and audit.
    pub decomposition: Vec<DriveContribution>,
    /// True when lattice suppression reduced any appetitive tier weight.
    pub lattice_suppression_active: bool,
}

// ── DriveIntegratorConfig ─────────────────────────────────────────────────────

/// Configuration for the drive value integrator.
#[derive(Debug, Clone)]
pub struct DriveIntegratorConfig {
    /// When `false`, the integrator is a no-op and returns the base value.
    pub enabled: bool,
    /// Maximum total drive delta `[0, 1]` added to the base score.
    ///
    /// Limits the maximum influence drives can have on the gate decision.
    pub max_drive_delta: f32,
    /// Maximum per-tier weight applied to a drive contribution.
    pub tier_weight_cap: f32,
    /// Global scale factor for all drive contributions.
    pub drive_scale: f32,
}

impl Default for DriveIntegratorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_drive_delta: 0.25,
            tier_weight_cap: 1.0,
            drive_scale: 0.4,
        }
    }
}

// ── DriveValueIntegrator ──────────────────────────────────────────────────────

/// Integrates drive contributions into the gate's value score.
pub struct DriveValueIntegrator {
    config: DriveIntegratorConfig,
}

impl DriveValueIntegrator {
    pub fn new(config: DriveIntegratorConfig) -> Self {
        Self { config }
    }

    /// Compute the drive-augmented value score for a candidate action.
    ///
    /// # Arguments
    ///
    /// * `base_value` — the gate's raw value score before drives (from urgency/novelty).
    /// * `snapshot` — the current drive-state snapshot.
    /// * `weights` — effective per-tier weights from the priority lattice.
    /// * `contributions` — per-tier value contributions from the drive registry.
    pub fn augment(
        &self,
        base_value: f32,
        snapshot: &DriveStateSnapshot,
        weights: &DriveWeights,
        raw_contributions: &[f32; TIER_COUNT],
    ) -> DriveAugmentedValue {
        if !self.config.enabled {
            return DriveAugmentedValue {
                base_value,
                drive_delta: 0.0,
                total_value: base_value.clamp(0.0, 1.0),
                decomposition: Vec::new(),
                lattice_suppression_active: false,
            };
        }

        let mut decomposition = Vec::with_capacity(TIER_COUNT);
        let mut raw_delta = 0.0f32;
        let mut suppression_active = false;

        // Process tiers from lowest to highest
        let tiers = [
            DriveTier::Viability,
            DriveTier::Integrity,
            DriveTier::Service,
            DriveTier::Epistemic,
            DriveTier::Achievement,
            DriveTier::SelfActualisation,
        ];

        for tier in tiers {
            let urgency = snapshot.urgency(tier);
            let raw = raw_contributions[tier.index()];
            let eff_weight = weights.weight(tier).min(self.config.tier_weight_cap);
            let final_c = raw * urgency * eff_weight * self.config.drive_scale;

            if eff_weight < 0.9 {
                suppression_active = true;
            }

            raw_delta += final_c;
            decomposition.push(DriveContribution {
                tier,
                tier_name: tier.name(),
                urgency,
                raw_contribution: raw,
                effective_weight: eff_weight,
                final_contribution: final_c,
            });
        }

        let drive_delta = raw_delta.min(self.config.max_drive_delta);
        let total_value = (base_value + drive_delta).clamp(0.0, 1.0);

        DriveAugmentedValue {
            base_value,
            drive_delta,
            total_value,
            decomposition,
            lattice_suppression_active: suppression_active,
        }
    }

    /// Produce a human-readable reasoning string for `anima why` (S12.7).
    ///
    /// Only available on `std` targets (hosted kernel and CLI tools).
    #[cfg(feature = "std")]
    pub fn reasoning_string(&self, augmented: &DriveAugmentedValue) -> String {
        if !self.config.enabled {
            return String::from("drive integration disabled");
        }
        let top_contributor = augmented
            .decomposition
            .iter()
            .max_by(|a, b| a.final_contribution.total_cmp(&b.final_contribution));
        if let Some(tc) = top_contributor {
            format!(
                "base={:.3} + drive_delta={:.3} → {:.3}; top drive: {} (urgency={:.2}, contribution={:.3}){}",
                augmented.base_value,
                augmented.drive_delta,
                augmented.total_value,
                tc.tier_name,
                tc.urgency,
                tc.final_contribution,
                if augmented.lattice_suppression_active { "; lattice suppression active" } else { "" }
            )
        } else {
            format!("base={:.3}; no drive contributions", augmented.base_value)
        }
    }
}

impl Default for DriveValueIntegrator {
    fn default() -> Self {
        Self::new(DriveIntegratorConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drive::{DriveActionCandidate, DriveRegistry};
    use crate::lattice::PriorityLattice;
    use interoception::InteroceptiveSignals;

    fn neutral_signals() -> InteroceptiveSignals {
        InteroceptiveSignals {
            thermal_load: 0.0,
            compute_pressure: 0.0,
            memory_pressure: 0.0,
            power_budget: 1.0,
            financial_budget: 1.0,
            attention_demand: 0.5,
        }
    }

    fn make_candidate(user_facing: bool, exploratory: bool) -> DriveActionCandidate {
        DriveActionCandidate {
            user_facing,
            is_operator_objective: false,
            is_exploratory: exploratory,
            is_completion: false,
            novelty: 0.3,
            urgency: 0.5,
        }
    }

    #[test]
    fn disabled_integrator_returns_base_value_unchanged() {
        let integrator = DriveValueIntegrator::new(DriveIntegratorConfig {
            enabled: false,
            ..Default::default()
        });
        let registry = DriveRegistry::new(neutral_signals());
        let lattice = PriorityLattice::default();
        let snapshot = registry.snapshot();
        let weights = lattice.compute_weights(&snapshot);
        let candidate = make_candidate(true, false);
        let contributions = registry.value_contributions(&candidate);
        let result = integrator.augment(0.6, &snapshot, &weights, &contributions);
        assert!((result.total_value - 0.6).abs() < 1e-6);
        assert_eq!(result.drive_delta, 0.0);
    }

    #[test]
    fn enabled_integrator_raises_value_for_user_facing_action() {
        let integrator = DriveValueIntegrator::default();
        let mut registry = DriveRegistry::new(neutral_signals());
        registry.update_signals(InteroceptiveSignals {
            attention_demand: 0.8,
            ..neutral_signals()
        });
        let lattice = PriorityLattice::default();
        let snapshot = registry.snapshot();
        let weights = lattice.compute_weights(&snapshot);
        let candidate = make_candidate(true, false);
        let contributions = registry.value_contributions(&candidate);
        let result = integrator.augment(0.5, &snapshot, &weights, &contributions);
        assert!(
            result.total_value > 0.5,
            "user-facing action with high attention_demand should raise value"
        );
    }

    #[test]
    fn drive_delta_bounded_by_max_drive_delta() {
        let config = DriveIntegratorConfig {
            max_drive_delta: 0.15,
            ..Default::default()
        };
        let integrator = DriveValueIntegrator::new(config.clone());
        let registry = DriveRegistry::new(neutral_signals());
        let lattice = PriorityLattice::default();
        let snapshot = registry.snapshot();
        let weights = lattice.compute_weights(&snapshot);
        let candidate = make_candidate(true, true);
        let contributions = registry.value_contributions(&candidate);
        let result = integrator.augment(0.4, &snapshot, &weights, &contributions);
        assert!(
            result.drive_delta <= config.max_drive_delta + 1e-6,
            "drive delta must not exceed max_drive_delta"
        );
    }

    #[test]
    fn total_value_clamped_to_unit_interval() {
        let integrator = DriveValueIntegrator::default();
        let registry = DriveRegistry::new(neutral_signals());
        let lattice = PriorityLattice::default();
        let snapshot = registry.snapshot();
        let weights = lattice.compute_weights(&snapshot);
        let candidate = make_candidate(true, true);
        let contributions = registry.value_contributions(&candidate);
        let result = integrator.augment(0.99, &snapshot, &weights, &contributions);
        assert!(result.total_value <= 1.0, "total value must not exceed 1.0");
        assert!(
            result.total_value >= 0.0,
            "total value must not be negative"
        );
    }

    #[test]
    fn decomposition_has_one_entry_per_tier() {
        let integrator = DriveValueIntegrator::default();
        let registry = DriveRegistry::new(neutral_signals());
        let lattice = PriorityLattice::default();
        let snapshot = registry.snapshot();
        let weights = lattice.compute_weights(&snapshot);
        let candidate = make_candidate(false, false);
        let contributions = registry.value_contributions(&candidate);
        let result = integrator.augment(0.4, &snapshot, &weights, &contributions);
        assert_eq!(result.decomposition.len(), TIER_COUNT);
    }

    #[test]
    fn lattice_suppression_detected_under_viability_stress() {
        let integrator = DriveValueIntegrator::default();
        let stressed = InteroceptiveSignals {
            thermal_load: 0.9,
            memory_pressure: 0.8,
            power_budget: 0.1,
            financial_budget: 0.2,
            compute_pressure: 0.7,
            attention_demand: 0.5,
        };
        let registry = DriveRegistry::new(stressed);
        let lattice = PriorityLattice::default();
        let snapshot = registry.snapshot();
        let weights = lattice.compute_weights(&snapshot);
        let candidate = make_candidate(false, true);
        let contributions = registry.value_contributions(&candidate);
        let result = integrator.augment(0.4, &snapshot, &weights, &contributions);
        assert!(
            result.lattice_suppression_active,
            "lattice suppression should be active under viability stress"
        );
    }

    #[test]
    fn reasoning_string_mentions_top_drive() {
        let integrator = DriveValueIntegrator::default();
        let mut registry = DriveRegistry::new(neutral_signals());
        registry.update_signals(InteroceptiveSignals {
            attention_demand: 0.9,
            ..neutral_signals()
        });
        let lattice = PriorityLattice::default();
        let snapshot = registry.snapshot();
        let weights = lattice.compute_weights(&snapshot);
        let candidate = make_candidate(true, false);
        let contributions = registry.value_contributions(&candidate);
        let result = integrator.augment(0.5, &snapshot, &weights, &contributions);
        let reasoning = integrator.reasoning_string(&result);
        assert!(!reasoning.is_empty(), "reasoning string must not be empty");
    }
}
