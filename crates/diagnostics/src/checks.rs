//! Built-in diagnostic checks for all major AnimaOS subsystems.
//!
//! Each check is a zero-sized struct implementing [`DiagnosticCheck`].  The
//! full set of built-in checks is returned by [`all_checks`].

use crate::check::{CheckResult, DiagnosticCheck};
use crate::AuditSnapshot;
use serde_json::json;

// ── Task Scheduler Health ─────────────────────────────────────────────────────

/// Checks whether the task failure rate is within acceptable bounds.
///
/// | Rate          | Status   |
/// |---------------|----------|
/// | < 5 %         | Healthy  |
/// | 5 % – 20 %   | Degraded |
/// | ≥ 20 %        | Critical |
pub struct TaskFailureRateCheck;

impl DiagnosticCheck for TaskFailureRateCheck {
    fn check_id(&self) -> &'static str {
        "task_failure_rate"
    }
    fn display_name(&self) -> &'static str {
        "Task Failure Rate"
    }
    fn run(&self, snapshot: &AuditSnapshot) -> CheckResult {
        if snapshot.tasks_dispatched == 0 {
            return CheckResult::unknown(
                self.check_id(),
                self.display_name(),
                "No tasks have been dispatched yet — insufficient data.",
            );
        }
        let rate = snapshot.task_failure_rate();
        let pct = (rate * 100.0) as u32;

        if rate < 0.05 {
            CheckResult::healthy(
                self.check_id(),
                self.display_name(),
                format!(
                    "Task failure rate is {}% ({}/{} tasks failed).",
                    pct, snapshot.task_failures, snapshot.tasks_dispatched
                ),
            )
            .with_detail(json!({
                "failure_rate": rate,
                "failures": snapshot.task_failures,
                "total": snapshot.tasks_dispatched,
            }))
        } else if rate < 0.20 {
            CheckResult::degraded(
                self.check_id(),
                self.display_name(),
                format!(
                    "Task failure rate is {}% ({}/{} tasks). This is above the 5% warning threshold.",
                    pct, snapshot.task_failures, snapshot.tasks_dispatched
                ),
                "Check recent TaskFailed audit entries for patterns. Consider reducing task complexity \
                 or verifying LLM backend connectivity.",
            )
            .with_detail(json!({
                "failure_rate": rate,
                "failures": snapshot.task_failures,
                "total": snapshot.tasks_dispatched,
            }))
        } else {
            CheckResult::critical(
                self.check_id(),
                self.display_name(),
                format!(
                    "Task failure rate is {}% ({}/{} tasks). More than 1 in 5 tasks are failing.",
                    pct, snapshot.task_failures, snapshot.tasks_dispatched
                ),
                "Inspect the LLM backend configuration (ANIMA_BACKEND), verify API key validity, \
                 and check network connectivity. Run `anima doctor` for a full preflight check.",
            )
            .with_detail(json!({
                "failure_rate": rate,
                "failures": snapshot.task_failures,
                "total": snapshot.tasks_dispatched,
            }))
        }
    }
}

// ── Cortex Health ─────────────────────────────────────────────────────────────

/// Checks whether the cortex fault rate is within acceptable bounds.
///
/// | Rate          | Status   |
/// |---------------|----------|
/// | < 10 %        | Healthy  |
/// | 10 % – 30 %   | Degraded |
/// | ≥ 30 %        | Critical |
pub struct CortexFaultRateCheck;

