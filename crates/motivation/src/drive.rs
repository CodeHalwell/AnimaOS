//! Drive model & registry (S12.1 + S12.4).
//!
//! Six tiers: Viability → Integrity → Service → Epistemic → Achievement →
//! SelfActualisation.  Tier-0 drives wrap the existing interoceptive signals;
//! Tier-3 drives (curiosity, mastery) maintain internal satiation state.

#![forbid(unsafe_code)]

use interoception::InteroceptiveSignals;

// ── Tier ──────────────────────────────────────────────────────────────────────

/// Six-tier drive hierarchy from lowest (most overriding) to highest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum DriveTier {
    /// Tier 0: energy, thermal, compute/memory headroom, financial solvency.
    Viability = 0,
    /// Tier 1: structural integrity, identity coherence, security.
    Integrity = 1,
    /// Tier 2: be useful to the operator, attend to the human, fulfil objectives.
    Service = 2,
    /// Tier 3: curiosity (info gain) and mastery (competence improvement).
    Epistemic = 3,
    /// Tier 4: goal completion, progress, bounded achievement.
    Achievement = 4,
    /// Tier 5: long-horizon coherent self-narrative, value alignment.
    SelfActualisation = 5,
}

impl DriveTier {
    /// Human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            DriveTier::Viability => "Viability",
            DriveTier::Integrity => "Integrity",
            DriveTier::Service => "Service",
            DriveTier::Epistemic => "Epistemic",
            DriveTier::Achievement => "Achievement",
            DriveTier::SelfActualisation => "SelfActualisation",
        }
    }

    /// Ordinal index into per-tier arrays.
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// Number of drive tiers.
pub const TIER_COUNT: usize = 6;

// ── DriveActionCandidate ──────────────────────────────────────────────────────

/// Features of a candidate action from the drive system's perspective.
///
/// Passed to the drive registry to compute per-tier value contributions.
#[derive(Debug, Clone)]
pub struct DriveActionCandidate {
    /// Is this action visible to or requested by the human user?
    pub user_facing: bool,
    /// Is this action directly related to an active operator objective?
    pub is_operator_objective: bool,
    /// Is this action exploratory or novel (drives curiosity/mastery)?
    pub is_exploratory: bool,
    /// Does this action directly contribute to an active goal's completion?
    pub is_completion: bool,
    /// Novelty score `[0, 1]` — reuses the gate's event novelty field.
    pub novelty: f32,
    /// Urgency score `[0, 1]` — reuses the gate's event urgency field.
    pub urgency: f32,
}

// ── DriveStateSnapshot ───────────────────────────────────────────────────────

/// A point-in-time snapshot of urgency levels for all six tiers.
///
/// All urgencies are `[0, 1]` — `1.0` = maximum urgency/appetite.
#[derive(Debug, Clone)]
pub struct DriveStateSnapshot {
    /// Urgencies indexed by `DriveTier as usize`.
    pub urgencies: [f32; TIER_COUNT],
}

impl DriveStateSnapshot {
    /// Urgency for the given tier.
    pub fn urgency(&self, tier: DriveTier) -> f32 {
        self.urgencies[tier.index()]
    }

    /// True when any Tier-0 or Tier-1 drive urgency exceeds `threshold`.
    pub fn under_survival_stress(&self, threshold: f32) -> bool {
        self.urgencies[DriveTier::Viability.index()] > threshold
            || self.urgencies[DriveTier::Integrity.index()] > threshold
    }
}

// ── Internal epistemic state (S12.4) ─────────────────────────────────────────

/// Internal state for the curiosity drive (Tier-3).
///
/// Tracks recent novelty exposure and applies satiation with diminishing
/// returns to resist Goodharting.
#[derive(Debug, Clone)]
pub struct CuriosityState {
    /// Exponentially decayed recent-novelty accumulator `[0, 1]`.
    recent_novelty: f32,
    /// Decay factor per tick (< 1.0 → faster decay → faster re-satiation).
    decay: f32,
    /// Satiation ceiling: urgency is capped at `1.0 - satiation_floor`.
    satiation_floor: f32,
}

impl CuriosityState {
    pub fn new(decay: f32, satiation_floor: f32) -> Self {
        Self {
            recent_novelty: 0.0,
            decay,
            satiation_floor: satiation_floor.clamp(0.0, 0.9),
        }
    }

    /// Expose the agent to a novel stimulus and update satiation.
    pub fn observe(&mut self, novelty: f32) {
        self.recent_novelty = (self.recent_novelty + novelty).min(1.0);
    }

    /// Decay satiation by one tick (called once per scheduler cycle).
    pub fn tick(&mut self) {
        self.recent_novelty *= self.decay;
    }

