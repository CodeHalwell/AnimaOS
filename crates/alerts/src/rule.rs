//! Alert rule definitions.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

// ── MetricField ───────────────────────────────────────────────────────────────

/// Identifies a scalar field inside [`AgentMetrics`] that can be compared
/// against a threshold.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricField {
    /// `AgentMetrics::task_success_rate` — fraction of started tasks completed.
    TaskSuccessRate,
    /// `AgentMetrics::tasks_failed` — total tasks failed.
    TasksFailed,
    /// `AgentMetrics::gate_invoke_rate` — fraction of gate evaluations invoking the cortex.
    GateInvokeRate,
    /// `AgentMetrics::gate_mean_value_score` — mean value score at the gate.
    GateMeanValueScore,
    /// `AgentMetrics::cortex_fault_rate` — fraction of cortex invocations that faulted.
    CortexFaultRate,
    /// `AgentMetrics::cortex_mean_latency_ms` — mean time-to-first-action (ms).
    CortexMeanLatencyMs,
    /// `AgentMetrics::total_vetoes` — cumulative defence-layer vetoes.
    TotalVetoes,
    /// `AgentMetrics::sleep_cycles` — sleep cycles entered.
    SleepCycles,
    /// `AgentMetrics::mean_thermal_load` — mean interoceptive thermal load.
    MeanThermalLoad,
    /// `AgentMetrics::mean_memory_pressure` — mean interoceptive memory pressure.
    MeanMemoryPressure,
    /// `AgentMetrics::mean_financial_budget` — mean remaining financial budget.
    MeanFinancialBudget,
    /// `AgentMetrics::total_tokens_emitted` — total tokens emitted by the agent.
    TotalTokensEmitted,
    /// `AgentMetrics::router_modulations` — stress-triggered route downgrades.
    RouterModulations,
}

impl fmt::Display for MetricField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            MetricField::TaskSuccessRate => "task_success_rate",
            MetricField::TasksFailed => "tasks_failed",
            MetricField::GateInvokeRate => "gate_invoke_rate",
            MetricField::GateMeanValueScore => "gate_mean_value_score",
            MetricField::CortexFaultRate => "cortex_fault_rate",
            MetricField::CortexMeanLatencyMs => "cortex_mean_latency_ms",
            MetricField::TotalVetoes => "total_vetoes",
            MetricField::SleepCycles => "sleep_cycles",
            MetricField::MeanThermalLoad => "mean_thermal_load",
            MetricField::MeanMemoryPressure => "mean_memory_pressure",
            MetricField::MeanFinancialBudget => "mean_financial_budget",
            MetricField::TotalTokensEmitted => "total_tokens_emitted",
            MetricField::RouterModulations => "router_modulations",
        };
        f.write_str(s)
    }
}

impl FromStr for MetricField {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "task_success_rate" => Ok(MetricField::TaskSuccessRate),
            "tasks_failed" => Ok(MetricField::TasksFailed),
            "gate_invoke_rate" => Ok(MetricField::GateInvokeRate),
            "gate_mean_value_score" => Ok(MetricField::GateMeanValueScore),
            "cortex_fault_rate" => Ok(MetricField::CortexFaultRate),
            "cortex_mean_latency_ms" => Ok(MetricField::CortexMeanLatencyMs),
            "total_vetoes" => Ok(MetricField::TotalVetoes),
            "sleep_cycles" => Ok(MetricField::SleepCycles),
            "mean_thermal_load" => Ok(MetricField::MeanThermalLoad),
            "mean_memory_pressure" => Ok(MetricField::MeanMemoryPressure),
            "mean_financial_budget" => Ok(MetricField::MeanFinancialBudget),
            "total_tokens_emitted" => Ok(MetricField::TotalTokensEmitted),
            "router_modulations" => Ok(MetricField::RouterModulations),
            other => Err(format!("unknown metric field: {other}")),
        }
    }
}

// ── ComparisonOp ──────────────────────────────────────────────────────────────

