// crates/vita/src/gate.rs
//! Striatal Gate — E5.2.
//!
//! The arbitration point that decides whether a candidate event warrants a
//! cortex invocation, and at what cost class
//! (`CheapLocal` / `MidTier` / `Frontier`).
//!
//! # Design
//!
//! The default implementation is a **hand-tuned threshold gate** whose inputs
//! are fully explicit and whose every decision is written to the audit trail.
//! The [`Gate`] trait is the hookpoint for a learned replacement (S5.2.5).
//!
//! ## Value score
//!
//! ```text
//! value = urgency_weight × urgency + novelty_weight × novelty
//!       + user_facing_bonus               (if event.user_facing)
//!       + operator_command_bonus          (if SemanticClass::OperatorCommand)
//! value = clamp(value, 0.0, 1.0)
//! ```
//!
//! ## Adaptive threshold (S5.2.2)
//!
//! Thresholds *rise* under stress (harder to trigger cortex) and *fall* when
//! the user is present (more responsive).
//!
//! ```text
//! threshold = base_threshold
//!           + thermal_penalty   × thermal_load
//!           + memory_penalty    × memory_pressure
//!           + financial_penalty × (1.0 − financial_budget)
//!           − attention_boost   × attention_demand
//! threshold = clamp(threshold, 0.05, 0.99)
//! ```
//!
//! ## Cost class
//!
//! | Condition                        | Cost class   |
//! |----------------------------------|--------------|
//! | value < cheap\_local\_ceiling    | `CheapLocal` |
//! | value ≥ frontier\_floor          | `Frontier`   |
//! | cheap\_ceiling ≤ value < frontier | `MidTier`   |
//!
//! # Exit criteria (E5.2)
//!
//! 1. Every cortex invocation is preceded by a `GateDecision` audit entry; no
//!    invocation bypasses the gate without an `override_active = true` entry.
//! 2. Threshold sensitivity to each homeostatic signal is covered by table-driven
//!    unit tests, including neutral-signal baseline behaviour.
//! 3. The `anima why` subcommand reads the most recent `GateDecision` from the
//!    audit log and prints its inputs and reasoning (see `kernels/hosted`).

#![forbid(unsafe_code)]

use crate::{AuditEntry, AuditLog};

// ── Event features ────────────────────────────────────────────────────────────

/// Semantic classification of an incoming event.
///
/// Used to apply class-specific cost bonuses in the value score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticClass {
    /// A direct question or command from a human user.
    UserQuery,
    /// An internal or external system notification.
    SystemEvent,
    /// Low-priority work deferred from the foreground queue.
    BackgroundTask,
    /// A privileged directive from an operator or admin.
    ///
    /// Receives an additional `operator_command_bonus` in the value score
    /// and is always routed to `Frontier` when overridden.
    OperatorCommand,
}

/// Observable features of the candidate event.
#[derive(Debug, Clone)]
pub struct EventFeatures {
    /// Urgency score in `[0.0, 1.0]`.  Higher → time-critical.
    pub urgency: f32,
    /// Novelty score in `[0.0, 1.0]`.  Higher → unexpected content.
    pub novelty: f32,
    /// Semantic classification for class-specific bonuses.
    pub semantic_class: SemanticClass,
    /// `true` when the event originates from or is visible to the human user.
    pub user_facing: bool,
}

// ── Homeostatic signals ───────────────────────────────────────────────────────

/// Snapshot of normalised homeostatic signals at the time of gate evaluation.
///
/// All fields are in `[0.0, 1.0]`.  For budget fields (`power_budget`,
/// `financial_budget`), `1.0` means *fully available* and `0.0` means
/// *exhausted*.
#[derive(Debug, Clone)]
pub struct HomeostaticSignals {
    /// CPU/GPU thermal occupancy (`0.0` = cool, `1.0` = at thermal limit).
    pub thermal_load: f32,
    /// Compute-pipeline saturation (`0.0` = idle, `1.0` = saturated).
    pub compute_pressure: f32,
    /// Working-memory (L1/L2) fill fraction (`0.0` = empty, `1.0` = full).
    pub memory_pressure: f32,
    /// Available power budget (`1.0` = wall-power or full battery, `0.0` = flat).
    pub power_budget: f32,
    /// Remaining financial API budget fraction (`1.0` = fully available).
    pub financial_budget: f32,
    /// User presence/attention level (`1.0` = full attention, `0.0` = absent).
    pub attention_demand: f32,
}