    /// Curiosity urgency: high when little novelty has been seen recently.
    pub fn urgency(&self) -> f32 {
        // Urgency is inversely proportional to recent novelty exposure
        let base = 1.0 - self.recent_novelty;
        // Apply satiation floor so it never fully reaches 1.0
        (base - self.satiation_floor).max(0.0)
    }
}

impl Default for CuriosityState {
    fn default() -> Self {
        Self::new(0.85, 0.1)
    }
}

/// Internal state for the mastery drive (Tier-3).
///
/// Tracks measured competence gains on recurring task classes and applies
/// satiation via diminishing returns.
#[derive(Debug, Clone)]
pub struct MasteryState {
    /// Running estimate of competence `[0, 1]`.
    competence: f32,
    /// Urgency to improve when competence is below the aspiration level.
    aspiration: f32,
}

impl MasteryState {
    pub fn new(aspiration: f32) -> Self {
        Self {
            competence: 0.0,
            aspiration: aspiration.clamp(0.0, 1.0),
        }
    }

    /// Record a competence observation (e.g. from eval harness scores).
    pub fn record_outcome(&mut self, success_rate: f32) {
        // Exponential moving average with weight 0.2
        self.competence = self.competence * 0.8 + success_rate * 0.2;
    }

    /// Mastery urgency: proportional to the gap between aspiration and current competence.
    pub fn urgency(&self) -> f32 {
        (self.aspiration - self.competence).max(0.0)
    }
}

impl Default for MasteryState {
    fn default() -> Self {
        Self::new(0.8)
    }
}

// ── DriveRegistry ─────────────────────────────────────────────────────────────

/// Configuration for the drive registry.
#[derive(Debug, Clone)]
pub struct DriveRegistryConfig {
    /// Urgency threshold above which Tier-0 viability is considered stressed.
    pub viability_stress_threshold: f32,
    /// Urgency threshold above which Tier-1 integrity is considered stressed.
    pub integrity_stress_threshold: f32,
    /// Weight of attention_demand in the Tier-2 service urgency.
    pub service_attention_weight: f32,
    /// Weight of pending operator objectives in Tier-2 urgency.
    pub service_objective_weight: f32,
}

impl Default for DriveRegistryConfig {
    fn default() -> Self {
        Self {
            viability_stress_threshold: 0.6,
            integrity_stress_threshold: 0.7,
            service_attention_weight: 0.6,
            service_objective_weight: 0.4,
        }
    }
}

/// Registry holding all six tiers of drives and computing urgency snapshots.
///
/// Tier-0 wraps the existing interoceptive signals (no new sensing).
/// Tier-3 maintains internal satiation state (curiosity + mastery).
pub struct DriveRegistry {
    signals: InteroceptiveSignals,
    curiosity: CuriosityState,
    mastery: MasteryState,
    /// Pending operator-endorsed objectives (Tier-2/5 input, S12.6).
    pending_objectives: u32,
    /// Active goals needing completion (Tier-4 input).
    active_goal_count: u32,
    config: DriveRegistryConfig,
}

impl DriveRegistry {
    pub fn new(signals: InteroceptiveSignals) -> Self {
        Self {
            signals,
            curiosity: CuriosityState::default(),
            mastery: MasteryState::default(),
            pending_objectives: 0,
            active_goal_count: 0,
            config: DriveRegistryConfig::default(),
        }
    }

    pub fn with_config(mut self, config: DriveRegistryConfig) -> Self {
        self.config = config;
        self
    }

    /// Update the interoceptive signal snapshot (call at 1 Hz).
    pub fn update_signals(&mut self, signals: InteroceptiveSignals) {
        self.signals = signals;
    }

    /// Notify the curiosity drive about a novelty observation.
    pub fn observe_novelty(&mut self, novelty: f32) {
        self.curiosity.observe(novelty);
    }

    /// Advance curiosity satiation decay by one tick.
    pub fn tick(&mut self) {
        self.curiosity.tick();
    }

    /// Record a task outcome to update mastery competence.
    pub fn record_task_outcome(&mut self, success_rate: f32) {
        self.mastery.record_outcome(success_rate);
    }

    /// Set the number of pending operator objectives for Tier-2.
    pub fn set_pending_objectives(&mut self, count: u32) {
        self.pending_objectives = count;
    }

    /// Set the number of active goals needing completion for Tier-4.
    pub fn set_active_goal_count(&mut self, count: u32) {
        self.active_goal_count = count;
    }

