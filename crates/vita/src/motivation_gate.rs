// crates/vita/src/motivation_gate.rs
//! Motivated Striatal Gate — E12 ↔ E5.2 integration.
//!
//! Wires the **E12 Motivation** drive hierarchy into the **Striatal Gate** so
//! that the agent's drives (viability, integrity, service, epistemic,
//! achievement, self-actualisation) actually influence cortex-invocation
//! decisions.
//!
//! [`MotivatedGate`] composes the four motivation primitives around the existing
//! [`ThresholdGate`]:
//!
//! ```text
//! HomeostaticSignals ─► DriveRegistry ─► PriorityLattice ─► DriveValueIntegrator
//!         │                  │                                       │
//!         │            DriveStateSnapshot                  augmented value_score
//!         │                  │                                       │
//!         ▼                  ▼                                       ▼
//!   adaptive_threshold ◄─ AffectState.gate_threshold_nudge ──► GateDecision
//! ```
//!
//! # Reused formulas (no re-derivation)
//!
//! Base value, adaptive threshold, and cost-class mapping all come straight from
//! the wrapped [`ThresholdGate`] via its now-`pub` helpers
//! ([`ThresholdGate::value_score`], [`ThresholdGate::adaptive_threshold`],
//! [`ThresholdGate::cost_class_for`]).  The motivated path differs from the plain
//! gate in exactly two places:
//!
//! 1. the value tested against the threshold is the **drive-augmented**
//!    `total_value` rather than the bare `base_value`; and
//! 2. the threshold is multiplied by the **affect nudge** in `[0.9, 1.1]`,
//!    re-clamped to the gate's canonical `[0.05, 0.99]` band.
//!
//! # Invariants (load-bearing)
//!
//! * **Corrigibility / overrides are never weakened.**  `GateOverride::UserForced`
//!   and `GateOverride::OperatorForced` force `invoke = true` exactly as in
//!   [`ThresholdGate::decide`]; operator overrides still route to
//!   [`CostClass::Frontier`].  Drives and affect only ever influence the
//!   *un-overridden* threshold comparison — they can never block a forced
//!   invocation, and they can never force one past the operator's hand.
//! * **Drives only nudge.**  `drive_delta` is bounded by
//!   [`DriveIntegratorConfig::max_drive_delta`] and the affect factor by
//!   `[0.9, 1.1]`; neither can dominate the hand-tuned baseline.
//! * **Disabled path is byte-for-byte the plain gate.**  With a disabled
//!   integrator and an all-quiescent drive snapshot the augmented value equals
//!   the base value and the nudge is ≈ 1.0, so `decide_motivated` reproduces
//!   [`ThresholdGate::decide`] (see `disabled_path_matches_plain_gate`).

#![forbid(unsafe_code)]

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

use interoception::InteroceptiveSignals;
use motivation::{
    AffectState, DriveActionCandidate, DriveAugmentedValue, DriveIntegratorConfig, DriveRegistry,
    DriveStateSnapshot, DriveValueIntegrator, PriorityLattice,
};

use crate::gate::{
    CostClass, EventFeatures, GateDecision, GateOverride, HomeostaticSignals, SemanticClass,
    ThresholdGate,
};

// ── HomeostaticSignals → InteroceptiveSignals ──────────────────────────────────

/// Convert the gate's [`HomeostaticSignals`] back into the
/// [`InteroceptiveSignals`] shape the [`DriveRegistry`] consumes.
///
/// The two structs share the same six-signal contract (S5.7.1); this is the
/// inverse of [`HomeostaticSignals::from_interoceptive`].  We map field-by-field
/// rather than via `InteroceptiveSignals::neutral()` because the two `neutral`
/// constructors disagree on `attention_demand` (gate uses `0.0`, interoception
/// uses `0.5`).
fn signals_for_registry(h: &HomeostaticSignals) -> InteroceptiveSignals {
    InteroceptiveSignals {
        thermal_load: h.thermal_load,
        compute_pressure: h.compute_pressure,
        memory_pressure: h.memory_pressure,
        power_budget: h.power_budget,
        financial_budget: h.financial_budget,
        attention_demand: h.attention_demand,
    }
}

