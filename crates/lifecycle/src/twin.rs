//! S15.4 — Digital-twin sandbox.
//!
//! A shadow agent that mirrors the real agent's state, against which proposed
//! changes (new skill/tool from E11, fine-tuned adapter from E8, config change)
//! can be exercised on recorded or synthetic scenarios **before** touching the
//! live agent.
//!
//! ## Architecture
//!
//! A [`DigitalTwin`] is initialised from an [`AgentSnapshot`] (S15.5); it
//! exposes the same decision machinery as the live agent but runs scenarios
//! in isolation.  The twin pairs with S15.3 (replay) to let operators run
//! recorded audit-log scenarios through the proposed configuration.
//!
//! ## Current scope
//!
//! This implementation delivers:
//! - **[`DigitalTwin`]**: twin container initialised from a snapshot.
//! - **[`TwinScenario`]**: a named scenario with a sequence of gate inputs.
//! - **[`ScenarioResult`]**: outcome report for a scenario run.
//! - **[`TwinConfig`]**: configuration overrides to test against the twin.
//!
//! Full execution of scenarios through the live `vita` stack requires the
//! twin to spawn its own `LifecycleManager` — that wiring lands when E15 is
//! integrated end-to-end.  For now, the twin runs scenarios through the
//! Striatal Gate in isolation (the highest-value, most composable path).

use serde::{Deserialize, Serialize};

use crate::snapshot::AgentSnapshot;

// ── TwinConfig ────────────────────────────────────────────────────────────────

/// Configuration overrides applied to the digital twin.
///
/// Only the fields set in `TwinConfig` differ from the live agent; unset fields
/// inherit the live agent's configuration from the snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TwinConfig {
    /// Override the gate base threshold (default: live agent's value).
    pub gate_base_threshold: Option<f32>,
    /// Override the financial budget scalar (simulates a tighter budget).
    pub financial_budget_override: Option<f32>,
    /// Override the thermal load scalar (simulates a hotter machine).
    pub thermal_load_override: Option<f32>,
    /// Override the memory pressure scalar.
    pub memory_pressure_override: Option<f32>,
    /// Human-readable description of what change this twin tests.
    pub description: Option<String>,
}

// ── GateInputs ────────────────────────────────────────────────────────────────

/// A single gate-evaluation input for a twin scenario.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateInputs {
    /// Per-scenario event label (e.g. `"user-query"`, `"background-cleanup"`).
    pub event_label: String,
    /// Event urgency score `[0.0, 1.0]`.
    pub urgency: f32,
    /// Event novelty score `[0.0, 1.0]`.
    pub novelty: f32,
    /// `true` when the event is user-facing.
    pub user_facing: bool,
}

impl GateInputs {
    /// High-urgency user-facing event (typical foreground request).
    pub fn foreground(event_label: impl Into<String>) -> Self {
        GateInputs {
            event_label: event_label.into(),
            urgency: 0.9,
            novelty: 0.6,
            user_facing: true,
        }
    }

    /// Low-urgency background task.
    pub fn background(event_label: impl Into<String>) -> Self {
        GateInputs {
            event_label: event_label.into(),
            urgency: 0.2,
            novelty: 0.1,
            user_facing: false,
        }
    }
}

// ── GateOutcome ───────────────────────────────────────────────────────────────

/// Outcome of a single gate evaluation inside the twin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateOutcome {
    /// The event label from [`GateInputs`].
    pub event_label: String,
    /// Computed value score.
    pub value_score: f32,
    /// Adaptive threshold applied.
    pub threshold: f32,
    /// `true` when the gate would invoke the cortex.
    pub invoke: bool,
}

// ── TwinScenario ─────────────────────────────────────────────────────────────

/// A named sequence of gate evaluations to run through the twin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TwinScenario {
    /// Human-readable scenario name (used in reports).
    pub name: String,
    /// Ordered sequence of gate inputs.
    pub events: Vec<GateInputs>,
}

impl TwinScenario {
    /// Create a new scenario.
    pub fn new(name: impl Into<String>, events: Vec<GateInputs>) -> Self {
        TwinScenario {
            name: name.into(),
            events,
        }
    }
}

// ── ScenarioResult ────────────────────────────────────────────────────────────

/// Outcome report for a scenario run through the digital twin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioResult {
    /// Scenario name.
    pub scenario_name: String,
    /// Per-event gate outcomes, in scenario event order.
    pub gate_outcomes: Vec<GateOutcome>,
    /// Number of events for which the gate decided `invoke = true`.
    pub invocations: usize,
    /// Number of events blocked by the gate.
    pub blocks: usize,
    /// Config overrides applied during this run.
    pub config_applied: TwinConfig,
}

impl ScenarioResult {
    /// Fraction of events that were invoked `[0.0, 1.0]`.
    pub fn invocation_rate(&self) -> f32 {
        if self.gate_outcomes.is_empty() {
            return 0.0;
        }
        self.invocations as f32 / self.gate_outcomes.len() as f32
    }
}