    /// Compute the current drive-state snapshot from all signal sources.
    pub fn snapshot(&self) -> DriveStateSnapshot {
        let mut urgencies = [0.0f32; TIER_COUNT];

        // Tier-0: Viability — aggregate of the four sub-drives.
        // deficit = 1 - available; power/financial are "budget" (high = good).
        let thermal_deficit = self.signals.thermal_load;
        let memory_deficit = self.signals.memory_pressure;
        let power_deficit = (1.0 - self.signals.power_budget).max(0.0);
        let financial_deficit = (1.0 - self.signals.financial_budget).max(0.0);
        let viability_urgency =
            (thermal_deficit + memory_deficit + power_deficit + financial_deficit) / 4.0;
        urgencies[DriveTier::Viability.index()] = viability_urgency.clamp(0.0, 1.0);

        // Tier-1: Integrity — simplified: elevated when viability or
        // compute_pressure exceeds threshold (identity coherence at risk).
        let integrity_urgency =
            (self.signals.compute_pressure * 0.5 + viability_urgency * 0.5).clamp(0.0, 1.0);
        urgencies[DriveTier::Integrity.index()] = integrity_urgency * 0.6;

        // Tier-2: Service — driven by user attention + pending objectives.
        let attention_component =
            self.signals.attention_demand * self.config.service_attention_weight;
        let objective_component = if self.pending_objectives > 0 {
            (self.pending_objectives as f32 / 5.0).min(1.0) * self.config.service_objective_weight
        } else {
            0.0
        };
        urgencies[DriveTier::Service.index()] =
            (attention_component + objective_component).clamp(0.0, 1.0);

        // Tier-3: Epistemic — curiosity + mastery (both satiating).
        let epistemic_urgency =
            (self.curiosity.urgency() * 0.6 + self.mastery.urgency() * 0.4).clamp(0.0, 1.0);
        urgencies[DriveTier::Epistemic.index()] = epistemic_urgency;

        // Tier-4: Achievement — proportional to active goals.
        let achievement_urgency = if self.active_goal_count > 0 {
            (self.active_goal_count as f32 / 10.0).min(0.8)
        } else {
            0.0
        };
        urgencies[DriveTier::Achievement.index()] = achievement_urgency;

        // Tier-5: Self-Actualisation — latent, rises slowly over lifetime.
        // In v1, a small constant background drive.
        urgencies[DriveTier::SelfActualisation.index()] = 0.1;

        DriveStateSnapshot { urgencies }
    }

