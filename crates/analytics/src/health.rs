//! Overall agent health scoring — S25.4.
//!
//! Produces a composite health score and letter grade by combining the
//! task success rate, cortex reliability, defence health, and gate
//! efficiency from the other sub-reports.  The score is interpretable:
//! actionable recommendations are generated when a factor falls below
//! its healthy operating range.

use serde::{Deserialize, Serialize};
use vita::audit::AuditEntry;

use crate::{gate::GateReport, latency::LatencyReport, token::TokenReport};

// ── HealthGrade ───────────────────────────────────────────────────────────────

/// Letter grade derived from the composite health score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HealthGrade {
    /// Score ≥ 0.90 — excellent.
    A,
    /// Score ≥ 0.75 — good.
    B,
    /// Score ≥ 0.60 — acceptable.
    C,
    /// Score ≥ 0.40 — degraded.
    D,
    /// Score < 0.40 — critical.
    F,
}

impl HealthGrade {
    /// Derive a grade from a score in `[0.0, 1.0]`.
    pub fn from_score(score: f64) -> Self {
        if score >= 0.90 {
            HealthGrade::A
        } else if score >= 0.75 {
            HealthGrade::B
        } else if score >= 0.60 {
            HealthGrade::C
        } else if score >= 0.40 {
            HealthGrade::D
        } else {
            HealthGrade::F
        }
    }

    /// Single-character label.
    pub fn label(self) -> &'static str {
        match self {
            HealthGrade::A => "A",
            HealthGrade::B => "B",
            HealthGrade::C => "C",
            HealthGrade::D => "D",
            HealthGrade::F => "F",
        }
    }
}

// ── HealthFactors ─────────────────────────────────────────────────────────────

/// Individual factors contributing to the composite health score.
///
/// Each factor is in `[0.0, 1.0]`; higher is better.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HealthFactors {
    /// Task success rate: `completed / (completed + failed)`.
    pub task_success_rate: f64,
    /// Cortex reliability: `1 − fault_rate_pct / 100`.
    pub cortex_reliability: f64,
    /// Defence health: `1 − veto_rate` (fraction of completions that were vetoed).
    pub defence_health: f64,
    /// Gate efficiency fraction (invocations that organically cleared threshold).
    pub gate_efficiency: f64,
}

// ── HealthReport ──────────────────────────────────────────────────────────────

/// Overall agent health assessment.
///
/// Produced by [`compute_health_report`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HealthReport {
    /// Composite health score in `[0.0, 1.0]` (higher = healthier).
    pub score: f64,
    /// Letter grade derived from the score.
    pub grade: String,
    /// Per-factor breakdown.
    pub factors: HealthFactors,
    /// Actionable recommendations when a factor is below its healthy threshold.
    pub recommendations: Vec<String>,
    /// Raw count context: total tasks (completed + failed).
    pub total_tasks: usize,
    /// Raw count context: defence vetoes observed.
    pub defence_vetoes: usize,
}

// ── compute_health_report ─────────────────────────────────────────────────────