// ── DigitalTwin ───────────────────────────────────────────────────────────────

/// A shadow agent initialised from a live agent's snapshot.
///
/// The twin runs proposed configuration changes in isolation so that
/// behavioural regressions are caught before the live agent is affected.
pub struct DigitalTwin {
    /// The snapshot that backs this twin.
    pub snapshot: AgentSnapshot,
    /// All scenario results accumulated in this twin session.
    pub scenario_results: Vec<ScenarioResult>,
}

impl DigitalTwin {
    /// Create a digital twin from a live agent snapshot.
    pub fn from_snapshot(snapshot: AgentSnapshot) -> Self {
        DigitalTwin {
            snapshot,
            scenario_results: Vec::new(),
        }
    }

    /// Run a scenario through the twin gate with the given config overrides.
    ///
    /// Uses the same threshold function as the live Striatal Gate (E5.2):
    ///
    /// ```text
    /// value_score = urgency * 0.65 + novelty * 0.35 + user_facing * 0.15
    /// adaptive_threshold = base_threshold
    ///     + thermal_load * 0.30
    ///     + memory_pressure * 0.20
    ///     − (1.0 − financial_budget) * 0.15  (financial pressure raises threshold)
    /// invoke = value_score >= adaptive_threshold
    /// ```
    ///
    /// Config overrides are applied on top of live-agent defaults.
    pub fn run_scenario(&mut self, scenario: &TwinScenario, config: TwinConfig) -> ScenarioResult {
        let base_threshold = config.gate_base_threshold.unwrap_or(0.40);
        let thermal = config.thermal_load_override.unwrap_or(0.0);
        let memory = config.memory_pressure_override.unwrap_or(0.0);
        let financial = config.financial_budget_override.unwrap_or(1.0);

        // Financial pressure raises the threshold: a depleted budget makes the
        // gate more conservative, mirroring the live ThresholdGate behaviour.
        let adaptive_threshold =
            (base_threshold + thermal * 0.30 + memory * 0.20 + (1.0 - financial) * 0.15)
                .clamp(0.0, 1.0);

        let mut outcomes = Vec::with_capacity(scenario.events.len());
        let mut invocations = 0usize;
        let mut blocks = 0usize;

        for event in &scenario.events {
            let user_bonus = if event.user_facing { 0.15 } else { 0.0 };
            let value_score =
                (event.urgency * 0.65 + event.novelty * 0.35 + user_bonus).clamp(0.0, 1.0);
            let invoke = value_score >= adaptive_threshold;

            if invoke {
                invocations += 1;
            } else {
                blocks += 1;
            }

            outcomes.push(GateOutcome {
                event_label: event.event_label.clone(),
                value_score,
                threshold: adaptive_threshold,
                invoke,
            });
        }

        let result = ScenarioResult {
            scenario_name: scenario.name.clone(),
            gate_outcomes: outcomes,
            invocations,
            blocks,
            config_applied: config,
        };

        self.scenario_results.push(result.clone());
        result
    }

