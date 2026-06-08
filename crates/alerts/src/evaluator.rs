//! Pure alert evaluation — no I/O, no side-effects.

use metrics::AgentMetrics;
use serde::{Deserialize, Serialize};

use crate::rule::{AlertRule, AlertSeverity, MetricField};
use crate::state::{AlertStateTracker, StateTransition};

// ── AlertEvent ────────────────────────────────────────────────────────────────

/// An event produced by evaluating alert rules against a metrics snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertEvent {
    /// Rule identifier that produced this event.
    pub rule_id: String,
    /// Human-readable description from the rule.
    pub description: String,
    /// Kind of event.
    pub kind: AlertEventKind,
    /// Severity of the rule.
    pub severity: AlertSeverity,
    /// The metric field that was tested.
    pub field: MetricField,
    /// The observed value at evaluation time.
    pub actual_value: f64,
    /// The rule threshold.
    pub threshold: f64,
}

/// Whether this event represents a new fire or a resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertEventKind {
    /// The rule just entered the `Firing` state.
    Fired,
    /// The rule just returned to the `Normal` state.
    Resolved,
}

// ── extract_field ─────────────────────────────────────────────────────────────

/// Extract the f64 value for a [`MetricField`] from an [`AgentMetrics`] snapshot.
pub fn extract_field(metrics: &AgentMetrics, field: &MetricField) -> f64 {
    match field {
        MetricField::TaskSuccessRate => metrics.task_success_rate,
        MetricField::TasksFailed => metrics.tasks_failed as f64,
        MetricField::GateInvokeRate => metrics.gate_invoke_rate,
        MetricField::GateMeanValueScore => metrics.gate_mean_value_score,
        MetricField::CortexFaultRate => metrics.cortex_fault_rate,
        MetricField::CortexMeanLatencyMs => metrics.cortex_mean_latency_ms,
        MetricField::TotalVetoes => (metrics.defence_vetoes + metrics.constitution_vetoes) as f64,
        MetricField::SleepCycles => metrics.sleep_cycles as f64,
        MetricField::MeanThermalLoad => metrics.mean_thermal_load,
        MetricField::MeanMemoryPressure => metrics.mean_memory_pressure,
        MetricField::MeanFinancialBudget => metrics.mean_financial_budget,
        MetricField::TotalTokensEmitted => metrics.total_tokens_emitted as f64,
        MetricField::RouterModulations => metrics.router_modulations as f64,
    }
}

// ── evaluate ─────────────────────────────────────────────────────────────────

