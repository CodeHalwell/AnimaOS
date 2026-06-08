//! Striatal Gate and Thalamic Router analytics — S25.3.
//!
//! Derives gate invocation rates, cost-class distributions, routing
//! modulation frequency, and gate efficiency from `GateDecision` and
//! `RouterModulated` audit entries.

use serde::{Deserialize, Serialize};
use vita::audit::AuditEntry;

// ── CostClassCount ────────────────────────────────────────────────────────────

/// Invocation count for a single gate cost class.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CostClassCount {
    /// Cost class label (`"CheapLocal"`, `"MidTier"`, or `"Frontier"`).
    pub cost_class: String,
    /// Number of invocations routed to this cost class.
    pub count: usize,
}

// ── GateReport ────────────────────────────────────────────────────────────────

/// Gate and routing analytics report.
///
/// Produced by [`compute_gate_report`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GateReport {
    /// Total gate evaluations in the window.
    pub total_evaluations: usize,
    /// Evaluations that resulted in a cortex invocation.
    pub invocations: usize,
    /// Evaluations that blocked the event.
    pub blocks: usize,
    /// Percentage of evaluations that resulted in an invocation.
    ///
    /// `0.0` when no evaluations were observed.
    pub invocation_rate_pct: f64,
    /// Cost-class distribution among invocations (descending by count).
    pub by_cost_class: Vec<CostClassCount>,
    /// Number of `RouterModulated` entries (route was downgraded by homeostatic pressure).
    pub route_modulations: usize,
    /// Mean value score across all evaluations.
    pub mean_value_score: f64,
    /// Mean adaptive threshold across all evaluations.
    pub mean_threshold: f64,
    /// Number of override decisions (`GateDecision.override_active = true`).
    pub overrides: usize,
    /// Gate efficiency: fraction of invocations where value_score > threshold.
    ///
    /// A score near 1.0 means the gate is well-calibrated: it invokes when the
    /// event genuinely clears the threshold.  A score near 0.0 indicates
    /// overrides are driving most invocations.
    pub efficiency_pct: f64,
}

// ── compute_gate_report ───────────────────────────────────────────────────────

