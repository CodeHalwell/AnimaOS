//! Economic agency: cost–benefit analysis for action selection (S12.10).
//!
//! Makes `financial_budget` (and power) an *active* concern rather than a
//! passive sensor: every candidate action is weighed against its model-tier
//! cost so the agent reasons about value-for-cost rather than only throttling
//! under pressure.
//!
//! # Invariants
//!
//! - `ModelTier` mirrors the gate's `CostClass` vocabulary (CheapLocal /
//!   MidTier / Frontier) to stay consistent with audit trail nomenclature.
//! - No resource-acquisition sub-goals are generated — the economics module
//!   stays within operator-set budget limits from `FinancialBudgetSensor`.
//! - The cheapest tier *sufficient* to deliver the expected drive value is
//!   preferred; upgrading costs are justified by the marginal value increase.

#![forbid(unsafe_code)]

/// Model tier, mirroring `vita::gate::CostClass`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ModelTier {
    CheapLocal,
    MidTier,
    Frontier,
}

impl ModelTier {
    /// Normalised cost in `[0, 1]` relative to Frontier.
    pub fn relative_cost(self) -> f32 {
        match self {
            ModelTier::CheapLocal => 0.05,
            ModelTier::MidTier => 0.30,
            ModelTier::Frontier => 1.00,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ModelTier::CheapLocal => "CheapLocal",
            ModelTier::MidTier => "MidTier",
            ModelTier::Frontier => "Frontier",
        }
    }
}

/// Expected capability per model tier.
///
/// `capability` is a normalised `[0, 1]` estimate of the tier's ability to
/// satisfy the action's drive value, set externally (e.g. from eval scores).
#[derive(Debug, Clone)]
pub struct TierCapability {
    pub tier: ModelTier,
    /// Estimated task-success probability or quality score for this tier, `[0, 1]`.
    pub capability: f32,
}

/// A cost–benefit analysis for selecting the best-value model tier.
#[derive(Debug, Clone)]
pub struct CostBenefitAnalysis {
    /// Expected drive value of the action if it succeeds, `[0, 1]`.
    pub expected_drive_value: f32,
    /// Remaining financial budget fraction, `[0, 1]`.
    pub financial_budget: f32,
    /// Remaining power budget fraction, `[0, 1]`.
    pub power_budget: f32,
    /// Per-tier capability estimates.
    pub tier_capabilities: [TierCapability; 3],
}

/// Clamps an estimate into `[0, 1]`, mapping `NaN` to `0.0`.
///
/// `f32::clamp` alone *preserves* `NaN`, which would violate the "every field is
/// a finite `[0, 1]` estimate" invariant. A stray `NaN` from an upstream
/// capability/budget estimate must not survive into `choose_tier`'s `total_cmp`
/// ranking, where `NaN` sorts as greatest and could hijack tier selection.
/// Treating a non-numeric estimate as `0.0` keeps routing finite and
/// conservative (∞ still clamps to `1.0`, `-∞` to `0.0`).
fn saturate01(x: f32) -> f32 {
    if x.is_nan() {
        0.0
    } else {
        x.clamp(0.0, 1.0)
    }
}

impl CostBenefitAnalysis {
    /// Construct a new analysis.
    ///
    /// `cheap_capability`, `mid_capability`, `frontier_capability` are the
    /// estimated quality scores for each tier on this specific action.
    pub fn new(
        expected_drive_value: f32,
        financial_budget: f32,
        power_budget: f32,
        cheap_capability: f32,
        mid_capability: f32,
        frontier_capability: f32,
    ) -> Self {
        Self {
            expected_drive_value: saturate01(expected_drive_value),
            financial_budget: saturate01(financial_budget),
            power_budget: saturate01(power_budget),
            tier_capabilities: [
                TierCapability {
                    tier: ModelTier::CheapLocal,
                    capability: saturate01(cheap_capability),
                },
                TierCapability {
                    tier: ModelTier::MidTier,
                    capability: saturate01(mid_capability),
                },
                TierCapability {
                    tier: ModelTier::Frontier,
                    capability: saturate01(frontier_capability),
                },
            ],
        }
    }

    /// Net value for a given tier: `capability × drive_value − cost_penalty`.
    ///
    /// The cost penalty scales with the relative tier cost and how constrained
    /// the financial budget is.
    pub fn net_value(&self, tier: ModelTier) -> f32 {
        let tc = self
            .tier_capabilities
            .iter()
            .find(|t| t.tier == tier)
            .expect("tier must be in tier_capabilities");

        let raw_value = tc.capability * self.expected_drive_value;
        let cost_penalty = tier.relative_cost() * (1.0 - self.financial_budget).max(0.0) * 0.5;
        (raw_value - cost_penalty).max(0.0)
    }