impl DiagnosticCheck for CortexFaultRateCheck {
    fn check_id(&self) -> &'static str {
        "cortex_fault_rate"
    }
    fn display_name(&self) -> &'static str {
        "Cortex Fault Rate"
    }
    fn run(&self, snapshot: &AuditSnapshot) -> CheckResult {
        if snapshot.cortex_invocations == 0 {
            return CheckResult::unknown(
                self.check_id(),
                self.display_name(),
                "No cortex invocations recorded — check is not applicable.",
            );
        }
        let rate = snapshot.cortex_fault_rate();
        let pct = (rate * 100.0) as u32;

        if rate < 0.10 {
            CheckResult::healthy(
                self.check_id(),
                self.display_name(),
                format!(
                    "Cortex fault rate is {}% ({}/{} invocations).",
                    pct, snapshot.cortex_faults, snapshot.cortex_invocations
                ),
            )
        } else if rate < 0.30 {
            CheckResult::degraded(
                self.check_id(),
                self.display_name(),
                format!(
                    "Cortex fault rate is {}% ({}/{} invocations). Above the 10% warning threshold.",
                    pct, snapshot.cortex_faults, snapshot.cortex_invocations
                ),
                "Check CortexFault audit entries for error patterns. Verify the Python cortex \
                 process is reachable and the UDS socket path is writable.",
            )
        } else {
            CheckResult::critical(
                self.check_id(),
                self.display_name(),
                format!(
                    "Cortex fault rate is {}% ({}/{} invocations). Critical threshold exceeded.",
                    pct, snapshot.cortex_faults, snapshot.cortex_invocations
                ),
                "The cortex process is crashing repeatedly. Inspect `cortex/__main__.py` logs, \
                 check that all Python dependencies are installed, and verify the IPC socket path.",
            )
        }
    }
}

// ── Memory Pressure ───────────────────────────────────────────────────────────

/// Checks the L1 context window fill fraction and critical pressure event count.
///
/// | Condition                              | Status   |
/// |----------------------------------------|----------|
/// | Fill < 75 % and 0 critical events      | Healthy  |
/// | Fill 75–90 % or 1–5 critical events    | Degraded |
/// | Fill ≥ 90 % or > 5 critical events     | Critical |
pub struct MemoryPressureCheck;

impl DiagnosticCheck for MemoryPressureCheck {
    fn check_id(&self) -> &'static str {
        "memory_pressure"
    }
    fn display_name(&self) -> &'static str {
        "L1 Memory Pressure"
    }
    fn run(&self, snapshot: &AuditSnapshot) -> CheckResult {
        let fill = snapshot.l1_fill_fraction();
        let critical_events = snapshot.memory_pressure_critical_events;

        if snapshot.last_l1_max_context == 0 {
            return CheckResult::unknown(
                self.check_id(),
                self.display_name(),
                "No L1 memory pressure events recorded — check is not applicable.",
            );
        }

        let detail = json!({
            "l1_tokens": snapshot.last_l1_tokens,
            "max_context": snapshot.last_l1_max_context,
            "fill_fraction": fill,
            "critical_events": critical_events,
        });

        if fill < 0.75 && critical_events == 0 {
            CheckResult::healthy(
                self.check_id(),
                self.display_name(),
                format!(
                    "L1 context is {:.0}% full ({}/{} tokens, {} critical events).",
                    fill * 100.0,
                    snapshot.last_l1_tokens,
                    snapshot.last_l1_max_context,
                    critical_events
                ),
            )
            .with_detail(detail)
        } else if fill < 0.90 || (1..=5).contains(&critical_events) {
            CheckResult::degraded(
                self.check_id(),
                self.display_name(),
                format!(
                    "L1 context is {:.0}% full with {} critical pressure events.",
                    fill * 100.0,
                    critical_events
                ),
                "Consider reducing task complexity, enabling more frequent sleep cycles to evict \
                 stale L1 entries, or increasing the configured max_context window.",
            )
            .with_detail(detail)
        } else {
            CheckResult::critical(
                self.check_id(),
                self.display_name(),
                format!(
                    "L1 context is {:.0}% full with {} critical pressure events — eviction is aggressive.",
                    fill * 100.0, critical_events
                ),
                "Urgent: reduce task size or token budget immediately. Review the L3 archive capacity \
                 and demotion pipeline. The agent may be losing important context.",
            )
            .with_detail(detail)
        }
    }
}

// ── Financial Budget ──────────────────────────────────────────────────────────