/// Fold `entries` into a [`GateReport`].
pub fn compute_gate_report(entries: &[AuditEntry]) -> GateReport {
    use std::collections::HashMap;

    let mut total_evaluations = 0usize;
    let mut invocations = 0usize;
    let mut blocks = 0usize;
    let mut cost_class_map: HashMap<String, usize> = HashMap::new();
    let mut route_modulations = 0usize;
    let mut value_score_sum = 0.0f64;
    let mut threshold_sum = 0.0f64;
    let mut overrides = 0usize;
    let mut efficient_invocations = 0usize;

    for entry in entries {
        match entry {
            AuditEntry::GateDecision {
                invoke,
                cost_class,
                value_score,
                threshold_applied,
                override_active,
                ..
            } => {
                total_evaluations += 1;
                value_score_sum += *value_score as f64;
                threshold_sum += *threshold_applied as f64;
                if *invoke {
                    invocations += 1;
                    if let Some(class) = cost_class {
                        *cost_class_map.entry(class.clone()).or_insert(0) += 1;
                    }
                    if *value_score > *threshold_applied {
                        efficient_invocations += 1;
                    }
                } else {
                    blocks += 1;
                }
                if *override_active {
                    overrides += 1;
                }
            }
            AuditEntry::RouterModulated { .. } => {
                route_modulations += 1;
            }
            _ => {}
        }
    }

    let invocation_rate_pct = if total_evaluations > 0 {
        invocations as f64 / total_evaluations as f64 * 100.0
    } else {
        0.0
    };

    let mean_value_score = if total_evaluations > 0 {
        value_score_sum / total_evaluations as f64
    } else {
        0.0
    };

    let mean_threshold = if total_evaluations > 0 {
        threshold_sum / total_evaluations as f64
    } else {
        0.0
    };

    let efficiency_pct = if invocations > 0 {
        efficient_invocations as f64 / invocations as f64 * 100.0
    } else {
        0.0
    };

    let mut by_cost_class: Vec<CostClassCount> = cost_class_map
        .into_iter()
        .map(|(cost_class, count)| CostClassCount { cost_class, count })
        .collect();
    by_cost_class.sort_by(|a, b| b.count.cmp(&a.count).then(a.cost_class.cmp(&b.cost_class)));

    GateReport {
        total_evaluations,
        invocations,
        blocks,
        invocation_rate_pct,
        by_cost_class,
        route_modulations,
        mean_value_score,
        mean_threshold,
        overrides,
        efficiency_pct,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use vita::audit::AuditEntry;

    fn gate(
        invoke: bool,
        cost_class: Option<&str>,
        value_score: f32,
        threshold: f32,
        ov: bool,
    ) -> AuditEntry {
        AuditEntry::GateDecision {
            agent_id: "a".into(),
            event_id: "e1".into(),
            invoke,
            cost_class: cost_class.map(str::to_string),
            urgency: 0.5,
            novelty: 0.5,
            user_facing: false,
            semantic_class: "background".into(),
            value_score,
            threshold_applied: threshold,
            thermal_load: 0.0,
            compute_pressure: 0.0,
            memory_pressure: 0.0,
            power_budget: 1.0,
            financial_budget: 1.0,
            attention_demand: 0.5,
            reasoning: "test".into(),
            override_active: ov,
        }
    }

    fn modulated() -> AuditEntry {
        AuditEntry::RouterModulated {
            agent_id: "a".into(),
            event_id: "e1".into(),
            requested_route_id: "frontier".into(),
            effective_route_id: "mid-tier".into(),
            reason: "thermal".into(),
        }
    }

    #[test]
    fn empty_entries_produce_zero_report() {
        let r = compute_gate_report(&[]);
        assert_eq!(r.total_evaluations, 0);
        assert_eq!(r.invocations, 0);
        assert_eq!(r.invocation_rate_pct, 0.0);
        assert!(r.by_cost_class.is_empty());
    }

    #[test]
    fn invocation_rate_computed_correctly() {
        let entries = vec![
            gate(true, Some("MidTier"), 0.6, 0.4, false),
            gate(false, None, 0.3, 0.4, false),
        ];
        let r = compute_gate_report(&entries);
        assert_eq!(r.total_evaluations, 2);
        assert_eq!(r.invocations, 1);
        assert_eq!(r.blocks, 1);
        assert!((r.invocation_rate_pct - 50.0).abs() < 1e-9);
    }

    #[test]
    fn cost_class_distribution_sorted_by_count_descending() {
        let entries = vec![
            gate(true, Some("CheapLocal"), 0.6, 0.4, false),
            gate(true, Some("CheapLocal"), 0.6, 0.4, false),
            gate(true, Some("Frontier"), 0.9, 0.4, false),
        ];
        let r = compute_gate_report(&entries);
        assert_eq!(r.by_cost_class[0].cost_class, "CheapLocal");
        assert_eq!(r.by_cost_class[0].count, 2);
    }

    #[test]
    fn route_modulations_counted() {
        let entries = vec![modulated(), modulated()];
        let r = compute_gate_report(&entries);
        assert_eq!(r.route_modulations, 2);
    }

    #[test]
    fn override_decisions_counted() {
        let entries = vec![
            gate(true, Some("Frontier"), 0.3, 0.4, true),
            gate(true, Some("MidTier"), 0.7, 0.4, false),
        ];
        let r = compute_gate_report(&entries);
        assert_eq!(r.overrides, 1);
    }

    #[test]
    fn efficiency_is_100_when_all_invocations_clear_threshold() {
        let entries = vec![
            gate(true, Some("MidTier"), 0.8, 0.4, false),
            gate(true, Some("MidTier"), 0.9, 0.4, false),
        ];
        let r = compute_gate_report(&entries);
        assert!((r.efficiency_pct - 100.0).abs() < 1e-9);
    }

    #[test]
    fn efficiency_is_zero_when_all_invocations_are_overrides() {
        // value_score=0.3 < threshold=0.4 but override=true forces invoke=true
        let entries = vec![gate(true, Some("MidTier"), 0.3, 0.4, true)];
        let r = compute_gate_report(&entries);
        assert_eq!(r.efficiency_pct, 0.0);
    }

    #[test]
    fn mean_scores_computed_correctly() {
        let entries = vec![
            gate(true, Some("MidTier"), 0.6, 0.4, false),
            gate(false, None, 0.2, 0.4, false),
        ];
        let r = compute_gate_report(&entries);
        assert!((r.mean_value_score - 0.4).abs() < 1e-6);
        assert!((r.mean_threshold - 0.4).abs() < 1e-6);
    }
}