    /// Compute the value contribution of a candidate action to each drive tier.
    ///
    /// Returns an array of `[0, 1]` contributions indexed by `DriveTier as usize`.
    pub fn value_contributions(&self, candidate: &DriveActionCandidate) -> [f32; TIER_COUNT] {
        let mut contributions = [0.0f32; TIER_COUNT];

        // Tier-0: high urgency + high-urgency action benefit both sides
        // Actions that reduce pressure (e.g. CheapLocal routing) contribute.
        contributions[DriveTier::Viability.index()] =
            if candidate.urgency > 0.5 { 0.3 } else { 0.1 };

        // Tier-1: high urgency + system-preserving actions
        contributions[DriveTier::Integrity.index()] = 0.1;

        // Tier-2: user-facing and operator-objective actions score highly
        if candidate.user_facing {
            contributions[DriveTier::Service.index()] += 0.5;
        }
        if candidate.is_operator_objective {
            contributions[DriveTier::Service.index()] += 0.3;
        }
        contributions[DriveTier::Service.index()] =
            contributions[DriveTier::Service.index()].min(1.0);

        // Tier-3: exploratory actions feed curiosity; novel content feeds both
        if candidate.is_exploratory {
            contributions[DriveTier::Epistemic.index()] += 0.4;
        }
        contributions[DriveTier::Epistemic.index()] += candidate.novelty * 0.3;
        contributions[DriveTier::Epistemic.index()] =
            contributions[DriveTier::Epistemic.index()].min(1.0);

        // Tier-4: completion actions directly advance achievement
        if candidate.is_completion {
            contributions[DriveTier::Achievement.index()] = 0.7;
        }

        // Tier-5: user-facing + completion together advance self-actualisation
        if candidate.user_facing && candidate.is_completion {
            contributions[DriveTier::SelfActualisation.index()] = 0.3;
        }

        contributions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn neutral_signals() -> InteroceptiveSignals {
        InteroceptiveSignals {
            thermal_load: 0.0,
            compute_pressure: 0.0,
            memory_pressure: 0.0,
            power_budget: 1.0,
            financial_budget: 1.0,
            attention_demand: 0.0,
        }
    }

    fn stressed_signals() -> InteroceptiveSignals {
        InteroceptiveSignals {
            thermal_load: 0.9,
            compute_pressure: 0.8,
            memory_pressure: 0.7,
            power_budget: 0.1,
            financial_budget: 0.2,
            attention_demand: 0.5,
        }
    }

    #[test]
    fn tier_ordinal_order_is_ascending() {
        assert!(DriveTier::Viability < DriveTier::Integrity);
        assert!(DriveTier::Integrity < DriveTier::Service);
        assert!(DriveTier::Service < DriveTier::Epistemic);
        assert!(DriveTier::Epistemic < DriveTier::Achievement);
        assert!(DriveTier::Achievement < DriveTier::SelfActualisation);
    }

    #[test]
    fn neutral_signals_produce_low_viability_urgency() {
        let registry = DriveRegistry::new(neutral_signals());
        let snapshot = registry.snapshot();
        assert!(
            snapshot.urgency(DriveTier::Viability) < 0.1,
            "viability urgency should be near zero under neutral signals"
        );
    }

    #[test]
    fn stressed_signals_produce_high_viability_urgency() {
        let registry = DriveRegistry::new(stressed_signals());
        let snapshot = registry.snapshot();
        assert!(
            snapshot.urgency(DriveTier::Viability) > 0.5,
            "viability urgency should be high under stress"
        );
    }

    #[test]
    fn attention_demand_drives_service_urgency() {
        let mut signals = neutral_signals();
        signals.attention_demand = 1.0;
        let registry = DriveRegistry::new(signals);
        let snapshot = registry.snapshot();
        assert!(
            snapshot.urgency(DriveTier::Service) > 0.4,
            "service urgency should rise with attention_demand"
        );
    }

    #[test]
    fn pending_objectives_raise_service_urgency() {
        let mut registry = DriveRegistry::new(neutral_signals());
        registry.set_pending_objectives(5);
        let snapshot = registry.snapshot();
        assert!(
            snapshot.urgency(DriveTier::Service) > 0.1,
            "pending objectives should raise service urgency"
        );
    }

    #[test]
    fn curiosity_drive_starts_with_high_urgency() {
        // No novelty observed → curiosity urgency should be high initially
        let state = CuriosityState::default();
        assert!(
            state.urgency() > 0.0,
            "fresh curiosity should have some urgency"
        );
    }

    #[test]
    fn curiosity_saturates_after_many_observations() {
        let mut state = CuriosityState::new(0.5, 0.0);
        for _ in 0..20 {
            state.observe(1.0);
        }
        assert!(
            state.urgency() < 0.1,
            "curiosity should saturate after many novel stimuli"
        );
    }

    #[test]
    fn curiosity_recovers_after_decay() {
        let mut state = CuriosityState::new(0.5, 0.0);
        state.observe(1.0);
        let before = state.urgency();
        for _ in 0..20 {
            state.tick();
        }
        let after = state.urgency();
        assert!(
            after > before,
            "curiosity urgency should recover after decay ticks"
        );
    }

    #[test]
    fn mastery_urgency_falls_as_competence_rises() {
        let mut state = MasteryState::new(0.8);
        let initial_urgency = state.urgency();
        for _ in 0..20 {
            state.record_outcome(1.0);
        }
        assert!(
            state.urgency() < initial_urgency,
            "mastery urgency should fall as competence improves"
        );
    }

    #[test]
    fn under_survival_stress_detects_high_viability() {
        let registry = DriveRegistry::new(stressed_signals());
        let snapshot = registry.snapshot();
        assert!(
            snapshot.under_survival_stress(0.3),
            "stressed signals should trigger survival stress"
        );
    }

    #[test]
    fn value_contributions_user_facing_action_scores_service_tier() {
        let registry = DriveRegistry::new(neutral_signals());
        let candidate = DriveActionCandidate {
            user_facing: true,
            is_operator_objective: false,
            is_exploratory: false,
            is_completion: false,
            novelty: 0.0,
            urgency: 0.0,
        };
        let contributions = registry.value_contributions(&candidate);
        assert!(
            contributions[DriveTier::Service.index()] >= 0.4,
            "user-facing actions should score highly for service tier"
        );
    }

    #[test]
    fn value_contributions_exploratory_action_scores_epistemic_tier() {
        let registry = DriveRegistry::new(neutral_signals());
        let candidate = DriveActionCandidate {
            user_facing: false,
            is_operator_objective: false,
            is_exploratory: true,
            is_completion: false,
            novelty: 0.5,
            urgency: 0.0,
        };
        let contributions = registry.value_contributions(&candidate);
        assert!(
            contributions[DriveTier::Epistemic.index()] > 0.3,
            "exploratory actions should score epistemic tier"
        );
    }

    #[test]
    fn all_urgencies_are_clamped_to_unit_interval() {
        let registry = DriveRegistry::new(stressed_signals());
        let snapshot = registry.snapshot();
        for urgency in &snapshot.urgencies {
            assert!(
                *urgency >= 0.0 && *urgency <= 1.0,
                "all urgencies must be in [0, 1]"
            );
        }
    }
}