/// Comparison operator for alert conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonOp {
    /// Fires when `actual > threshold`.
    GreaterThan,
    /// Fires when `actual >= threshold`.
    GreaterThanOrEqual,
    /// Fires when `actual < threshold`.
    LessThan,
    /// Fires when `actual <= threshold`.
    LessThanOrEqual,
}

impl ComparisonOp {
    /// Evaluate the comparison: returns `true` when the condition fires.
    pub fn evaluate(self, actual: f64, threshold: f64) -> bool {
        match self {
            ComparisonOp::GreaterThan => actual > threshold,
            ComparisonOp::GreaterThanOrEqual => actual >= threshold,
            ComparisonOp::LessThan => actual < threshold,
            ComparisonOp::LessThanOrEqual => actual <= threshold,
        }
    }
}

impl fmt::Display for ComparisonOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ComparisonOp::GreaterThan => ">",
            ComparisonOp::GreaterThanOrEqual => ">=",
            ComparisonOp::LessThan => "<",
            ComparisonOp::LessThanOrEqual => "<=",
        })
    }
}

impl FromStr for ComparisonOp {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            ">" | "gt" => Ok(ComparisonOp::GreaterThan),
            ">=" | "gte" => Ok(ComparisonOp::GreaterThanOrEqual),
            "<" | "lt" => Ok(ComparisonOp::LessThan),
            "<=" | "lte" => Ok(ComparisonOp::LessThanOrEqual),
            other => Err(format!("unknown comparison operator: {other}")),
        }
    }
}

// ── AlertSeverity ─────────────────────────────────────────────────────────────

/// Severity level associated with a firing alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertSeverity {
    /// Informational — worth noting but not actionable.
    Info,
    /// Warning — operator should investigate.
    Warning,
    /// Critical — requires immediate attention.
    Critical,
}

impl fmt::Display for AlertSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            AlertSeverity::Info => "info",
            AlertSeverity::Warning => "warning",
            AlertSeverity::Critical => "critical",
        })
    }
}

impl FromStr for AlertSeverity {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "info" => Ok(AlertSeverity::Info),
            "warning" | "warn" => Ok(AlertSeverity::Warning),
            "critical" | "crit" => Ok(AlertSeverity::Critical),
            other => Err(format!("unknown severity: {other}")),
        }
    }
}

// ── AlertCondition ────────────────────────────────────────────────────────────

/// A single threshold condition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertCondition {
    /// Which metric field to test.
    pub field: MetricField,
    /// How to compare the actual value to the threshold.
    pub op: ComparisonOp,
    /// The numeric threshold to compare against.
    pub threshold: f64,
}

impl AlertCondition {
    /// Create a new condition.
    pub fn new(field: MetricField, op: ComparisonOp, threshold: f64) -> Self {
        Self {
            field,
            op,
            threshold,
        }
    }
}

impl fmt::Display for AlertCondition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {:.4}", self.field, self.op, self.threshold)
    }
}

// ── AlertRule ─────────────────────────────────────────────────────────────────

/// A named alert rule combining a condition, severity, and optional annotation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertRule {
    /// Stable unique identifier (e.g. `"high-cortex-fault-rate"`).
    pub id: String,
    /// Human-readable description shown in the audit log and CLI.
    pub description: String,
    /// The threshold condition that triggers this alert.
    pub condition: AlertCondition,
    /// Severity emitted when this rule fires.
    pub severity: AlertSeverity,
    /// Whether this rule is active (disabled rules are stored but never evaluated).
    pub enabled: bool,
}