impl HomeostaticSignals {
    /// A neutral snapshot: no stress, all budgets full.
    ///
    /// Use as a baseline in unit tests and newly-initialised contexts.
    pub fn neutral() -> Self {
        Self {
            thermal_load: 0.0,
            compute_pressure: 0.0,
            memory_pressure: 0.0,
            power_budget: 1.0,
            financial_budget: 1.0,
            attention_demand: 0.0,
        }
    }

    /// Convert an [`interoception::InteroceptiveSignals`] snapshot into the
    /// gate's homeostatic input format (E5.7, S5.7.4).
    ///
    /// This is the canonical wiring point that bridges the interoception sensor
    /// layer (provider of real signal values) and the Striatal Gate (consumer
    /// of those values for threshold arbitration).
    ///
    /// Field mapping is one-to-one — both structs share the same six-signal
    /// contract (S5.7.1).
    pub fn from_interoceptive(signals: &interoception::InteroceptiveSignals) -> Self {
        Self {
            thermal_load: signals.thermal_load,
            compute_pressure: signals.compute_pressure,
            memory_pressure: signals.memory_pressure,
            power_budget: signals.power_budget,
            financial_budget: signals.financial_budget,
            attention_demand: signals.attention_demand,
        }
    }
}

// ── Gate decision ─────────────────────────────────────────────────────────────

/// Routing tier for a cortex invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostClass {
    /// Low-cost, local-model invocation (fast, cheap, limited capability).
    CheapLocal,
    /// Intermediate cost/capability tradeoff.
    MidTier,
    /// Full-capability frontier model (slow, expensive, maximum quality).
    Frontier,
}

impl CostClass {
    /// Human-readable label used in audit entries and `anima why` output.
    pub fn as_str(self) -> &'static str {
        match self {
            CostClass::CheapLocal => "CheapLocal",
            CostClass::MidTier => "MidTier",
            CostClass::Frontier => "Frontier",
        }
    }
}

/// Override hint — lets external callers force or suppress cortex invocation.
///
/// Used by explicit user commands (S5.2.4) and operator directives to bypass
/// the normal threshold evaluation.  **Every override is recorded in the
/// audit log** so the bypass is never silent.
#[derive(Debug, Clone)]
pub enum GateOverride {
    /// Normal evaluation — no override applied.
    None,
    /// A human user explicitly requested cortex invocation.
    UserForced {
        /// Human-readable reason for the override (included in audit + reasoning string).
        reason: String,
    },
    /// An operator or system service explicitly requested invocation.
    OperatorForced {
        /// Human-readable reason for the override.
        reason: String,
    },
}

/// The complete output of one gate evaluation.
#[derive(Debug, Clone)]
pub struct GateDecision {
    /// Per-event identifier used for audit log correlation.
    pub event_id: String,
    /// `true` → cortex should be invoked; `false` → skip.
    pub invoke: bool,
    /// Routing tier selected when `invoke` is `true`.
    pub cost_class: Option<CostClass>,
    /// Computed value score for this event (before threshold comparison).
    pub value_score: f32,
    /// Adaptive threshold the value score was tested against.
    pub threshold_applied: f32,
    /// `true` when a `GateOverride` caused the decision to differ from the
    /// threshold comparison result.
    pub override_active: bool,
    /// Human-readable reasoning that can be surfaced by `anima why`.
    pub reasoning: String,
}

// ── Gate trait ────────────────────────────────────────────────────────────────

/// Abstraction over cortex-invocation arbitration (S5.2.5).
///
/// The default implementation is [`ThresholdGate`]; a learned model may replace
/// it at runtime without changing any caller.
pub trait Gate: Send + Sync {
    /// Evaluates whether the described event warrants cortex invocation.
    fn decide(
        &self,
        event_id: &str,
        event: &EventFeatures,
        homeostatic: &HomeostaticSignals,
        override_hint: &GateOverride,
    ) -> GateDecision;
}

// ── GateConfig ────────────────────────────────────────────────────────────────