/// Checks the remaining financial budget fraction.
///
/// | Fraction       | Status   |
/// |----------------|----------|
/// | ≥ 0.30         | Healthy  |
/// | 0.10 – 0.30    | Degraded |
/// | < 0.10         | Critical |
pub struct FinancialBudgetCheck;

impl DiagnosticCheck for FinancialBudgetCheck {
    fn check_id(&self) -> &'static str {
        "financial_budget"
    }
    fn display_name(&self) -> &'static str {
        "Financial Budget"
    }
    fn run(&self, snapshot: &AuditSnapshot) -> CheckResult {
        // If no interoceptive snapshot has been recorded, the budget field
        // defaults to 0.0 (worst case). Distinguish "unknown" from "exhausted"
        // by checking total entries.
        if snapshot.total_audit_entries == 0 {
            return CheckResult::unknown(
                self.check_id(),
                self.display_name(),
                "No audit data available — financial budget is unknown.",
            );
        }

        let budget = snapshot.last_financial_budget;
        let pct = (budget * 100.0) as u32;
        let detail = json!({ "financial_budget_fraction": budget });

        if budget >= 0.30 {
            CheckResult::healthy(
                self.check_id(),
                self.display_name(),
                format!("{pct}% of the API budget remains."),
            )
            .with_detail(detail)
        } else if budget >= 0.10 {
            CheckResult::degraded(
                self.check_id(),
                self.display_name(),
                format!("{pct}% of the API budget remains — approaching the daily/monthly limit."),
                "Review recent high-cost frontier-model invocations. Consider reducing the \
                 frontier-tier usage or topping up the budget before the window resets.",
            )
            .with_detail(detail)
        } else {
            CheckResult::critical(
                self.check_id(),
                self.display_name(),
                format!("{pct}% of the API budget remains — near exhaustion."),
                "Stop non-essential frontier API calls immediately. The Striatal Gate will have \
                 raised its threshold automatically; verify this in `anima why`. \
                 Add budget or wait for the window to reset.",
            )
            .with_detail(detail)
        }
    }
}

// ── Thermal Load ─────────────────────────────────────────────────────────────

/// Checks the thermal load on the host.
///
/// | Load       | Status   |
/// |------------|----------|
/// | < 0.70     | Healthy  |
/// | 0.70–0.85  | Degraded |
/// | ≥ 0.85     | Critical |
pub struct ThermalLoadCheck;

impl DiagnosticCheck for ThermalLoadCheck {
    fn check_id(&self) -> &'static str {
        "thermal_load"
    }
    fn display_name(&self) -> &'static str {
        "Thermal Load"
    }
    fn run(&self, snapshot: &AuditSnapshot) -> CheckResult {
        if snapshot.total_audit_entries == 0 {
            return CheckResult::unknown(
                self.check_id(),
                self.display_name(),
                "No interoceptive data — thermal load is unknown.",
            );
        }

        let load = snapshot.last_thermal_load;
        let pct = (load * 100.0) as u32;
        let detail = json!({ "thermal_load": load });

        if load < 0.70 {
            CheckResult::healthy(
                self.check_id(),
                self.display_name(),
                format!("Thermal load is {pct}% — within comfortable operating range."),
            )
            .with_detail(detail)
        } else if load < 0.85 {
            CheckResult::degraded(
                self.check_id(),
                self.display_name(),
                format!("Thermal load is {pct}% — the host is running warm."),
                "The Striatal Gate has raised its threshold to reduce expensive invocations. \
                 Consider closing CPU-intensive background processes or adding cooling.",
            )
            .with_detail(detail)
        } else {
            CheckResult::critical(
                self.check_id(),
                self.display_name(),
                format!("Thermal load is {pct}% — the host is near thermal throttle."),
                "Reduce agent workload immediately. Frontier model routes will be blocked. \
                 Ensure adequate cooling and close all background CPU/GPU loads.",
            )
            .with_detail(detail)
        }
    }
}