impl AlertRule {
    /// Create a new enabled alert rule.
    pub fn new(
        id: impl Into<String>,
        description: impl Into<String>,
        condition: AlertCondition,
        severity: AlertSeverity,
    ) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            condition,
            severity,
            enabled: true,
        }
    }

    /// Returns `true` if the condition fires against `actual_value`.
    pub fn fires(&self, actual_value: f64) -> bool {
        self.enabled
            && self
                .condition
                .op
                .evaluate(actual_value, self.condition.threshold)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparison_op_greater_than_fires_correctly() {
        assert!(ComparisonOp::GreaterThan.evaluate(0.6, 0.5));
        assert!(!ComparisonOp::GreaterThan.evaluate(0.5, 0.5));
        assert!(!ComparisonOp::GreaterThan.evaluate(0.4, 0.5));
    }

    #[test]
    fn comparison_op_less_than_fires_correctly() {
        assert!(ComparisonOp::LessThan.evaluate(0.3, 0.5));
        assert!(!ComparisonOp::LessThan.evaluate(0.5, 0.5));
        assert!(!ComparisonOp::LessThan.evaluate(0.7, 0.5));
    }

    #[test]
    fn comparison_op_gte_lte_boundary() {
        assert!(ComparisonOp::GreaterThanOrEqual.evaluate(0.5, 0.5));
        assert!(ComparisonOp::LessThanOrEqual.evaluate(0.5, 0.5));
    }

    #[test]
    fn comparison_op_from_str_all_variants() {
        assert_eq!(
            ">".parse::<ComparisonOp>().unwrap(),
            ComparisonOp::GreaterThan
        );
        assert_eq!(
            ">=".parse::<ComparisonOp>().unwrap(),
            ComparisonOp::GreaterThanOrEqual
        );
        assert_eq!("<".parse::<ComparisonOp>().unwrap(), ComparisonOp::LessThan);
        assert_eq!(
            "<=".parse::<ComparisonOp>().unwrap(),
            ComparisonOp::LessThanOrEqual
        );
        assert_eq!(
            "gt".parse::<ComparisonOp>().unwrap(),
            ComparisonOp::GreaterThan
        );
        assert_eq!(
            "gte".parse::<ComparisonOp>().unwrap(),
            ComparisonOp::GreaterThanOrEqual
        );
        assert_eq!(
            "lt".parse::<ComparisonOp>().unwrap(),
            ComparisonOp::LessThan
        );
        assert_eq!(
            "lte".parse::<ComparisonOp>().unwrap(),
            ComparisonOp::LessThanOrEqual
        );
    }

    #[test]
    fn comparison_op_from_str_rejects_unknown() {
        assert!("==".parse::<ComparisonOp>().is_err());
    }

    #[test]
    fn metric_field_round_trips_through_str() {
        let fields = [
            MetricField::TaskSuccessRate,
            MetricField::CortexFaultRate,
            MetricField::MeanThermalLoad,
            MetricField::TotalVetoes,
        ];
        for field in &fields {
            let s = field.to_string();
            let parsed: MetricField = s.parse().expect("round-trip failed");
            assert_eq!(&parsed, field);
        }
    }

    #[test]
    fn alert_severity_ordering_is_info_warning_critical() {
        assert!(AlertSeverity::Info < AlertSeverity::Warning);
        assert!(AlertSeverity::Warning < AlertSeverity::Critical);
    }

    #[test]
    fn alert_rule_fires_when_condition_met() {
        let rule = AlertRule::new(
            "test-rule",
            "Test",
            AlertCondition::new(MetricField::CortexFaultRate, ComparisonOp::GreaterThan, 0.1),
            AlertSeverity::Warning,
        );
        assert!(rule.fires(0.2));
        assert!(!rule.fires(0.05));
        assert!(!rule.fires(0.1)); // not > 0.1, only >=
    }

    #[test]
    fn disabled_rule_never_fires() {
        let mut rule = AlertRule::new(
            "disabled-rule",
            "Disabled",
            AlertCondition::new(MetricField::TotalVetoes, ComparisonOp::GreaterThan, 0.0),
            AlertSeverity::Critical,
        );
        rule.enabled = false;
        assert!(!rule.fires(999.0));
    }

    #[test]
    fn alert_rule_json_round_trips() {
        let rule = AlertRule::new(
            "json-rule",
            "JSON round-trip test",
            AlertCondition::new(MetricField::GateInvokeRate, ComparisonOp::LessThan, 0.3),
            AlertSeverity::Info,
        );
        let json = serde_json::to_string(&rule).unwrap();
        let recovered: AlertRule = serde_json::from_str(&json).unwrap();
        assert_eq!(rule, recovered);
    }
}