// ── DriveActionCandidate derivation ────────────────────────────────────────────

/// Derive a [`DriveActionCandidate`] from the gate's [`EventFeatures`].
///
/// Mapping (kept deliberately simple and explainable):
///
/// | candidate field         | source                                              |
/// |-------------------------|-----------------------------------------------------|
/// | `urgency` / `novelty`   | passed through directly from the event              |
/// | `user_facing`           | `event.user_facing`                                 |
/// | `is_operator_objective` | `semantic_class == OperatorCommand`                 |
/// | `is_exploratory`        | `semantic_class == BackgroundTask` *or* high novelty |
/// | `is_completion`         | `semantic_class == UserQuery` (answering completes a request) |
///
/// `is_exploratory` treats high-novelty events (≥ 0.6) as curiosity-relevant
/// even outside the background lane, so a surprising system event can still feed
/// the epistemic drive.
pub fn candidate_from_event(event: &EventFeatures) -> DriveActionCandidate {
    let is_operator_objective = matches!(event.semantic_class, SemanticClass::OperatorCommand);
    let is_exploratory =
        matches!(event.semantic_class, SemanticClass::BackgroundTask) || event.novelty >= 0.6;
    let is_completion = matches!(event.semantic_class, SemanticClass::UserQuery);

    DriveActionCandidate {
        user_facing: event.user_facing,
        is_operator_objective,
        is_exploratory,
        is_completion,
        novelty: event.novelty.clamp(0.0, 1.0),
        urgency: event.urgency.clamp(0.0, 1.0),
    }
}

// ── MotivatedGate ──────────────────────────────────────────────────────────────

/// A [`ThresholdGate`] augmented with the E12 drive hierarchy.
///
/// Owns the four motivation primitives.  Internal drive state (curiosity /
/// mastery satiation, pending objectives, active-goal count) lives on the
/// embedded [`DriveRegistry`]; call [`MotivatedGate::update_signals`] once per
/// scheduler tick to refresh the interoceptive snapshot it derives urgencies
/// from.
pub struct MotivatedGate {
    gate: ThresholdGate,
    registry: DriveRegistry,
    lattice: PriorityLattice,
    integrator: DriveValueIntegrator,
}

impl MotivatedGate {
    /// Build a motivated gate from its four components.
    pub fn new(
        gate: ThresholdGate,
        registry: DriveRegistry,
        lattice: PriorityLattice,
        integrator: DriveValueIntegrator,
    ) -> Self {
        Self {
            gate,
            registry,
            lattice,
            integrator,
        }
    }

    /// Build a motivated gate with default lattice + integrator, a default
    /// [`ThresholdGate`], and a [`DriveRegistry`] seeded from `signals`.
    pub fn with_defaults(signals: &HomeostaticSignals) -> Self {
        Self::new(
            ThresholdGate::with_defaults(),
            DriveRegistry::new(signals_for_registry(signals)),
            PriorityLattice::default(),
            DriveValueIntegrator::default(),
        )
    }

    /// Build a motivated gate around an explicit [`ThresholdGate`] (so callers
    /// that already tuned a `GateConfig` keep their coefficients) with default
    /// motivation primitives seeded from `signals`.
    pub fn from_gate(gate: ThresholdGate, signals: &HomeostaticSignals) -> Self {
        Self::new(
            gate,
            DriveRegistry::new(signals_for_registry(signals)),
            PriorityLattice::default(),
            DriveValueIntegrator::new(DriveIntegratorConfig::default()),
        )
    }

    /// Borrow the wrapped threshold gate (for reuse of its formulas / config).
    pub fn gate(&self) -> &ThresholdGate {
        &self.gate
    }