// ── Defence System ────────────────────────────────────────────────────────────

/// Checks the defence layer for unusual veto activity.
///
/// | Condition                         | Status   |
/// |-----------------------------------|----------|
/// | 0 vetoes                          | Healthy  |
/// | 1–10 vetoes, 0 escalations        | Degraded |
/// | > 10 vetoes or ≥ 1 escalation     | Critical |
pub struct DefenceVetoCheck;

impl DiagnosticCheck for DefenceVetoCheck {
    fn check_id(&self) -> &'static str {
        "defence_vetoes"
    }
    fn display_name(&self) -> &'static str {
        "Defence Layer Vetoes"
    }
    fn run(&self, snapshot: &AuditSnapshot) -> CheckResult {
        let vetoes = snapshot.defence_vetoes;
        let escalations = snapshot.attention_escalations;
        let detail = json!({
            "total_vetoes": vetoes,
            "attention_escalations": escalations,
        });

        if vetoes == 0 {
            return CheckResult::healthy(
                self.check_id(),
                self.display_name(),
                "No defence vetoes recorded.",
            )
            .with_detail(detail);
        }

        if vetoes <= 10 && escalations == 0 {
            CheckResult::degraded(
                self.check_id(),
                self.display_name(),
                format!("{vetoes} defence veto(es) recorded — some cortex proposals were blocked."),
                "Review DefenceVeto audit entries to determine which detector(s) triggered. \
                 A low veto count may be normal for prompt-injection screening of external data.",
            )
            .with_detail(detail)
        } else {
            CheckResult::critical(
                self.check_id(),
                self.display_name(),
                format!(
                    "{vetoes} vetoes and {escalations} attention escalation(s) recorded. \
                     The cortex is producing unsafe proposals repeatedly.",
                ),
                "Investigate the cortex's behaviour immediately. Review all DefenceVeto and \
                 AttentionDemandEscalated entries. Consider pausing autonomous operation \
                 and inspecting recent tool outputs for prompt-injection attempts.",
            )
            .with_detail(detail)
        }
    }
}

// ── Sleep Cycle Health ────────────────────────────────────────────────────────

/// Checks the health of the sleep maintenance cycle.
///
/// | Condition                     | Status   |
/// |-------------------------------|----------|
/// | No failures                   | Healthy  |
/// | 1–2 failures                  | Degraded |
/// | ≥ 3 failures or rate ≥ 25 %   | Critical |
pub struct SleepCycleHealthCheck;

impl DiagnosticCheck for SleepCycleHealthCheck {
    fn check_id(&self) -> &'static str {
        "sleep_cycle_health"
    }
    fn display_name(&self) -> &'static str {
        "Sleep Cycle Health"
    }
    fn run(&self, snapshot: &AuditSnapshot) -> CheckResult {
        let total = snapshot.sleep_cycles_ok + snapshot.sleep_cycles_failed;
        if total == 0 {
            return CheckResult::unknown(
                self.check_id(),
                self.display_name(),
                "No sleep cycles recorded — check is not applicable.",
            );
        }

        let failures = snapshot.sleep_cycles_failed;
        let rate = snapshot.sleep_failure_rate();
        let detail = json!({
            "ok": snapshot.sleep_cycles_ok,
            "failed": failures,
            "total": total,
            "failure_rate": rate,
        });

        if failures == 0 {
            CheckResult::healthy(
                self.check_id(),
                self.display_name(),
                format!("All {total} sleep cycle phase(s) completed successfully."),
            )
            .with_detail(detail)
        } else if failures <= 2 && rate < 0.25 {
            CheckResult::degraded(
                self.check_id(),
                self.display_name(),
                format!("{failures}/{total} sleep phase(s) failed."),
                "Review SleepPhaseCompleted(success=false) audit entries. The memory pruning, \
                 replay validation, or compilation phase may be encountering errors.",
            )
            .with_detail(detail)
        } else {
            CheckResult::critical(
                self.check_id(),
                self.display_name(),
                format!(
                    "{failures}/{total} sleep phase(s) failed ({:.0}% failure rate). \
                     Memory maintenance is unreliable.",
                    rate * 100.0
                ),
                "The sleep cycle is failing frequently, which means memory consolidation, \
                 replay validation, and training-data compilation are not running reliably. \
                 Check the L3 archive path, disk space, and memory subsystem configuration.",
            )
            .with_detail(detail)
        }
    }
}