/// Evaluate a slice of rules against a metrics snapshot, updating state
/// trackers, and returning any [`AlertEvent`]s generated.
///
/// Rules without a corresponding tracker get a new `Normal` tracker created
/// inline (first call).  Pass the same `trackers` slice across calls to
/// preserve firing state across evaluation windows.
pub fn evaluate(
    metrics: &AgentMetrics,
    rules: &[AlertRule],
    trackers: &mut Vec<AlertStateTracker>,
) -> Vec<AlertEvent> {
    let mut events = Vec::new();

    for rule in rules {
        if !rule.enabled {
            continue;
        }

        let actual = extract_field(metrics, &rule.condition.field);
        let fires = rule.condition.op.evaluate(actual, rule.condition.threshold);

        // Find or create the tracker for this rule.
        let tracker = if let Some(pos) = trackers.iter().position(|t| t.rule_id == rule.id) {
            &mut trackers[pos]
        } else {
            trackers.push(AlertStateTracker::new(&rule.id));
            trackers.last_mut().unwrap()
        };

        match tracker.advance(fires) {
            StateTransition::NewlyFiring => {
                events.push(AlertEvent {
                    rule_id: rule.id.clone(),
                    description: rule.description.clone(),
                    kind: AlertEventKind::Fired,
                    severity: rule.severity,
                    field: rule.condition.field.clone(),
                    actual_value: actual,
                    threshold: rule.condition.threshold,
                });
            }
            StateTransition::Resolved => {
                events.push(AlertEvent {
                    rule_id: rule.id.clone(),
                    description: rule.description.clone(),
                    kind: AlertEventKind::Resolved,
                    severity: rule.severity,
                    field: rule.condition.field.clone(),
                    actual_value: actual,
                    threshold: rule.condition.threshold,
                });
            }
            StateTransition::StillFiring | StateTransition::StillNormal => {
                // No new event.
            }
        }
    }

    events
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::{AlertCondition, ComparisonOp};
    use metrics::aggregate;

    fn empty_metrics() -> AgentMetrics {
        aggregate(&[])
    }

    fn make_rule(id: &str, field: MetricField, op: ComparisonOp, threshold: f64) -> AlertRule {
        AlertRule::new(
            id,
            format!("{id} description"),
            AlertCondition::new(field, op, threshold),
            AlertSeverity::Warning,
        )
    }

    #[test]
    fn evaluate_fires_when_condition_met() {
        let m = empty_metrics();
        // tasks_failed = 0 — which is not > 5 so rule shouldn't fire.
        // Use a condition that fires on zero metrics: cortex_fault_rate = 0.0, test < 1.0.
        let rule = make_rule(
            "r1",
            MetricField::CortexFaultRate,
            ComparisonOp::LessThan,
            0.5,
        );
        let rules = vec![rule];
        let mut trackers = vec![];
        let events = evaluate(&m, &rules, &mut trackers);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, AlertEventKind::Fired);
        assert_eq!(events[0].rule_id, "r1");
    }

    #[test]
    fn evaluate_no_event_when_condition_not_met() {
        let m = empty_metrics();
        // cortex_fault_rate = 0.0, condition > 0.5 should not fire.
        let rule = make_rule(
            "r2",
            MetricField::CortexFaultRate,
            ComparisonOp::GreaterThan,
            0.5,
        );
        let rules = vec![rule];
        let mut trackers = vec![];
        let events = evaluate(&m, &rules, &mut trackers);
        assert!(events.is_empty());
    }

    #[test]
    fn evaluate_suppresses_duplicate_fire_events() {
        let m = empty_metrics();
        let rule = make_rule(
            "r3",
            MetricField::TaskSuccessRate,
            ComparisonOp::LessThan,
            0.5,
        );
        let rules = vec![rule];
        let mut trackers = vec![];
        // First call — fires.
        let ev1 = evaluate(&m, &rules, &mut trackers);
        assert_eq!(ev1.len(), 1);
        assert_eq!(ev1[0].kind, AlertEventKind::Fired);
        // Second call — same metrics, still firing but NOT a new event.
        let ev2 = evaluate(&m, &rules, &mut trackers);
        assert!(ev2.is_empty(), "duplicate fire should be suppressed");
    }

    #[test]
    fn evaluate_generates_resolved_event_when_condition_clears() {
        // Use a rule that fires on zero tokens and resolves when tokens > 0.
        let rule = make_rule(
            "r4",
            MetricField::TotalTokensEmitted,
            ComparisonOp::LessThan,
            1.0,
        );
        let rules = vec![rule];
        let mut trackers = vec![];

        fn m_tokens(n: u64) -> AgentMetrics {
            let mut m = aggregate(&[]);
            m.total_tokens_emitted = n;
            m
        }

        // Zero tokens — condition fires.
        evaluate(&m_tokens(0), &rules, &mut trackers);
        // 100 tokens — condition resolves.
        let events = evaluate(&m_tokens(100), &rules, &mut trackers);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, AlertEventKind::Resolved);
    }

    #[test]
    fn evaluate_disabled_rule_produces_no_events() {
        let m = empty_metrics();
        let mut rule = make_rule(
            "r5",
            MetricField::CortexFaultRate,
            ComparisonOp::LessThan,
            0.5,
        );
        rule.enabled = false;
        let rules = vec![rule];
        let mut trackers = vec![];
        let events = evaluate(&m, &rules, &mut trackers);
        assert!(events.is_empty());
    }

    #[test]
    fn evaluate_multiple_rules_independently() {
        let m = empty_metrics();
        let rules = vec![
            make_rule(
                "fire1",
                MetricField::TaskSuccessRate,
                ComparisonOp::LessThan,
                0.5,
            ),
            make_rule(
                "nofire",
                MetricField::TotalVetoes,
                ComparisonOp::GreaterThan,
                100.0,
            ),
            make_rule(
                "fire2",
                MetricField::RouterModulations,
                ComparisonOp::LessThanOrEqual,
                999.0,
            ),
        ];
        let mut trackers = vec![];
        let events = evaluate(&m, &rules, &mut trackers);
        let fired_ids: Vec<&str> = events.iter().map(|e| e.rule_id.as_str()).collect();
        assert!(fired_ids.contains(&"fire1"));
        assert!(fired_ids.contains(&"fire2"));
        assert!(!fired_ids.contains(&"nofire"));
    }

    #[test]
    fn extract_field_covers_all_metric_fields() {
        let m = empty_metrics();
        // Just verify no panic on all variants.
        let fields = [
            MetricField::TaskSuccessRate,
            MetricField::TasksFailed,
            MetricField::GateInvokeRate,
            MetricField::GateMeanValueScore,
            MetricField::CortexFaultRate,
            MetricField::CortexMeanLatencyMs,
            MetricField::TotalVetoes,
            MetricField::SleepCycles,
            MetricField::MeanThermalLoad,
            MetricField::MeanMemoryPressure,
            MetricField::MeanFinancialBudget,
            MetricField::TotalTokensEmitted,
            MetricField::RouterModulations,
        ];
        for field in &fields {
            let _ = extract_field(&m, field);
        }
    }

    #[test]
    fn alert_event_json_round_trips() {
        let m = empty_metrics();
        let rule = make_rule(
            "json-ev",
            MetricField::GateInvokeRate,
            ComparisonOp::LessThan,
            1.0,
        );
        let rules = vec![rule];
        let mut trackers = vec![];
        let events = evaluate(&m, &rules, &mut trackers);
        assert_eq!(events.len(), 1);
        let json = serde_json::to_string(&events[0]).unwrap();
        let recovered: AlertEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(events[0], recovered);
    }

    #[test]
    fn evaluate_creates_tracker_on_first_call() {
        let m = empty_metrics();
        let rule = make_rule(
            "new-tracker",
            MetricField::SleepCycles,
            ComparisonOp::LessThanOrEqual,
            999.0,
        );
        let rules = vec![rule];
        let mut trackers: Vec<AlertStateTracker> = vec![];
        evaluate(&m, &rules, &mut trackers);
        assert_eq!(trackers.len(), 1);
        assert_eq!(trackers[0].rule_id, "new-tracker");
    }
}