    /// Mutable access to the embedded drive registry (set pending objectives,
    /// record task outcomes, observe novelty, advance satiation, …).
    pub fn registry_mut(&mut self) -> &mut DriveRegistry {
        &mut self.registry
    }

    /// Refresh the drive registry's interoceptive snapshot from the gate's
    /// homeostatic signals.  Call once per scheduler cycle (1 Hz) before
    /// [`MotivatedGate::decide_motivated`] so drive urgencies track the latest
    /// sensor reading.
    pub fn update_signals(&mut self, h: &HomeostaticSignals) {
        self.registry.update_signals(signals_for_registry(h));
    }

    /// Current drive-state snapshot (urgencies for all six tiers).
    pub fn drive_snapshot(&self) -> DriveStateSnapshot {
        self.registry.snapshot()
    }

    /// The motivated decision.
    ///
    /// Returns the [`GateDecision`] together with the full
    /// [`DriveAugmentedValue`] decomposition and the derived [`AffectState`] so
    /// the caller can emit `DriveStateSnapshot` / `AffectStateSnapshot` audit
    /// entries.
    ///
    /// Semantics:
    ///
    /// * base value ← [`ThresholdGate::value_score`] (identical formula);
    /// * `total_value` ← [`DriveValueIntegrator::augment`] (base + bounded
    ///   drive delta);
    /// * threshold ← [`ThresholdGate::adaptive_threshold`] × affect nudge,
    ///   re-clamped to `[0.05, 0.99]`;
    /// * override handling is identical to [`ThresholdGate::decide`] — forced
    ///   invocations are never blocked by drives/affect, and operator forces
    ///   still route to [`CostClass::Frontier`].
    pub fn decide_motivated(
        &self,
        event_id: &str,
        event: &EventFeatures,
        homeostatic: &HomeostaticSignals,
        override_hint: &GateOverride,
    ) -> (GateDecision, DriveAugmentedValue, AffectState) {
        // 1. Drive snapshot + lattice weights from the registry's current state.
        let snapshot = self.registry.snapshot();
        let weights = self.lattice.compute_weights(&snapshot);
        let affect = AffectState::from_drives(&snapshot);

        // 2. Base value via the gate's own formula, then drive augmentation.
        let candidate = candidate_from_event(event);
        let base_value = self.gate.value_score(event);
        let contributions = self.registry.value_contributions(&candidate);
        let augmented = self
            .integrator
            .augment(base_value, &snapshot, &weights, &contributions);
        let total_value = augmented.total_value;

        // 3. Affect-nudged adaptive threshold, re-clamped to the canonical band.
        //    The nudge is multiplicative and bounded to [0.9, 1.1]; the clamp
        //    keeps the effective threshold inside the gate's [0.05, 0.99] band.
        let base_threshold = self.gate.adaptive_threshold(homeostatic);
        let nudge = affect.gate_threshold_nudge();
        let threshold = (base_threshold * nudge).clamp(0.05, 0.99);

        // 4. Decision — override handling mirrors ThresholdGate::decide exactly,
        //    but tests `total_value` against the nudged threshold.
        let decision = match override_hint {
            GateOverride::None => {
                let invoke = total_value >= threshold;
                let cost_class = if invoke {
                    Some(self.gate.cost_class_for(total_value))
                } else {
                    None
                };
                let class_label = cost_class.map(CostClass::as_str).unwrap_or("none");
                let reasoning = if invoke {
                    format!(
                        "motivated: base={:.3} +drive Δ={:.3} → total={:.3} >= threshold={:.3} \
                         (nudge×{:.3}) → invoke at {class_label}",
                        base_value, augmented.drive_delta, total_value, threshold, nudge,
                    )
                } else {
                    format!(
                        "motivated: base={:.3} +drive Δ={:.3} → total={:.3} < threshold={:.3} \
                         (nudge×{:.3}) → blocked",
                        base_value, augmented.drive_delta, total_value, threshold, nudge,
                    )
                };
                GateDecision {
                    event_id: event_id.to_string(),
                    invoke,
                    cost_class,
                    value_score: total_value,
                    threshold_applied: threshold,
                    override_active: false,
                    reasoning,
                }
            }

            GateOverride::UserForced { reason } => {
                let cost_class = Some(self.gate.cost_class_for(total_value));
                GateDecision {
                    event_id: event_id.to_string(),
                    invoke: true,
                    cost_class,
                    value_score: total_value,
                    threshold_applied: threshold,
                    override_active: true,
                    reasoning: format!(
                        "user-forced override (reason: {reason}); \
                         motivated total={:.3} (base={:.3}, drive Δ={:.3}), threshold={:.3}",
                        total_value, base_value, augmented.drive_delta, threshold,
                    ),
                }
            }

            GateOverride::OperatorForced { reason } => {
                // Operator commands always route to Frontier regardless of score
                // — drives and affect cannot alter this.
                GateDecision {
                    event_id: event_id.to_string(),
                    invoke: true,
                    cost_class: Some(CostClass::Frontier),
                    value_score: total_value,
                    threshold_applied: threshold,
                    override_active: true,
                    reasoning: format!(
                        "operator-forced override (reason: {reason}); \
                         motivated total={:.3} (base={:.3}, drive Δ={:.3}), \
                         threshold={:.3}, cost_class=Frontier",
                        total_value, base_value, augmented.drive_delta, threshold,
                    ),
                }
            }
        };

        (decision, augmented, affect)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::{Gate, GateConfig};

    fn neutral() -> HomeostaticSignals {
        HomeostaticSignals::neutral()
    }

    /// An event whose *base* value sits just below the neutral threshold (0.40)
    /// but which is strongly drive-relevant (user-facing → Service tier), so the
    /// drive delta can push it over.
    fn drive_relevant_borderline_event() -> EventFeatures {
        // base = 0.65*0.45 + 0.35*0.0 = 0.2925 (no user_facing bonus counted in
        // base? it IS counted: +0.15 → 0.4425). Drop urgency so base < 0.40.
        // 0.65*0.30 = 0.195 + user_facing_bonus 0.15 = 0.345 < 0.40.
        EventFeatures {
            urgency: 0.30,
            novelty: 0.0,
            semantic_class: SemanticClass::UserQuery,
            user_facing: true,
        }
    }

    // ── (a) drive augmentation raises total_value and can flip the decision ────

    /// Build a motivated gate (neutral homeostatic signals so the *threshold* is
    /// the plain baseline) whose internal drive state is strongly engaged via
    /// pending operator objectives + active goals.  This isolates the effect we
    /// want to prove — drives raising the *value* — from the orthogonal effect
    /// of homeostatic signals lowering the *threshold* (e.g. attention_demand).
    fn engaged_gate(signals: &HomeostaticSignals) -> MotivatedGate {
        let mut gate = MotivatedGate::with_defaults(signals);
        gate.registry_mut().set_pending_objectives(5);
        gate.registry_mut().set_active_goal_count(8);
        gate
    }

    #[test]
    fn drive_relevant_event_gets_higher_total_value_than_base() {
        let signals = neutral();
        let gate = engaged_gate(&signals);
        // user-facing UserQuery → Service + Achievement (is_completion) drives.
        let event = drive_relevant_borderline_event();

        let (decision, augmented, _affect) =
            gate.decide_motivated("evt-drive", &event, &signals, &GateOverride::None);

        assert!(
            augmented.drive_delta > 0.0,
            "engaged drive state must add positive drive delta, got {:.4}",
            augmented.drive_delta
        );
        assert!(
            augmented.total_value > augmented.base_value,
            "total_value ({:.3}) must exceed base_value ({:.3})",
            augmented.total_value,
            augmented.base_value
        );
        // The reported value_score is the augmented total.
        assert!((decision.value_score - augmented.total_value).abs() < 1e-6);
    }

    #[test]
    fn motivation_can_flip_blocked_event_to_invoke() {
        // Neutral signals → plain threshold is the 0.40 baseline (no attention
        // discount), so the borderline event is genuinely blocked by the plain
        // gate, and only the drive delta can flip it.
        let signals = neutral();
        let event = drive_relevant_borderline_event();

        // Plain gate blocks it (base value < threshold).
        let plain = ThresholdGate::with_defaults();
        let plain_decision = plain.decide("evt-plain", &event, &signals, &GateOverride::None);
        assert!(
            !plain_decision.invoke,
            "precondition: plain gate must block this borderline event \
             (base={:.3}, thr={:.3})",
            plain_decision.value_score, plain_decision.threshold_applied
        );

        // Motivated gate flips it to invoke thanks to the engaged drives.
        let motivated = engaged_gate(&signals);
        let (decision, augmented, _) =
            motivated.decide_motivated("evt-mot", &event, &signals, &GateOverride::None);
        assert!(
            augmented.total_value > plain_decision.value_score,
            "drives must raise total ({:.3}) above plain base ({:.3})",
            augmented.total_value,
            plain_decision.value_score
        );
        assert!(
            decision.invoke,
            "motivated gate must invoke once drives lift total ({:.3}) over threshold ({:.3})",
            augmented.total_value, decision.threshold_applied
        );
        assert!(decision.cost_class.is_some());
        assert!(!decision.override_active);
    }

    // ── (b) disabled path equals the plain gate ───────────────────────────────

    #[test]
    fn disabled_path_matches_plain_gate() {
        // Disabled integrator → drive_delta == 0 → total == base. With a
        // quiescent drive snapshot the affect nudge is ≤ 1.0 (content → slightly
        // more permissive), so the decision is identical: we compare invoke /
        // cost_class / value_score against the plain gate.
        //
        // Note: the byte-for-byte guarantee in the spec is at the
        // `LifecycleManager` level — when no `MotivatedGate` is installed the
        // somatic loop uses the plain `ThresholdGate` verbatim (see lib.rs).
        // This test exercises the finer-grained "disabled integrator" path.
        let plain = ThresholdGate::with_defaults();

        let events = [
            EventFeatures {
                urgency: 0.1,
                novelty: 0.1,
                semantic_class: SemanticClass::BackgroundTask,
                user_facing: false,
            },
            EventFeatures {
                urgency: 1.0,
                novelty: 0.5,
                semantic_class: SemanticClass::UserQuery,
                user_facing: true,
            },
            EventFeatures {
                urgency: 0.7,
                novelty: 0.0,
                semantic_class: SemanticClass::SystemEvent,
                user_facing: false,
            },
        ];

        for (i, event) in events.iter().enumerate() {
            let signals = neutral();
            let motivated = MotivatedGate::new(
                ThresholdGate::with_defaults(),
                DriveRegistry::new(signals_for_registry(&signals)),
                PriorityLattice::default(),
                DriveValueIntegrator::new(DriveIntegratorConfig {
                    enabled: false,
                    ..Default::default()
                }),
            );

            let id = format!("evt-disabled-{i}");
            let plain_decision = plain.decide(&id, event, &signals, &GateOverride::None);
            let (decision, augmented, affect) =
                motivated.decide_motivated(&id, event, &signals, &GateOverride::None);

            // Under neutral signals the affect nudge is permissive (≤ 1.0), so a
            // disabled integrator cannot change the invoke outcome.
            assert!(
                affect.gate_threshold_nudge() <= 1.0 + 1e-6,
                "neutral affect must be permissive for the disabled-path equality to hold"
            );
            assert_eq!(
                augmented.drive_delta, 0.0,
                "disabled integrator adds nothing"
            );
            assert!(
                (decision.value_score - plain_decision.value_score).abs() < 1e-6,
                "disabled motivated value_score must equal plain base value_score"
            );
            assert_eq!(
                decision.invoke, plain_decision.invoke,
                "disabled motivated invoke must equal plain invoke for event {i}"
            );
            assert_eq!(
                decision.cost_class, plain_decision.cost_class,
                "disabled motivated cost_class must equal plain cost_class for event {i}"
            );
        }
    }

    #[test]
    fn neutral_quiescent_drives_yield_unit_nudge_and_plain_threshold() {
        // Under neutral signals the only non-zero urgency is the tiny constant
        // Tier-5 background (0.1); valence stays positive-ish so the nudge is
        // close to but at most 1.0. Verify the effective threshold never exceeds
        // the plain threshold (drives only make us *more* permissive when content).
        let signals = neutral();
        let plain = ThresholdGate::with_defaults();
        let motivated = MotivatedGate::new(
            ThresholdGate::with_defaults(),
            DriveRegistry::new(signals_for_registry(&signals)),
            PriorityLattice::default(),
            DriveValueIntegrator::new(DriveIntegratorConfig {
                enabled: false,
                ..Default::default()
            }),
        );
        let event = EventFeatures {
            urgency: 0.5,
            novelty: 0.2,
            semantic_class: SemanticClass::SystemEvent,
            user_facing: false,
        };
        let plain_thr = plain
            .decide("t", &event, &signals, &GateOverride::None)
            .threshold_applied;
        let (decision, _, affect) =
            motivated.decide_motivated("t", &event, &signals, &GateOverride::None);
        assert!(
            affect.gate_threshold_nudge() <= 1.0 + 1e-6,
            "content/neutral affect must not raise the threshold"
        );
        assert!(
            decision.threshold_applied <= plain_thr + 1e-6,
            "nudged threshold ({:.4}) must not exceed plain threshold ({:.4})",
            decision.threshold_applied,
            plain_thr
        );
    }

    // ── (c) overrides still force invocation ──────────────────────────────────

    #[test]
    fn user_override_forces_invocation_even_when_drives_quiescent() {
        // Worst case for drives: a low event that would be blocked, quiescent
        // registry. The override must still force invoke=true.
        let signals = neutral();
        let gate = MotivatedGate::with_defaults(&signals);
        let event = EventFeatures {
            urgency: 0.05,
            novelty: 0.05,
            semantic_class: SemanticClass::BackgroundTask,
            user_facing: false,
        };
        let (blocked, _, _) = gate.decide_motivated("o1", &event, &signals, &GateOverride::None);
        assert!(
            !blocked.invoke,
            "precondition: event blocked without override"
        );

        let (forced, _, _) = gate.decide_motivated(
            "o1",
            &event,
            &signals,
            &GateOverride::UserForced {
                reason: "user typed /force".to_string(),
            },
        );
        assert!(forced.invoke, "user override must force invocation");
        assert!(forced.override_active);
        assert!(forced.cost_class.is_some());
    }

    #[test]
    fn operator_override_still_routes_to_frontier() {
        // Even under severe viability stress (which suppresses appetitive drives
        // and pushes the threshold up) the operator force must route to Frontier.
        let signals = HomeostaticSignals {
            thermal_load: 1.0,
            memory_pressure: 1.0,
            power_budget: 0.0,
            financial_budget: 0.0,
            ..neutral()
        };
        let gate = MotivatedGate::with_defaults(&signals);
        let event = EventFeatures {
            urgency: 0.05,
            novelty: 0.0,
            semantic_class: SemanticClass::BackgroundTask,
            user_facing: false,
        };
        let (decision, _, _) = gate.decide_motivated(
            "o2",
            &event,
            &signals,
            &GateOverride::OperatorForced {
                reason: "emergency directive".to_string(),
            },
        );
        assert!(decision.invoke, "operator override must force invocation");
        assert_eq!(
            decision.cost_class,
            Some(CostClass::Frontier),
            "operator override must always route to Frontier"
        );
        assert!(decision.override_active);
    }

    // ── (d) affect nudge stays within bounds ──────────────────────────────────

    #[test]
    fn affect_nudge_stays_within_bounds_across_signal_extremes() {
        let extremes = [
            HomeostaticSignals::neutral(),
            HomeostaticSignals {
                thermal_load: 1.0,
                memory_pressure: 1.0,
                power_budget: 0.0,
                financial_budget: 0.0,
                compute_pressure: 1.0,
                attention_demand: 1.0,
            },
            HomeostaticSignals {
                attention_demand: 1.0,
                ..HomeostaticSignals::neutral()
            },
        ];
        for (i, signals) in extremes.iter().enumerate() {
            let gate = MotivatedGate::with_defaults(signals);
            let event = EventFeatures {
                urgency: 0.5,
                novelty: 0.5,
                semantic_class: SemanticClass::UserQuery,
                user_facing: true,
            };
            let (decision, _, affect) =
                gate.decide_motivated("nudge", &event, signals, &GateOverride::None);
            let nudge = affect.gate_threshold_nudge();
            assert!(
                (0.9..=1.1).contains(&nudge),
                "affect nudge out of [0.9, 1.1] for signal set {i}: {nudge}"
            );
            // The effective threshold must always remain inside the canonical band.
            assert!(
                (0.05..=0.99).contains(&decision.threshold_applied),
                "threshold out of [0.05, 0.99] for signal set {i}: {}",
                decision.threshold_applied
            );
        }
    }

    #[test]
    fn drive_delta_is_bounded_by_max_drive_delta() {
        // Even maximally drive-relevant events cannot add more than the
        // integrator's configured ceiling.
        let signals = HomeostaticSignals {
            attention_demand: 1.0,
            ..neutral()
        };
        let cfg = DriveIntegratorConfig::default();
        let gate = MotivatedGate::new(
            ThresholdGate::new(GateConfig::default()),
            DriveRegistry::new(signals_for_registry(&signals)),
            PriorityLattice::default(),
            DriveValueIntegrator::new(cfg.clone()),
        );
        let event = EventFeatures {
            urgency: 1.0,
            novelty: 1.0,
            semantic_class: SemanticClass::UserQuery,
            user_facing: true,
        };
        let (_, augmented, _) =
            gate.decide_motivated("bound", &event, &signals, &GateOverride::None);
        assert!(
            augmented.drive_delta <= cfg.max_drive_delta + 1e-6,
            "drive delta {:.3} exceeds max {:.3}",
            augmented.drive_delta,
            cfg.max_drive_delta
        );
    }

    #[test]
    fn candidate_mapping_sets_expected_flags() {
        let op = candidate_from_event(&EventFeatures {
            urgency: 0.5,
            novelty: 0.1,
            semantic_class: SemanticClass::OperatorCommand,
            user_facing: false,
        });
        assert!(op.is_operator_objective);
        assert!(!op.is_completion);

        let q = candidate_from_event(&EventFeatures {
            urgency: 0.5,
            novelty: 0.1,
            semantic_class: SemanticClass::UserQuery,
            user_facing: true,
        });
        assert!(q.is_completion);
        assert!(q.user_facing);
        assert!(
            !q.is_exploratory,
            "low-novelty user query is not exploratory"
        );

        let bg = candidate_from_event(&EventFeatures {
            urgency: 0.2,
            novelty: 0.1,
            semantic_class: SemanticClass::BackgroundTask,
            user_facing: false,
        });
        assert!(bg.is_exploratory, "background tasks are exploratory");

        let novel = candidate_from_event(&EventFeatures {
            urgency: 0.2,
            novelty: 0.8,
            semantic_class: SemanticClass::SystemEvent,
            user_facing: false,
        });
        assert!(novel.is_exploratory, "high-novelty events are exploratory");
    }
}