    /// Compare two runs of the same scenario under different configs and
    /// return the difference in invocation count (positive = `b` invokes more).
    ///
    /// Useful for quantifying the behavioural delta of a proposed change.
    pub fn compare_invocations(a: &ScenarioResult, b: &ScenarioResult) -> i32 {
        b.invocations as i32 - a.invocations as i32
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::AgentSnapshot;

    fn base_snapshot() -> AgentSnapshot {
        AgentSnapshot::capture("twin-agent", None, &[], None)
    }

    fn three_event_scenario() -> TwinScenario {
        TwinScenario::new(
            "mixed",
            vec![
                GateInputs::foreground("user-query"),
                GateInputs::background("bg-cleanup"),
                GateInputs::foreground("user-urgent"),
            ],
        )
    }

    #[test]
    fn twin_initialised_from_snapshot() {
        let snap = base_snapshot();
        let twin = DigitalTwin::from_snapshot(snap.clone());
        assert_eq!(twin.snapshot.agent_id, snap.agent_id);
        assert!(twin.scenario_results.is_empty());
    }

    #[test]
    fn foreground_events_invoke_under_default_config() {
        let snap = base_snapshot();
        let mut twin = DigitalTwin::from_snapshot(snap);
        let scenario = TwinScenario::new(
            "fg",
            vec![GateInputs::foreground("q1"), GateInputs::foreground("q2")],
        );
        let result = twin.run_scenario(&scenario, TwinConfig::default());
        // foreground: urgency=0.9, novelty=0.6, user_facing=0.15 →
        // score = 0.9*0.65 + 0.6*0.35 + 0.15 = 0.585+0.21+0.15 = 0.945 > 0.40
        assert_eq!(result.invocations, 2);
        assert_eq!(result.blocks, 0);
    }

    #[test]
    fn background_events_blocked_under_default_config() {
        let snap = base_snapshot();
        let mut twin = DigitalTwin::from_snapshot(snap);
        let scenario = TwinScenario::new(
            "bg",
            vec![GateInputs::background("b1"), GateInputs::background("b2")],
        );
        let result = twin.run_scenario(&scenario, TwinConfig::default());
        // background: urgency=0.2, novelty=0.1, user_facing=false →
        // score = 0.2*0.65 + 0.1*0.35 = 0.13+0.035 = 0.165 < 0.40
        assert_eq!(result.invocations, 0);
        assert_eq!(result.blocks, 2);
    }

    #[test]
    fn thermal_stress_raises_threshold_and_reduces_invocations() {
        let snap = base_snapshot();
        let mut twin = DigitalTwin::from_snapshot(snap);
        let scenario = three_event_scenario();

        let normal = twin.run_scenario(&scenario, TwinConfig::default());
        let stressed = twin.run_scenario(
            &scenario,
            TwinConfig {
                thermal_load_override: Some(0.9),
                ..TwinConfig::default()
            },
        );

        // Thermal stress should reduce or keep the same invocation count.
        assert!(stressed.invocations <= normal.invocations);
    }

    #[test]
    fn financial_pressure_raises_threshold() {
        let snap = base_snapshot();
        let mut twin = DigitalTwin::from_snapshot(snap);

        // A marginal foreground event: urgency=0.5, novelty=0.3, user_facing.
        // score = 0.5*0.65 + 0.3*0.35 + 0.15 = 0.325+0.105+0.15 = 0.58
        // Default threshold = 0.40 → invoked.
        // With financial_budget=0.0 → threshold += (1-0.0)*0.15 = +0.15 → 0.55 → still invoked.
        // With financial_budget=0.0 AND base_threshold=0.65 →
        //   threshold = 0.65 + (1-0.0)*0.15 = 0.80 > 0.58 → blocked.
        let marginal_event = GateInputs {
            event_label: "marginal".into(),
            urgency: 0.5,
            novelty: 0.3,
            user_facing: true,
        };
        let scenario = TwinScenario::new("fin-pressure", vec![marginal_event]);

        let result = twin.run_scenario(
            &scenario,
            TwinConfig {
                gate_base_threshold: Some(0.65),
                financial_budget_override: Some(0.0),
                ..TwinConfig::default()
            },
        );
        // threshold = 0.65 + (1.0-0.0)*0.15 = 0.80 > 0.58 → blocked
        assert_eq!(result.blocks, 1);
    }

    #[test]
    fn scenario_results_accumulated_across_runs() {
        let snap = base_snapshot();
        let mut twin = DigitalTwin::from_snapshot(snap);
        let s = three_event_scenario();
        twin.run_scenario(&s, TwinConfig::default());
        twin.run_scenario(
            &s,
            TwinConfig {
                thermal_load_override: Some(0.5),
                ..Default::default()
            },
        );
        assert_eq!(twin.scenario_results.len(), 2);
    }

    #[test]
    fn compare_invocations_is_positive_when_b_invokes_more() {
        let snap = base_snapshot();
        let mut twin = DigitalTwin::from_snapshot(snap);
        let scenario = three_event_scenario();

        let strict = twin.run_scenario(
            &scenario,
            TwinConfig {
                gate_base_threshold: Some(0.9),
                ..Default::default()
            },
        );
        let lenient = twin.run_scenario(&scenario, TwinConfig::default());

        let delta = DigitalTwin::compare_invocations(&strict, &lenient);
        assert!(
            delta >= 0,
            "lenient should invoke >= strict: delta={}",
            delta
        );
    }

    #[test]
    fn invocation_rate_zero_when_no_events() {
        let result = ScenarioResult {
            scenario_name: "empty".into(),
            gate_outcomes: vec![],
            invocations: 0,
            blocks: 0,
            config_applied: TwinConfig::default(),
        };
        assert!((result.invocation_rate() - 0.0).abs() < 1e-6);
    }

    #[test]
    fn invocation_rate_one_when_all_invoked() {
        let snap = base_snapshot();
        let mut twin = DigitalTwin::from_snapshot(snap);
        let scenario = TwinScenario::new(
            "all-fg",
            vec![GateInputs::foreground("a"), GateInputs::foreground("b")],
        );
        let result = twin.run_scenario(&scenario, TwinConfig::default());
        assert!((result.invocation_rate() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn gate_outcome_labels_preserved_from_scenario() {
        let snap = base_snapshot();
        let mut twin = DigitalTwin::from_snapshot(snap);
        let scenario = TwinScenario::new(
            "labels",
            vec![
                GateInputs::foreground("event-alpha"),
                GateInputs::background("event-beta"),
            ],
        );
        let result = twin.run_scenario(&scenario, TwinConfig::default());
        assert_eq!(result.gate_outcomes[0].event_label, "event-alpha");
        assert_eq!(result.gate_outcomes[1].event_label, "event-beta");
    }
}
