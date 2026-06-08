//! [`DiagnosticReport`] — aggregated result of all diagnostic checks.

use crate::check::{CheckResult, DiagnosticCheck, HealthStatus};
use crate::AuditSnapshot;
use serde::Serialize;

/// The aggregated result of running all (or a subset of) diagnostic checks.
#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticReport {
    /// Individual check results, in the order they were run.
    pub results: Vec<CheckResult>,
    /// Aggregate health status — the worst status across all checks.
    pub overall_status: HealthStatus,
    /// Number of checks with `Healthy` status.
    pub healthy_count: usize,
    /// Number of checks with `Degraded` status.
    pub degraded_count: usize,
    /// Number of checks with `Critical` status.
    pub critical_count: usize,
    /// Number of checks with `Unknown` status.
    pub unknown_count: usize,
    /// Snapshot statistics used to produce this report.
    pub audit_entries_analysed: u64,
}

impl DiagnosticReport {
    /// Run all provided checks against the snapshot and aggregate into a report.
    pub fn run(snapshot: &AuditSnapshot, checks: &[Box<dyn DiagnosticCheck>]) -> Self {
        let results: Vec<CheckResult> = checks.iter().map(|c| c.run(snapshot)).collect();

        let mut overall = HealthStatus::Healthy;
        let mut healthy_count = 0usize;
        let mut degraded_count = 0usize;
        let mut critical_count = 0usize;
        let mut unknown_count = 0usize;

        for r in &results {
            overall = HealthStatus::worst(overall, r.status.clone());
            match r.status {
                HealthStatus::Healthy => healthy_count += 1,
                HealthStatus::Degraded => degraded_count += 1,
                HealthStatus::Critical => critical_count += 1,
                HealthStatus::Unknown => unknown_count += 1,
            }
        }

        Self {
            results,
            overall_status: overall,
            healthy_count,
            degraded_count,
            critical_count,
            unknown_count,
            audit_entries_analysed: snapshot.total_audit_entries,
        }
    }

    /// Returns only the checks that need operator attention (Degraded or Critical).
    pub fn actionable_items(&self) -> impl Iterator<Item = &CheckResult> {
        self.results.iter().filter(|r| r.status.needs_attention())
    }

    /// `true` when the overall status is `Healthy` (no issues).
    pub fn is_healthy(&self) -> bool {
        self.overall_status == HealthStatus::Healthy
    }

    /// Render a human-readable text summary of the report.
    pub fn render_text(&self) -> String {
        let mut out = String::new();

        // Header
        let status_icon = match self.overall_status {
            HealthStatus::Healthy => "✅",
            HealthStatus::Degraded => "⚠️ ",
            HealthStatus::Critical => "🚨",
            HealthStatus::Unknown => "❓",
        };
        out.push_str(&format!(
            "{status_icon} AnimaOS Diagnostic Report — overall: {:?}\n",
            self.overall_status
        ));
        out.push_str(&format!(
            "   {} healthy  {} degraded  {} critical  {} unknown  ({} audit entries analysed)\n",
            self.healthy_count,
            self.degraded_count,
            self.critical_count,
            self.unknown_count,
            self.audit_entries_analysed,
        ));
        out.push_str(&"─".repeat(72));
        out.push('\n');

        // Per-check results
        for result in &self.results {
            let icon = match result.status {
                HealthStatus::Healthy => "  ✅",
                HealthStatus::Degraded => "  ⚠️ ",
                HealthStatus::Critical => "  🚨",
                HealthStatus::Unknown => "  ❓",
            };
            out.push_str(&format!(
                "{icon} {}: {}\n",
                result.display_name, result.summary
            ));
            if let Some(ref remediation) = result.remediation {
                out.push_str(&format!("        💡 {remediation}\n"));
            }
        }

        // Footer
        if self.is_healthy() {
            out.push_str(&"─".repeat(72));
            out.push('\n');
            out.push_str("All checks passed. The agent is operating normally.\n");
        } else {
            out.push_str(&"─".repeat(72));
            out.push('\n');
            let count = self.degraded_count + self.critical_count;
            out.push_str(&format!(
                "{count} check(s) require attention. Review the 💡 remediation hints above.\n"
            ));
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checks::all_checks;

    #[test]
    fn empty_snapshot_produces_report_with_unknown_and_healthy_statuses() {
        let snap = AuditSnapshot::default();
        let checks = all_checks();
        let report = DiagnosticReport::run(&snap, &checks);
        // Checks with no data return Unknown; the remaining return Healthy or Unknown.
        // Overall should not be Critical on an empty snapshot.
        assert_ne!(report.overall_status, HealthStatus::Critical);
        assert!(report.healthy_count + report.unknown_count == report.results.len());
    }

    #[test]
    fn critical_snapshot_produces_critical_overall_status() {
        let snap = AuditSnapshot {
            tasks_dispatched: 100,
            task_failures: 50, // 50% — critical
            total_audit_entries: 150,
            ..Default::default()
        };
        let checks = all_checks();
        let report = DiagnosticReport::run(&snap, &checks);
        assert_eq!(report.overall_status, HealthStatus::Critical);
        assert!(report.critical_count > 0);
    }

    #[test]
    fn healthy_snapshot_shows_all_healthy() {
        let snap = AuditSnapshot {
            tasks_dispatched: 1000,
            task_failures: 10, // 1% — healthy
            cortex_invocations: 50,
            cortex_faults: 2, // 4% — healthy
            sleep_cycles_ok: 20,
            last_l1_tokens: 1000,
            last_l1_max_context: 4096,
            last_financial_budget: 0.80,
            last_thermal_load: 0.40,
            total_audit_entries: 5000,
            ..Default::default()
        };
        let checks = all_checks();
        let report = DiagnosticReport::run(&snap, &checks);
        assert_eq!(report.overall_status, HealthStatus::Healthy);
        assert_eq!(report.critical_count, 0);
        assert_eq!(report.degraded_count, 0);
        assert!(report.is_healthy());
    }

    #[test]
    fn render_text_contains_check_names() {
        let snap = AuditSnapshot::default();
        let checks = all_checks();
        let report = DiagnosticReport::run(&snap, &checks);
        let text = report.render_text();
        assert!(text.contains("Task Failure Rate"));
        assert!(text.contains("Cortex Fault Rate"));
        assert!(text.contains("L1 Memory Pressure"));
    }

    #[test]
    fn actionable_items_only_returns_degraded_and_critical() {
        let snap = AuditSnapshot {
            tasks_dispatched: 100,
            task_failures: 10, // 10% — degraded
            total_audit_entries: 110,
            ..Default::default()
        };
        let checks = all_checks();
        let report = DiagnosticReport::run(&snap, &checks);
        let actionable: Vec<_> = report.actionable_items().collect();
        for r in &actionable {
            assert!(r.status.needs_attention(), "check_id={}", r.check_id);
        }
        assert!(
            !actionable.is_empty(),
            "expected at least one actionable item"
        );
    }

    #[test]
    fn report_serializes_to_json() {
        let snap = AuditSnapshot::default();
        let checks = all_checks();
        let report = DiagnosticReport::run(&snap, &checks);
        let json = serde_json::to_string(&report).expect("serialisation should not fail");
        assert!(json.contains("overall_status"));
        assert!(json.contains("results"));
    }
}