/// Tunable coefficients for [`ThresholdGate`].
///
/// All weights and penalties are dimensionless scalars.  The documented
/// defaults produce sensible baseline behaviour; calibrate with real workloads
/// before tightening in production.
#[derive(Debug, Clone)]
pub struct GateConfig {
    // ── Value-score weights ───────────────────────────────────────────────────
    /// Contribution of `urgency` to the value score (default `0.65`).
    pub urgency_weight: f32,
    /// Contribution of `novelty` to the value score (default `0.35`).
    pub novelty_weight: f32,
    /// Bonus added when `event.user_facing == true` (default `0.15`).
    pub user_facing_bonus: f32,
    /// Bonus added for `SemanticClass::OperatorCommand` events (default `0.20`).
    pub operator_command_bonus: f32,

    // ── Threshold construction ────────────────────────────────────────────────
    /// Baseline acceptance threshold (default `0.40`).
    pub base_threshold: f32,
    /// Threshold *increase* per unit of `thermal_load` (default `0.30`).
    ///
    /// At maximum thermal load the threshold rises by this amount, making the
    /// cortex harder to trigger when the system is overheating.
    pub thermal_penalty: f32,
    /// Threshold *increase* per unit of `memory_pressure` (default `0.20`).
    pub memory_penalty: f32,
    /// Threshold *increase* per unit of consumed financial budget
    /// (`1.0 − financial_budget`) (default `0.15`).
    pub financial_penalty: f32,
    /// Threshold *decrease* per unit of `attention_demand` (default `0.20`).
    ///
    /// A fully-attentive user drives the threshold down, making the cortex
    /// more responsive when someone is actively watching.
    pub attention_boost: f32,

    // ── Cost-class boundaries ─────────────────────────────────────────────────
    /// Value scores *strictly below* this route to `CheapLocal` (default `0.60`).
    pub cheap_local_ceiling: f32,
    /// Value scores at or above this route to `Frontier` (default `0.85`).
    pub frontier_floor: f32,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            urgency_weight: 0.65,
            novelty_weight: 0.35,
            user_facing_bonus: 0.15,
            operator_command_bonus: 0.20,
            base_threshold: 0.40,
            thermal_penalty: 0.30,
            memory_penalty: 0.20,
            financial_penalty: 0.15,
            attention_boost: 0.20,
            cheap_local_ceiling: 0.60,
            frontier_floor: 0.85,
        }
    }
}

// ── ThresholdGate ─────────────────────────────────────────────────────────────

/// Hand-tuned threshold implementation of [`Gate`].
///
/// Instances are cheaply cloneable (all state is in the config `f32` fields).
pub struct ThresholdGate {
    config: GateConfig,
}

impl ThresholdGate {
    /// Creates a gate with the supplied configuration.
    pub fn new(config: GateConfig) -> Self {
        Self { config }
    }

    /// Creates a gate with the documented default coefficients.
    pub fn with_defaults() -> Self {
        Self::new(GateConfig::default())
    }

    /// Returns a reference to the active configuration.
    pub fn config(&self) -> &GateConfig {
        &self.config
    }

    // ── Scoring helpers ───────────────────────────────────────────────────────
    //
    // These are `pub` so composing gates (e.g. the E12 `MotivatedGate` in
    // `motivation_gate`) can reuse the *exact* same value/threshold/cost-class
    // formulas rather than re-deriving them.  Behaviour is unchanged.

    /// Computes the base value score for an event (urgency/novelty + bonuses),
    /// clamped to `[0.0, 1.0]`.  This is the score the threshold is tested
    /// against before any drive augmentation.
    pub fn value_score(&self, event: &EventFeatures) -> f32 {
        let base = self.config.urgency_weight * event.urgency.clamp(0.0, 1.0)
            + self.config.novelty_weight * event.novelty.clamp(0.0, 1.0);
        let user_bonus = if event.user_facing {
            self.config.user_facing_bonus
        } else {
            0.0
        };
        let class_bonus = match event.semantic_class {
            SemanticClass::OperatorCommand => self.config.operator_command_bonus,
            _ => 0.0,
        };
        (base + user_bonus + class_bonus).clamp(0.0, 1.0)
    }

    /// Computes the adaptive acceptance threshold from homeostatic signals,
    /// clamped to `[0.05, 0.99]`.  Rises under stress, falls with attention.
    pub fn adaptive_threshold(&self, h: &HomeostaticSignals) -> f32 {
        let financial_used = (1.0_f32 - h.financial_budget).clamp(0.0, 1.0);
        let raw = self.config.base_threshold
            + self.config.thermal_penalty * h.thermal_load.clamp(0.0, 1.0)
            + self.config.memory_penalty * h.memory_pressure.clamp(0.0, 1.0)
            + self.config.financial_penalty * financial_used
            - self.config.attention_boost * h.attention_demand.clamp(0.0, 1.0);
        raw.clamp(0.05, 0.99)
    }