// ── KV-Cache Controller ───────────────────────────────────────────────────────

/// Checks the KV-cache gating controller for repeated faults (fallback to LRU).
///
/// | Faults        | Status   |
/// |---------------|----------|
/// | 0             | Healthy  |
/// | 1–3           | Degraded |
/// | ≥ 4           | Critical |
pub struct KvControllerHealthCheck;

impl DiagnosticCheck for KvControllerHealthCheck {
    fn check_id(&self) -> &'static str {
        "kv_controller_health"
    }
    fn display_name(&self) -> &'static str {
        "KV-Cache Controller Health"
    }
    fn run(&self, snapshot: &AuditSnapshot) -> CheckResult {
        let faults = snapshot.kv_controller_faults;
        let detail = json!({ "kv_controller_faults": faults });

        match faults {
            0 => CheckResult::healthy(
                self.check_id(),
                self.display_name(),
                "KV-cache controller has not faulted — semantic gating is active.",
            )
            .with_detail(detail),
            1..=3 => CheckResult::degraded(
                self.check_id(),
                self.display_name(),
                format!("KV-cache controller faulted {faults} time(s) and fell back to LRU."),
                "Check KvControllerFaulted audit entries. The controller may have encountered \
                 an unexpected block feature vector. Review `crates/kv-controller` logs.",
            )
            .with_detail(detail),
            _ => CheckResult::critical(
                self.check_id(),
                self.display_name(),
                format!(
                    "KV-cache controller faulted {faults} time(s). Semantic gating is effectively \
                     disabled — the agent is using plain LRU eviction.",
                ),
                "The controller is in a persistent fault state. Check whether the controller \
                 weights are corrupted. Running `cargo xtask demo --kind retention` can help \
                 diagnose whether the pre-trained weights are intact.",
            )
            .with_detail(detail),
        }
    }
}

// ── Agent Delegation ──────────────────────────────────────────────────────────

/// Checks the A2A agent delegation failure rate.
pub struct AgentDelegationCheck;

impl DiagnosticCheck for AgentDelegationCheck {
    fn check_id(&self) -> &'static str {
        "agent_delegation_failures"
    }
    fn display_name(&self) -> &'static str {
        "Agent Delegation Failures (A2A)"
    }
    fn run(&self, snapshot: &AuditSnapshot) -> CheckResult {
        let failures = snapshot.agent_delegation_failures;
        let detail = json!({ "delegation_failures": failures });

        match failures {
            0 => CheckResult::healthy(
                self.check_id(),
                self.display_name(),
                "No A2A delegation failures recorded.",
            )
            .with_detail(detail),
            1..=5 => CheckResult::degraded(
                self.check_id(),
                self.display_name(),
                format!("{failures} A2A delegation failure(s) recorded."),
                "Check AgentDelegationFailed audit entries. Verify that the target agent IDs \
                 registered in the AgentPool are reachable.",
            )
            .with_detail(detail),
            _ => CheckResult::critical(
                self.check_id(),
                self.display_name(),
                format!(
                    "{failures} A2A delegation failures — the multi-agent subsystem is unreliable."
                ),
                "Inspect AgentDelegationFailed entries for repeated target agent IDs. \
                 Verify that all agents in the pool are healthy and registered correctly.",
            )
            .with_detail(detail),
        }
    }
}

// ── Consolidation / Fine-Tuning Hook ─────────────────────────────────────────

/// Checks whether the sleep-cycle consolidation hook (E8 fine-tuning) has faulted.
pub struct ConsolidationHealthCheck;