/// Derive a [`HealthReport`] from sub-reports and raw entries.
///
/// # Weighting
///
/// | Factor              | Weight |
/// |---------------------|--------|
/// | Task success rate   | 35 %   |
/// | Cortex reliability  | 30 %   |
/// | Defence health      | 20 %   |
/// | Gate efficiency     | 15 %   |
pub fn compute_health_report(
    entries: &[AuditEntry],
    token_report: &TokenReport,
    latency_report: &LatencyReport,
    gate_report: &GateReport,
) -> HealthReport {
    // Task success rate.
    let total_tasks = token_report.tasks_completed + token_report.tasks_failed;
    let task_success_rate = if total_tasks > 0 {
        token_report.tasks_completed as f64 / total_tasks as f64
    } else {
        1.0
    };

    // Cortex reliability.
    let cortex_reliability = 1.0 - (latency_report.fault_rate_pct / 100.0).clamp(0.0, 1.0);

    // Defence health: count vetoes relative to cortex completions.
    let mut defence_vetoes = 0usize;
    for entry in entries {
        if matches!(
            entry,
            AuditEntry::DefenceVeto { .. } | AuditEntry::ConstitutionVeto { .. }
        ) {
            defence_vetoes += 1;
        }
    }
    let completions = latency_report
        .cortex_invocations
        .saturating_sub(latency_report.cortex_faults);
    let veto_rate = if completions > 0 {
        (defence_vetoes as f64 / completions as f64).clamp(0.0, 1.0)
    } else if defence_vetoes > 0 {
        1.0
    } else {
        0.0
    };
    let defence_health = 1.0 - veto_rate;

    // Gate efficiency: when there are no invocations there is no evidence of
    // inefficiency, so treat as 1.0 (neutral / healthy) rather than 0.0.
    let gate_efficiency = if gate_report.invocations > 0 {
        gate_report.efficiency_pct / 100.0
    } else {
        1.0
    };

    // Composite score (weighted sum).
    let score = (task_success_rate * 0.35
        + cortex_reliability * 0.30
        + defence_health * 0.20
        + gate_efficiency * 0.15)
        .clamp(0.0, 1.0);

    let grade = HealthGrade::from_score(score);

    // Recommendations.
    let mut recommendations: Vec<String> = Vec::new();

    if task_success_rate < 0.95 {
        recommendations.push(format!(
            "Task failure rate is {:.1}% — check backend connectivity and error logs",
            (1.0 - task_success_rate) * 100.0
        ));
    }
    if latency_report.fault_rate_pct > 5.0 {
        recommendations.push(format!(
            "Cortex fault rate is {:.1}% — inspect cortex process logs and IPC bridge",
            latency_report.fault_rate_pct
        ));
    }
    if defence_vetoes > 0 {
        if completions == 0 {
            recommendations.push(format!(
                "{} defence vetoes with 0 completions — review cortex policy or tool access scope",
                defence_vetoes
            ));
        } else if veto_rate > 0.10 {
            recommendations.push(format!(
                "{} defence vetoes across {} completions ({:.1}%) — review cortex policy or tool access scope",
                defence_vetoes, completions, veto_rate * 100.0
            ));
        }
    }
    if gate_report.route_modulations > 0 && gate_report.invocations > 0 {
        let mod_rate =
            gate_report.route_modulations as f64 / gate_report.invocations as f64 * 100.0;
        if mod_rate > 20.0 {
            recommendations.push(format!(
                "Router modulation rate is {:.1}% — homeostatic signals are frequently constraining the agent; \
                 consider reviewing power/financial budget configuration",
                mod_rate
            ));
        }
    }

    HealthReport {
        score,
        grade: grade.label().to_string(),
        factors: HealthFactors {
            task_success_rate,
            cortex_reliability,
            defence_health,
            gate_efficiency,
        },
        recommendations,
        total_tasks,
        defence_vetoes,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        gate::compute_gate_report, latency::compute_latency_report, token::compute_token_report,
    };

    #[test]
    fn grade_boundaries_are_correct() {
        assert_eq!(HealthGrade::from_score(0.95), HealthGrade::A);
        assert_eq!(HealthGrade::from_score(0.90), HealthGrade::A);
        assert_eq!(HealthGrade::from_score(0.89), HealthGrade::B);
        assert_eq!(HealthGrade::from_score(0.75), HealthGrade::B);
        assert_eq!(HealthGrade::from_score(0.74), HealthGrade::C);
        assert_eq!(HealthGrade::from_score(0.60), HealthGrade::C);
        assert_eq!(HealthGrade::from_score(0.59), HealthGrade::D);
        assert_eq!(HealthGrade::from_score(0.40), HealthGrade::D);
        assert_eq!(HealthGrade::from_score(0.39), HealthGrade::F);
        assert_eq!(HealthGrade::from_score(0.0), HealthGrade::F);
    }

    #[test]
    fn grade_labels_are_single_chars() {
        for &(g, label) in &[
            (HealthGrade::A, "A"),
            (HealthGrade::B, "B"),
            (HealthGrade::C, "C"),
            (HealthGrade::D, "D"),
            (HealthGrade::F, "F"),
        ] {
            assert_eq!(g.label(), label);
        }
    }

    #[test]
    fn empty_entries_produce_grade_a() {
        let entries: &[vita::audit::AuditEntry] = &[];
        let token = compute_token_report(entries);
        let latency = compute_latency_report(entries);
        let gate = compute_gate_report(entries);
        let health = compute_health_report(entries, &token, &latency, &gate);
        // No data → all factors default to their healthy baseline; no invocations
        // means gate efficiency is treated as neutral (1.0) not zero.
        assert_eq!(health.grade, "A");
        assert!(health.score >= 0.90);
        assert!(health.recommendations.is_empty());
    }

    #[test]
    fn all_tasks_failed_degrades_health_significantly() {
        use vita::audit::AuditEntry;
        let entries: Vec<AuditEntry> = (0..5)
            .map(|i| AuditEntry::TaskFailed {
                agent_id: "a".into(),
                task_id: i,
                error: "err".into(),
            })
            .collect();
        let token = compute_token_report(&entries);
        let latency = compute_latency_report(&entries);
        let gate = compute_gate_report(&entries);
        let health = compute_health_report(&entries, &token, &latency, &gate);
        // task_success_rate = 0 → score at most 0.65 (non-task factors at 1.0)
        assert!(health.score < 0.70);
        assert!(!health.recommendations.is_empty());
    }

    #[test]
    fn high_cortex_fault_rate_triggers_recommendation() {
        use vita::audit::AuditEntry;
        let mut entries: Vec<AuditEntry> = Vec::new();
        // 10 invocations, 6 faults → 60% fault rate
        for _ in 0..10 {
            entries.push(AuditEntry::CortexInvoked {
                task_id: "t".into(),
                latency_to_first_action_ms: 50,
            });
        }
        for _ in 0..6 {
            entries.push(AuditEntry::CortexFault {
                task_id: "t".into(),
                error: "err".into(),
            });
        }
        let token = compute_token_report(&entries);
        let latency = compute_latency_report(&entries);
        let gate = compute_gate_report(&entries);
        let health = compute_health_report(&entries, &token, &latency, &gate);
        assert!(health
            .recommendations
            .iter()
            .any(|r| r.contains("fault rate")));
    }

    #[test]
    fn health_score_is_clamped_to_unit_interval() {
        let entries: &[vita::audit::AuditEntry] = &[];
        let token = compute_token_report(entries);
        let latency = compute_latency_report(entries);
        let gate = compute_gate_report(entries);
        let health = compute_health_report(entries, &token, &latency, &gate);
        assert!(health.score >= 0.0 && health.score <= 1.0);
    }

    #[test]
    fn defence_vetoes_counted_correctly() {
        use vita::audit::AuditEntry;
        let entries = vec![
            AuditEntry::CortexInvoked {
                task_id: "t".into(),
                latency_to_first_action_ms: 50,
            },
            AuditEntry::CortexCompleted {
                task_id: "t".into(),
                tool_calls: 1,
                summary_len: 10,
            },
            AuditEntry::DefenceVeto {
                agent_id: "a".into(),
                invocation_id: "i".into(),
                detector: "test".into(),
                action_blocked: "x".into(),
                reason: "y".into(),
            },
        ];
        let token = compute_token_report(&entries);
        let latency = compute_latency_report(&entries);
        let gate = compute_gate_report(&entries);
        let health = compute_health_report(&entries, &token, &latency, &gate);
        assert_eq!(health.defence_vetoes, 1);
    }
}