    /// Maps a (possibly drive-augmented) value score to a [`CostClass`] using
    /// the configured cheap-local / frontier boundaries.
    pub fn cost_class_for(&self, score: f32) -> CostClass {
        if score >= self.config.frontier_floor {
            CostClass::Frontier
        } else if score < self.config.cheap_local_ceiling {
            CostClass::CheapLocal
        } else {
            CostClass::MidTier
        }
    }
}

impl Gate for ThresholdGate {
    fn decide(
        &self,
        event_id: &str,
        event: &EventFeatures,
        homeostatic: &HomeostaticSignals,
        override_hint: &GateOverride,
    ) -> GateDecision {
        let value_score = self.value_score(event);
        let threshold = self.adaptive_threshold(homeostatic);

        match override_hint {
            GateOverride::None => {
                let invoke = value_score >= threshold;
                let cost_class = if invoke {
                    Some(self.cost_class_for(value_score))
                } else {
                    None
                };
                let class_label = cost_class.map(CostClass::as_str).unwrap_or("none");
                let reasoning = if invoke {
                    format!(
                        "value_score={:.3} >= threshold={:.3} → invoke at {class_label}",
                        value_score, threshold,
                    )
                } else {
                    format!(
                        "value_score={:.3} < threshold={:.3} → blocked",
                        value_score, threshold,
                    )
                };
                GateDecision {
                    event_id: event_id.to_string(),
                    invoke,
                    cost_class,
                    value_score,
                    threshold_applied: threshold,
                    override_active: false,
                    reasoning,
                }
            }

            GateOverride::UserForced { reason } => {
                let cost_class = Some(self.cost_class_for(value_score));
                GateDecision {
                    event_id: event_id.to_string(),
                    invoke: true,
                    cost_class,
                    value_score,
                    threshold_applied: threshold,
                    override_active: true,
                    reasoning: format!(
                        "user-forced override (reason: {reason}); \
                         value_score={:.3}, threshold={:.3}",
                        value_score, threshold,
                    ),
                }
            }

            GateOverride::OperatorForced { reason } => {
                // Operator commands always route to Frontier regardless of score.
                GateDecision {
                    event_id: event_id.to_string(),
                    invoke: true,
                    cost_class: Some(CostClass::Frontier),
                    value_score,
                    threshold_applied: threshold,
                    override_active: true,
                    reasoning: format!(
                        "operator-forced override (reason: {reason}); \
                         value_score={:.3}, threshold={:.3}, cost_class=Frontier",
                        value_score, threshold,
                    ),
                }
            }
        }
    }
}

// ── Audit helper ──────────────────────────────────────────────────────────────