impl DiagnosticCheck for ConsolidationHealthCheck {
    fn check_id(&self) -> &'static str {
        "consolidation_health"
    }
    fn display_name(&self) -> &'static str {
        "Sleep-Cycle Consolidation"
    }
    fn run(&self, snapshot: &AuditSnapshot) -> CheckResult {
        let failures = snapshot.consolidation_failures;
        let detail = json!({ "consolidation_failures": failures });

        match failures {
            0 => CheckResult::healthy(
                self.check_id(),
                self.display_name(),
                "Consolidation hook has not faulted.",
            )
            .with_detail(detail),
            _ => CheckResult::degraded(
                self.check_id(),
                self.display_name(),
                format!("{failures} consolidation failure(s) — fine-tuning is not running."),
                "Check ConsolidationFailed audit entries. Verify the fine-tuner configuration \
                 and that ANIMA_FINETUNE_LIVE is set if a real trainer is expected.",
            )
            .with_detail(detail),
        }
    }
}

// ── Router Modulation Frequency ───────────────────────────────────────────────

/// Flags when the router is being modulated very frequently, indicating persistent
/// homeostatic stress that is affecting route quality.
pub struct RouterModulationCheck;

impl DiagnosticCheck for RouterModulationCheck {
    fn check_id(&self) -> &'static str {
        "router_modulation_frequency"
    }
    fn display_name(&self) -> &'static str {
        "Route Modulation Frequency"
    }
    fn run(&self, snapshot: &AuditSnapshot) -> CheckResult {
        if snapshot.cortex_invocations == 0 {
            return CheckResult::unknown(
                self.check_id(),
                self.display_name(),
                "No cortex invocations — modulation check not applicable.",
            );
        }

        let modulations = snapshot.router_modulations;
        let invocations = snapshot.cortex_invocations;
        let rate = modulations as f32 / invocations as f32;
        let pct = (rate * 100.0) as u32;
        let detail = json!({
            "modulations": modulations,
            "invocations": invocations,
            "modulation_rate": rate,
        });

        if rate < 0.20 {
            CheckResult::healthy(
                self.check_id(),
                self.display_name(),
                format!(
                    "Route modulation rate is {pct}% ({modulations}/{invocations} invocations)."
                ),
            )
            .with_detail(detail)
        } else if rate < 0.50 {
            CheckResult::degraded(
                self.check_id(),
                self.display_name(),
                format!(
                    "Route modulation rate is {pct}% — the agent is frequently downgrading routes \
                     due to homeostatic pressure.",
                ),
                "Check thermal_load, memory_pressure, and financial_budget via interoceptive snapshots. \
                 High modulation means the agent is under sustained stress.",
            )
            .with_detail(detail)
        } else {
            CheckResult::critical(
                self.check_id(),
                self.display_name(),
                format!(
                    "Route modulation rate is {pct}% — more than half of all invocations are being \
                     downgraded. The agent is severely constrained.",
                ),
                "Immediate attention required. Review all homeostatic signals (thermal, financial, \
                 power, memory). The agent is operating far below its configured capability tier.",
            )
            .with_detail(detail)
        }
    }
}

// ── Registry ──────────────────────────────────────────────────────────────────