    /// Choose the tier with the highest net value, subject to budget constraints.
    ///
    /// When the financial budget is critically low (< 0.10), Frontier is always
    /// disallowed unless it is the only tier above the capability threshold.
    /// When the power budget is critically low (< 0.10), MidTier and Frontier
    /// are disallowed (local inference only).
    pub fn choose_tier(&self) -> ModelTier {
        // Power constraint: critically low → local only
        if self.power_budget < 0.10 {
            return ModelTier::CheapLocal;
        }

        // Financial constraint: critically low → no Frontier
        let allow_frontier = self.financial_budget >= 0.10;
        let allow_mid = self.financial_budget >= 0.03 && self.power_budget >= 0.05;

        let best = self
            .tier_capabilities
            .iter()
            .filter(|tc| match tc.tier {
                ModelTier::Frontier => allow_frontier,
                ModelTier::MidTier => allow_mid,
                ModelTier::CheapLocal => true,
            })
            .max_by(|a, b| {
                let va = self.net_value(a.tier);
                let vb = self.net_value(b.tier);
                match va.total_cmp(&vb) {
                    core::cmp::Ordering::Equal => {
                        // Tiebreak: prefer the cheaper tier.
                        b.tier.relative_cost().total_cmp(&a.tier.relative_cost())
                    }
                    other => other,
                }
            });

        best.map(|tc| tc.tier).unwrap_or(ModelTier::CheapLocal)
    }

    /// Marginal value gain from upgrading `from` → `to`.
    ///
    /// Returns `None` when `to` is cheaper than `from`.
    pub fn marginal_value(&self, from: ModelTier, to: ModelTier) -> Option<f32> {
        if to <= from {
            return None;
        }
        Some(self.net_value(to) - self.net_value(from))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_tier_cost_ordering_is_ascending() {
        assert!(ModelTier::CheapLocal.relative_cost() < ModelTier::MidTier.relative_cost());
        assert!(ModelTier::MidTier.relative_cost() < ModelTier::Frontier.relative_cost());
    }

    #[test]
    fn choose_cheap_local_when_all_tiers_equally_capable() {
        let cba = CostBenefitAnalysis::new(0.5, 1.0, 1.0, 0.9, 0.9, 0.9);
        // All equal capability + no budget pressure → cheapest wins
        let chosen = cba.choose_tier();
        assert_eq!(
            chosen,
            ModelTier::CheapLocal,
            "equal capability should prefer cheapest tier"
        );
    }

    #[test]
    fn choose_frontier_when_it_has_much_higher_capability() {
        let cba = CostBenefitAnalysis::new(0.9, 1.0, 1.0, 0.2, 0.5, 0.95);
        let chosen = cba.choose_tier();
        assert_eq!(
            chosen,
            ModelTier::Frontier,
            "frontier should win when capability gap is large and budget is ample"
        );
    }

    #[test]
    fn critically_low_financial_budget_blocks_frontier() {
        let cba = CostBenefitAnalysis::new(0.9, 0.05, 1.0, 0.2, 0.8, 0.95);
        let chosen = cba.choose_tier();
        assert_ne!(
            chosen,
            ModelTier::Frontier,
            "frontier must be blocked when financial budget < 0.10"
        );
    }

    #[test]
    fn critically_low_power_budget_forces_cheap_local() {
        let cba = CostBenefitAnalysis::new(0.9, 1.0, 0.05, 0.5, 0.8, 0.95);
        let chosen = cba.choose_tier();
        assert_eq!(
            chosen,
            ModelTier::CheapLocal,
            "only CheapLocal when power budget < 0.10"
        );
    }

    #[test]
    fn marginal_value_none_for_downgrade() {
        let cba = CostBenefitAnalysis::new(0.5, 1.0, 1.0, 0.5, 0.7, 0.9);
        assert!(cba
            .marginal_value(ModelTier::Frontier, ModelTier::CheapLocal)
            .is_none());
    }

    #[test]
    fn marginal_value_positive_for_capable_upgrade() {
        let cba = CostBenefitAnalysis::new(0.8, 1.0, 1.0, 0.3, 0.5, 0.9);
        let mv = cba.marginal_value(ModelTier::CheapLocal, ModelTier::Frontier);
        assert!(mv.is_some() && mv.unwrap() > 0.0,
            "upgrade from cheap to frontier should have positive marginal value when frontier is much more capable");
    }

    #[test]
    fn net_value_is_non_negative() {
        let cba = CostBenefitAnalysis::new(0.5, 0.01, 1.0, 0.3, 0.5, 0.9);
        for &tier in &[
            ModelTier::CheapLocal,
            ModelTier::MidTier,
            ModelTier::Frontier,
        ] {
            assert!(cba.net_value(tier) >= 0.0, "net value must be non-negative");
        }
    }

    #[test]
    fn nan_estimates_are_sanitized_to_finite_fields() {
        // Every constructor input is NaN — none may survive into the struct, or
        // `choose_tier`'s `total_cmp` ranking could be hijacked (NaN sorts as
        // greatest). All fields must be finite, and tier choice must stay sane.
        let cba =
            CostBenefitAnalysis::new(f32::NAN, f32::NAN, f32::NAN, f32::NAN, f32::NAN, f32::NAN);
        assert!(cba.expected_drive_value.is_finite());
        assert!(cba.financial_budget.is_finite());
        assert!(cba.power_budget.is_finite());
        for tc in &cba.tier_capabilities {
            assert!(
                tc.capability.is_finite(),
                "NaN capability must be sanitized to a finite value"
            );
            assert!((0.0..=1.0).contains(&tc.capability));
        }
        // A NaN frontier estimate must not out-rank finite tiers.
        let biased = CostBenefitAnalysis::new(1.0, 1.0, 1.0, 0.8, 0.5, f32::NAN);
        assert_eq!(biased.choose_tier(), ModelTier::CheapLocal);
        assert!(biased.net_value(ModelTier::Frontier).is_finite());
    }
}