/// Records a [`GateDecision`] into the audit log as an
/// [`AuditEntry::GateDecision`] entry.
///
/// Called by `vita`'s dispatch loop immediately before (or instead of) each
/// cortex invocation so that every gate evaluation is permanently traceable.
/// The `anima why` subcommand reads these entries to explain recent decisions.
pub fn record_gate_decision(
    audit: &mut AuditLog,
    agent_id: &str,
    decision: &GateDecision,
    event: &EventFeatures,
    homeostatic: &HomeostaticSignals,
) {
    audit.push(AuditEntry::GateDecision {
        agent_id: agent_id.to_string(),
        event_id: decision.event_id.clone(),
        invoke: decision.invoke,
        cost_class: decision.cost_class.map(|c| c.as_str().to_string()),
        urgency: event.urgency,
        novelty: event.novelty,
        user_facing: event.user_facing,
        semantic_class: format!("{:?}", event.semantic_class),
        value_score: decision.value_score,
        threshold_applied: decision.threshold_applied,
        thermal_load: homeostatic.thermal_load,
        compute_pressure: homeostatic.compute_pressure,
        memory_pressure: homeostatic.memory_pressure,
        power_budget: homeostatic.power_budget,
        financial_budget: homeostatic.financial_budget,
        attention_demand: homeostatic.attention_demand,
        reasoning: decision.reasoning.clone(),
        override_active: decision.override_active,
    });
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn default_gate() -> ThresholdGate {
        ThresholdGate::with_defaults()
    }

    fn low_event() -> EventFeatures {
        EventFeatures {
            urgency: 0.1,
            novelty: 0.1,
            semantic_class: SemanticClass::BackgroundTask,
            user_facing: false,
        }
    }

    fn high_event() -> EventFeatures {
        EventFeatures {
            urgency: 1.0,
            novelty: 0.5,
            semantic_class: SemanticClass::UserQuery,
            user_facing: true,
        }
    }

    // ── S5.2.2 — Hand-tuned threshold function ────────────────────────────────

    #[test]
    fn gate_blocks_low_urgency_low_novelty_events() {
        let gate = default_gate();
        let d = gate.decide(
            "evt-1",
            &low_event(),
            &HomeostaticSignals::neutral(),
            &GateOverride::None,
        );
        assert!(!d.invoke, "low-urgency/novelty event must be blocked");
        assert!(d.cost_class.is_none());
        assert!(!d.override_active);
    }

    #[test]
    fn gate_invokes_on_high_urgency_user_facing_event() {
        let gate = default_gate();
        let d = gate.decide(
            "evt-2",
            &high_event(),
            &HomeostaticSignals::neutral(),
            &GateOverride::None,
        );
        assert!(
            d.invoke,
            "high-urgency user-facing event must trigger invocation"
        );
        assert!(d.cost_class.is_some());
    }

    #[test]
    fn cost_class_is_cheap_local_for_moderate_value_event() {
        let gate = default_gate();
        // urgency=0.7, novelty=0.0, no bonuses: value = 0.65*0.7 = 0.455 → above base_threshold(0.40)
        // and below cheap_local_ceiling(0.60) → CheapLocal
        let event = EventFeatures {
            urgency: 0.7,
            novelty: 0.0,
            semantic_class: SemanticClass::SystemEvent,
            user_facing: false,
        };
        let d = gate.decide(
            "evt-cc1",
            &event,
            &HomeostaticSignals::neutral(),
            &GateOverride::None,
        );
        assert!(d.invoke);
        assert_eq!(d.cost_class, Some(CostClass::CheapLocal));
    }

    #[test]
    fn cost_class_is_mid_tier_for_intermediate_event() {
        let gate = default_gate();
        // urgency=0.8, novelty=0.3, user_facing=false:
        // value = 0.65*0.8 + 0.35*0.3 = 0.52 + 0.105 = 0.625
        // 0.625 >= cheap_ceiling(0.60) and < frontier_floor(0.85) → MidTier
        let event = EventFeatures {
            urgency: 0.8,
            novelty: 0.3,
            semantic_class: SemanticClass::UserQuery,
            user_facing: false,
        };
        let d = gate.decide(
            "evt-cc2",
            &event,
            &HomeostaticSignals::neutral(),
            &GateOverride::None,
        );
        assert!(d.invoke);
        assert_eq!(d.cost_class, Some(CostClass::MidTier));
    }

    #[test]
    fn cost_class_is_frontier_for_maximum_value_event() {
        let gate = default_gate();
        // urgency=1.0, novelty=1.0, user_facing=true, OperatorCommand:
        // raw = 0.65 + 0.35 + 0.15 + 0.20 = 1.15 → clamped to 1.0 → Frontier
        let event = EventFeatures {
            urgency: 1.0,
            novelty: 1.0,
            semantic_class: SemanticClass::OperatorCommand,
            user_facing: true,
        };
        let d = gate.decide(
            "evt-cc3",
            &event,
            &HomeostaticSignals::neutral(),
            &GateOverride::None,
        );
        assert!(d.invoke);
        assert_eq!(d.cost_class, Some(CostClass::Frontier));
    }

    // ── S5.2.2 — Homeostatic signal sensitivity (table-driven) ───────────────

    /// Exit criterion 2: threshold sensitivity to each homeostatic signal,
    /// including the neutral-signal baseline.
    #[test]
    fn homeostatic_signal_sensitivity_table() {
        let gate = default_gate();
        let cfg = gate.config();

        // For each signal we compute the threshold at neutral and at max, and
        // assert the *direction* and approximate *magnitude* of the shift.
        struct Row {
            name: &'static str,
            signals_high: HomeostaticSignals,
            threshold_neutral: f32,
            expected_shift_direction: f32, // +1 = raises threshold, −1 = lowers it
            expected_shift_magnitude: f32, // the penalty/boost coefficient
        }

        let neutral_threshold = gate.adaptive_threshold(&HomeostaticSignals::neutral());

        let rows = [
            Row {
                name: "thermal_load at max",
                signals_high: HomeostaticSignals {
                    thermal_load: 1.0,
                    ..HomeostaticSignals::neutral()
                },
                threshold_neutral: neutral_threshold,
                expected_shift_direction: 1.0,
                expected_shift_magnitude: cfg.thermal_penalty,
            },
            Row {
                name: "memory_pressure at max",
                signals_high: HomeostaticSignals {
                    memory_pressure: 1.0,
                    ..HomeostaticSignals::neutral()
                },
                threshold_neutral: neutral_threshold,
                expected_shift_direction: 1.0,
                expected_shift_magnitude: cfg.memory_penalty,
            },
            Row {
                name: "financial_budget exhausted (0.0)",
                signals_high: HomeostaticSignals {
                    financial_budget: 0.0,
                    ..HomeostaticSignals::neutral()
                },
                threshold_neutral: neutral_threshold,
                expected_shift_direction: 1.0,
                expected_shift_magnitude: cfg.financial_penalty,
            },
            Row {
                name: "attention_demand at max",
                signals_high: HomeostaticSignals {
                    attention_demand: 1.0,
                    ..HomeostaticSignals::neutral()
                },
                threshold_neutral: neutral_threshold,
                expected_shift_direction: -1.0,
                expected_shift_magnitude: cfg.attention_boost,
            },
        ];

        for row in &rows {
            let threshold_high = gate.adaptive_threshold(&row.signals_high);
            let shift = threshold_high - row.threshold_neutral;
            let expected = row.expected_shift_direction * row.expected_shift_magnitude;
            assert!(
                (shift - expected).abs() < 1e-5,
                "signal '{}': expected shift {:.4}, got {:.4} \
                 (neutral={:.4}, stressed={:.4})",
                row.name,
                expected,
                shift,
                row.threshold_neutral,
                threshold_high
            );
        }
    }

    #[test]
    fn neutral_signals_produce_baseline_threshold() {
        let gate = default_gate();
        let threshold = gate.adaptive_threshold(&HomeostaticSignals::neutral());
        // neutral: no stress, financial fully available, attention absent
        // threshold = base(0.40) + 0 + 0 + 0 - 0 = 0.40
        assert!(
            (threshold - gate.config().base_threshold).abs() < 1e-5,
            "neutral signals must produce exactly the base threshold"
        );
    }

    #[test]
    fn thermal_stress_raises_threshold() {
        let gate = default_gate();
        let t_neutral = gate.adaptive_threshold(&HomeostaticSignals::neutral());
        let t_hot = gate.adaptive_threshold(&HomeostaticSignals {
            thermal_load: 1.0,
            ..HomeostaticSignals::neutral()
        });
        assert!(
            t_hot > t_neutral,
            "maximum thermal load must raise the threshold (neutral={t_neutral:.3}, hot={t_hot:.3})"
        );
    }

    #[test]
    fn financial_pressure_raises_threshold() {
        let gate = default_gate();
        let t_funded = gate.adaptive_threshold(&HomeostaticSignals::neutral()); // financial_budget=1.0
        let t_broke = gate.adaptive_threshold(&HomeostaticSignals {
            financial_budget: 0.0,
            ..HomeostaticSignals::neutral()
        });
        assert!(
            t_broke > t_funded,
            "exhausted financial budget must raise the threshold (funded={t_funded:.3}, broke={t_broke:.3})"
        );
    }

    #[test]
    fn memory_pressure_raises_threshold() {
        let gate = default_gate();
        let t_free = gate.adaptive_threshold(&HomeostaticSignals::neutral());
        let t_full = gate.adaptive_threshold(&HomeostaticSignals {
            memory_pressure: 1.0,
            ..HomeostaticSignals::neutral()
        });
        assert!(
            t_full > t_free,
            "maximum memory pressure must raise the threshold (free={t_free:.3}, full={t_full:.3})"
        );
    }

    #[test]
    fn high_attention_demand_lowers_threshold() {
        let gate = default_gate();
        let t_inattentive = gate.adaptive_threshold(&HomeostaticSignals::neutral()); // attention=0.0
        let t_attentive = gate.adaptive_threshold(&HomeostaticSignals {
            attention_demand: 1.0,
            ..HomeostaticSignals::neutral()
        });
        assert!(
            t_attentive < t_inattentive,
            "full user attention must lower the threshold (inattentive={t_inattentive:.3}, attentive={t_attentive:.3})"
        );
    }

    // ── S5.2.4 — Override mechanics ───────────────────────────────────────────

    #[test]
    fn user_override_forces_invocation_of_blocked_event() {
        let gate = default_gate();
        // low_event() would normally be blocked.
        let d_blocked = gate.decide(
            "evt-o1",
            &low_event(),
            &HomeostaticSignals::neutral(),
            &GateOverride::None,
        );
        assert!(
            !d_blocked.invoke,
            "low event must be blocked without override"
        );

        let d_forced = gate.decide(
            "evt-o1",
            &low_event(),
            &HomeostaticSignals::neutral(),
            &GateOverride::UserForced {
                reason: "user typed /force".to_string(),
            },
        );
        assert!(d_forced.invoke, "user override must force invocation");
        assert!(d_forced.override_active, "override_active must be true");
    }

    #[test]
    fn operator_override_forces_frontier_cost_class() {
        let gate = default_gate();
        let d = gate.decide(
            "evt-o2",
            &low_event(),
            &HomeostaticSignals::neutral(),
            &GateOverride::OperatorForced {
                reason: "emergency directive".to_string(),
            },
        );
        assert!(d.invoke, "operator override must force invocation");
        assert_eq!(
            d.cost_class,
            Some(CostClass::Frontier),
            "operator override must always route to Frontier"
        );
        assert!(d.override_active);
    }

    #[test]
    fn no_override_does_not_set_override_active() {
        let gate = default_gate();
        let d = gate.decide(
            "evt-o3",
            &high_event(),
            &HomeostaticSignals::neutral(),
            &GateOverride::None,
        );
        assert!(
            !d.override_active,
            "override_active must be false when no override applied"
        );
    }

    // ── S5.2.3 — Audit entry completeness ────────────────────────────────────

    #[test]
    fn record_gate_decision_emits_audit_entry_with_all_inputs() {
        let gate = default_gate();
        let event = high_event();
        let signals = HomeostaticSignals::neutral();
        let decision = gate.decide("evt-audit", &event, &signals, &GateOverride::None);

        let mut log = AuditLog::new();
        record_gate_decision(&mut log, "test-agent", &decision, &event, &signals);

        assert_eq!(log.len(), 1);
        match &log.entries()[0] {
            AuditEntry::GateDecision {
                agent_id,
                event_id,
                invoke,
                urgency,
                novelty,
                user_facing,
                value_score,
                threshold_applied,
                reasoning,
                override_active,
                ..
            } => {
                assert_eq!(agent_id, "test-agent");
                assert_eq!(event_id, "evt-audit");
                assert_eq!(*invoke, decision.invoke);
                assert!((*urgency - event.urgency).abs() < 1e-6);
                assert!((*novelty - event.novelty).abs() < 1e-6);
                assert_eq!(*user_facing, event.user_facing);
                assert!((*value_score - decision.value_score).abs() < 1e-6);
                assert!((*threshold_applied - decision.threshold_applied).abs() < 1e-6);
                assert!(!reasoning.is_empty());
                assert_eq!(*override_active, decision.override_active);
            }
            other => panic!("expected GateDecision entry, got {other:?}"),
        }
    }

    #[test]
    fn override_decision_audit_entry_carries_override_active_true() {
        let gate = default_gate();
        let event = low_event();
        let signals = HomeostaticSignals::neutral();
        let decision = gate.decide(
            "evt-audit-override",
            &event,
            &signals,
            &GateOverride::UserForced {
                reason: "test".to_string(),
            },
        );

        let mut log = AuditLog::new();
        record_gate_decision(&mut log, "agent-x", &decision, &event, &signals);

        match &log.entries()[0] {
            AuditEntry::GateDecision {
                override_active,
                invoke,
                ..
            } => {
                assert!(
                    *override_active,
                    "override_active must be true for forced decision"
                );
                assert!(*invoke, "invoke must be true for forced decision");
            }
            other => panic!("expected GateDecision, got {other:?}"),
        }
    }

    // ── E5.2 exit criterion 1 — every invocation preceded by gate entry ───────

    #[test]
    fn every_invocation_decision_is_preceded_by_gate_audit_entry() {
        let gate = default_gate();
        let mut log = AuditLog::new();
        let agent_id = "gate-soak-agent";

        // Simulate 10 gate evaluations, some invoked, some blocked.
        for i in 0..10u32 {
            let urgency = if i % 2 == 0 { 0.9 } else { 0.1 };
            let event = EventFeatures {
                urgency,
                novelty: 0.0,
                semantic_class: SemanticClass::SystemEvent,
                user_facing: false,
            };
            let signals = HomeostaticSignals::neutral();
            let decision = gate.decide(&format!("evt-{i}"), &event, &signals, &GateOverride::None);
            record_gate_decision(&mut log, agent_id, &decision, &event, &signals);
        }

        assert_eq!(
            log.len(),
            10,
            "every evaluation must produce exactly one audit entry"
        );

        // Count invoked decisions.
        let invoked = log
            .entries()
            .iter()
            .filter(|e| matches!(e, AuditEntry::GateDecision { invoke: true, .. }))
            .count();
        let blocked = log
            .entries()
            .iter()
            .filter(|e| matches!(e, AuditEntry::GateDecision { invoke: false, .. }))
            .count();
        assert_eq!(invoked + blocked, 10);
    }

    // ── Reasoning string ──────────────────────────────────────────────────────

    #[test]
    fn blocked_decision_reasoning_mentions_blocked() {
        let gate = default_gate();
        let d = gate.decide(
            "evt-r1",
            &low_event(),
            &HomeostaticSignals::neutral(),
            &GateOverride::None,
        );
        assert!(!d.invoke);
        assert!(
            d.reasoning.contains("blocked"),
            "reasoning for blocked event must mention 'blocked', got: {:?}",
            d.reasoning
        );
    }

    #[test]
    fn invoked_decision_reasoning_mentions_invoke() {
        let gate = default_gate();
        let d = gate.decide(
            "evt-r2",
            &high_event(),
            &HomeostaticSignals::neutral(),
            &GateOverride::None,
        );
        assert!(d.invoke);
        assert!(
            d.reasoning.contains("invoke"),
            "reasoning for invoked event must mention 'invoke', got: {:?}",
            d.reasoning
        );
    }

    #[test]
    fn override_decision_reasoning_mentions_override() {
        let gate = default_gate();
        let d = gate.decide(
            "evt-r3",
            &low_event(),
            &HomeostaticSignals::neutral(),
            &GateOverride::UserForced {
                reason: "manual".to_string(),
            },
        );
        assert!(
            d.reasoning.contains("override") || d.reasoning.contains("forced"),
            "reasoning for override must mention override, got: {:?}",
            d.reasoning
        );
    }

    // ── E5.7: HomeostaticSignals::from_interoceptive ──────────────────────────

    #[test]
    fn from_interoceptive_converts_neutral_signals_correctly() {
        let i = interoception::InteroceptiveSignals::neutral();
        let h = HomeostaticSignals::from_interoceptive(&i);
        assert_eq!(h.thermal_load, i.thermal_load);
        assert_eq!(h.compute_pressure, i.compute_pressure);
        assert_eq!(h.memory_pressure, i.memory_pressure);
        assert_eq!(h.power_budget, i.power_budget);
        assert_eq!(h.financial_budget, i.financial_budget);
        assert_eq!(h.attention_demand, i.attention_demand);
    }

    #[test]
    fn from_interoceptive_converts_maximum_stress_correctly() {
        let i = interoception::InteroceptiveSignals::maximum_stress();
        let h = HomeostaticSignals::from_interoceptive(&i);
        assert_eq!(h.thermal_load, 1.0);
        assert_eq!(h.power_budget, 0.0);
        assert_eq!(h.financial_budget, 0.0);
    }

    #[test]
    fn from_interoceptive_wires_correctly_into_gate_threshold() {
        // Verify that a maximum-stress snapshot raises the gate threshold
        // enough to block a moderate-value event.
        let gate = default_gate();
        let i = interoception::InteroceptiveSignals {
            thermal_load: 0.9,
            compute_pressure: 0.9,
            memory_pressure: 0.5,
            power_budget: 0.0,     // flat battery
            financial_budget: 0.0, // budget exhausted
            attention_demand: 0.0,
        };
        let h = HomeostaticSignals::from_interoceptive(&i);
        let event = EventFeatures {
            urgency: 0.5,
            novelty: 0.3,
            semantic_class: SemanticClass::BackgroundTask,
            user_facing: false,
        };
        let d = gate.decide("stress-test", &event, &h, &GateOverride::None);
        // Under severe resource stress the threshold should be high enough
        // to block a moderate event.
        assert!(
            !d.invoke,
            "severe resource stress must raise threshold enough to block moderate event"
        );
    }
}