/// Returns the full set of built-in diagnostic checks.
pub fn all_checks() -> Vec<Box<dyn DiagnosticCheck>> {
    vec![
        Box::new(TaskFailureRateCheck),
        Box::new(CortexFaultRateCheck),
        Box::new(MemoryPressureCheck),
        Box::new(FinancialBudgetCheck),
        Box::new(ThermalLoadCheck),
        Box::new(DefenceVetoCheck),
        Box::new(SleepCycleHealthCheck),
        Box::new(KvControllerHealthCheck),
        Box::new(AgentDelegationCheck),
        Box::new(ConsolidationHealthCheck),
        Box::new(RouterModulationCheck),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::HealthStatus;

    fn empty_snap() -> AuditSnapshot {
        AuditSnapshot::default()
    }

    fn snap_with_tasks(dispatched: u64, failures: u64) -> AuditSnapshot {
        AuditSnapshot {
            tasks_dispatched: dispatched,
            task_failures: failures,
            total_audit_entries: dispatched + failures,
            ..Default::default()
        }
    }

    // ── TaskFailureRateCheck ──────────────────────────────────────────────────

    #[test]
    fn task_failure_check_unknown_when_no_tasks() {
        let result = TaskFailureRateCheck.run(&empty_snap());
        assert_eq!(result.status, HealthStatus::Unknown);
    }

    #[test]
    fn task_failure_check_healthy_below_five_pct() {
        let snap = snap_with_tasks(100, 3); // 3%
        let result = TaskFailureRateCheck.run(&snap);
        assert_eq!(result.status, HealthStatus::Healthy);
    }

    #[test]
    fn task_failure_check_degraded_between_five_and_twenty_pct() {
        let snap = snap_with_tasks(100, 10); // 10%
        let result = TaskFailureRateCheck.run(&snap);
        assert_eq!(result.status, HealthStatus::Degraded);
    }

    #[test]
    fn task_failure_check_critical_above_twenty_pct() {
        let snap = snap_with_tasks(100, 25); // 25%
        let result = TaskFailureRateCheck.run(&snap);
        assert_eq!(result.status, HealthStatus::Critical);
    }

    // ── CortexFaultRateCheck ──────────────────────────────────────────────────

    #[test]
    fn cortex_fault_check_unknown_with_no_invocations() {
        let result = CortexFaultRateCheck.run(&empty_snap());
        assert_eq!(result.status, HealthStatus::Unknown);
    }

    #[test]
    fn cortex_fault_check_healthy_under_ten_pct() {
        let snap = AuditSnapshot {
            cortex_invocations: 50,
            cortex_faults: 4, // 8%
            total_audit_entries: 54,
            ..Default::default()
        };
        let result = CortexFaultRateCheck.run(&snap);
        assert_eq!(result.status, HealthStatus::Healthy);
    }

    #[test]
    fn cortex_fault_check_critical_above_thirty_pct() {
        let snap = AuditSnapshot {
            cortex_invocations: 10,
            cortex_faults: 4, // 40%
            total_audit_entries: 14,
            ..Default::default()
        };
        let result = CortexFaultRateCheck.run(&snap);
        assert_eq!(result.status, HealthStatus::Critical);
    }

    // ── MemoryPressureCheck ───────────────────────────────────────────────────

    #[test]
    fn memory_pressure_check_unknown_when_no_max_context() {
        let result = MemoryPressureCheck.run(&empty_snap());
        assert_eq!(result.status, HealthStatus::Unknown);
    }

    #[test]
    fn memory_pressure_check_healthy_at_low_fill() {
        let snap = AuditSnapshot {
            last_l1_tokens: 1000,
            last_l1_max_context: 4096,
            total_audit_entries: 1,
            ..Default::default()
        };
        let result = MemoryPressureCheck.run(&snap);
        assert_eq!(result.status, HealthStatus::Healthy);
    }

    #[test]
    fn memory_pressure_check_critical_at_very_high_fill() {
        let snap = AuditSnapshot {
            last_l1_tokens: 3900,
            last_l1_max_context: 4096,
            memory_pressure_critical_events: 8,
            total_audit_entries: 10,
            ..Default::default()
        };
        let result = MemoryPressureCheck.run(&snap);
        assert_eq!(result.status, HealthStatus::Critical);
    }

    // ── FinancialBudgetCheck ──────────────────────────────────────────────────

    #[test]
    fn financial_budget_check_unknown_with_no_entries() {
        let result = FinancialBudgetCheck.run(&empty_snap());
        assert_eq!(result.status, HealthStatus::Unknown);
    }

    #[test]
    fn financial_budget_check_healthy_above_thirty_pct() {
        let snap = AuditSnapshot {
            last_financial_budget: 0.75,
            total_audit_entries: 1,
            ..Default::default()
        };
        let result = FinancialBudgetCheck.run(&snap);
        assert_eq!(result.status, HealthStatus::Healthy);
    }

    #[test]
    fn financial_budget_check_critical_below_ten_pct() {
        let snap = AuditSnapshot {
            last_financial_budget: 0.05,
            total_audit_entries: 1,
            ..Default::default()
        };
        let result = FinancialBudgetCheck.run(&snap);
        assert_eq!(result.status, HealthStatus::Critical);
    }

    // ── ThermalLoadCheck ──────────────────────────────────────────────────────

    #[test]
    fn thermal_load_check_healthy_under_seventy_pct() {
        let snap = AuditSnapshot {
            last_thermal_load: 0.50,
            total_audit_entries: 1,
            ..Default::default()
        };
        let result = ThermalLoadCheck.run(&snap);
        assert_eq!(result.status, HealthStatus::Healthy);
    }

    #[test]
    fn thermal_load_check_critical_above_eighty_five_pct() {
        let snap = AuditSnapshot {
            last_thermal_load: 0.90,
            total_audit_entries: 1,
            ..Default::default()
        };
        let result = ThermalLoadCheck.run(&snap);
        assert_eq!(result.status, HealthStatus::Critical);
    }

    // ── DefenceVetoCheck ──────────────────────────────────────────────────────

    #[test]
    fn defence_veto_check_healthy_with_zero_vetoes() {
        let result = DefenceVetoCheck.run(&empty_snap());
        assert_eq!(result.status, HealthStatus::Healthy);
    }

    #[test]
    fn defence_veto_check_degraded_with_few_vetoes() {
        let snap = AuditSnapshot {
            defence_vetoes: 5,
            total_audit_entries: 5,
            ..Default::default()
        };
        let result = DefenceVetoCheck.run(&snap);
        assert_eq!(result.status, HealthStatus::Degraded);
    }

    #[test]
    fn defence_veto_check_critical_with_escalation() {
        let snap = AuditSnapshot {
            defence_vetoes: 3,
            attention_escalations: 1,
            total_audit_entries: 4,
            ..Default::default()
        };
        let result = DefenceVetoCheck.run(&snap);
        assert_eq!(result.status, HealthStatus::Critical);
    }

    // ── SleepCycleHealthCheck ─────────────────────────────────────────────────

    #[test]
    fn sleep_cycle_check_unknown_with_no_cycles() {
        let result = SleepCycleHealthCheck.run(&empty_snap());
        assert_eq!(result.status, HealthStatus::Unknown);
    }

    #[test]
    fn sleep_cycle_check_healthy_with_all_successes() {
        let snap = AuditSnapshot {
            sleep_cycles_ok: 100,
            total_audit_entries: 100,
            ..Default::default()
        };
        let result = SleepCycleHealthCheck.run(&snap);
        assert_eq!(result.status, HealthStatus::Healthy);
    }

    #[test]
    fn sleep_cycle_check_critical_with_many_failures() {
        let snap = AuditSnapshot {
            sleep_cycles_ok: 3,
            sleep_cycles_failed: 7,
            total_audit_entries: 10,
            ..Default::default()
        };
        let result = SleepCycleHealthCheck.run(&snap);
        assert_eq!(result.status, HealthStatus::Critical);
    }

    // ── all_checks ────────────────────────────────────────────────────────────

    #[test]
    fn all_checks_returns_eleven_checks() {
        assert_eq!(all_checks().len(), 11);
    }

    #[test]
    fn all_checks_have_distinct_check_ids() {
        let checks = all_checks();
        let ids: std::collections::HashSet<&'static str> =
            checks.iter().map(|c| c.check_id()).collect();
        assert_eq!(
            ids.len(),
            checks.len(),
            "each check must have a unique check_id"
        );
    }
}
