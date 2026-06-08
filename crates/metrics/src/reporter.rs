//! Human-readable text report renderer for [`AgentMetrics`].

use crate::aggregator::AgentMetrics;

/// Render `metrics` as a formatted text report suitable for a terminal or log.
///
/// The report is structured into sections, each covering one aspect of agent
/// health.  All values are right-aligned within their column for readability.
pub fn render_text_report(m: &AgentMetrics) -> String {
    let mut out = String::with_capacity(1024);

    let sep = "─".repeat(52);
    out.push_str(&format!("┌{sep}┐\n"));
    out.push_str(&format!(
        "│  AnimaOS Metrics Report — agent: {:<18}│\n",
        m.agent_id
    ));
    out.push_str(&format!(
        "│  Window: {} audit entries{:<24}│\n",
        m.window_entries, ""
    ));
    out.push_str(&format!("└{sep}┘\n\n"));

    // ── Tasks ─────────────────────────────────────────────────────────────────
    out.push_str("Tasks\n");
    out.push_str(&format!(
        "  started     {:>8}   completed  {:>8}   failed  {:>8}\n",
        m.tasks_started, m.tasks_completed, m.tasks_failed
    ));
    out.push_str(&format!(
        "  success rate {:>7.1}%  tokens emitted  {:>12}\n\n",
        m.task_success_rate * 100.0,
        m.total_tokens_emitted
    ));

    // ── Gate ─────────────────────────────────────────────────────────────────
    out.push_str("Striatal Gate\n");
    out.push_str(&format!(
        "  decisions {:>6}   invoked {:>6}  ({:.1}%)   blocked {:>6}\n",
        m.gate_decisions,
        m.gate_invocations,
        m.gate_invoke_rate * 100.0,
        m.gate_blocks
    ));
    out.push_str(&format!(
        "  cost class: cheap-local {:>4}  mid-tier {:>4}  frontier {:>4}\n",
        m.gate_cheap_local, m.gate_mid_tier, m.gate_frontier
    ));
    out.push_str(&format!(
        "  overrides {:>6}   mean value score {:.3}\n\n",
        m.gate_overrides, m.gate_mean_value_score
    ));

    // ── Router ────────────────────────────────────────────────────────────────
    out.push_str("Router\n");
    out.push_str(&format!(
        "  route modulations (stress downgrades)  {:>6}\n\n",
        m.router_modulations
    ));

    // ── Memory ────────────────────────────────────────────────────────────────
    out.push_str("Memory\n");
    out.push_str(&format!(
        "  pressure: normal {:>4}  high-water {:>4}  critical {:>4}\n",
        m.memory_pressure_normal, m.memory_pressure_high_water, m.memory_pressure_critical
    ));
    out.push_str(&format!(
        "  sleep cycles {:>4}   phases ok {:>4}   phases failed {:>4}\n\n",
        m.sleep_cycles, m.sleep_phases_succeeded, m.sleep_phases_failed
    ));

    // ── Cortex ────────────────────────────────────────────────────────────────
    out.push_str("Cortex\n");
    out.push_str(&format!(
        "  invoked {:>6}  completed {:>6}  faulted {:>6}  (fault rate {:.1}%)\n",
        m.cortex_invocations,
        m.cortex_completions,
        m.cortex_faults,
        m.cortex_fault_rate * 100.0
    ));
    out.push_str(&format!(
        "  tool calls {:>6}   mean latency {:>8.1} ms\n\n",
        m.cortex_total_tool_calls, m.cortex_mean_latency_ms
    ));

    // ── Defence ───────────────────────────────────────────────────────────────
    out.push_str("Defence\n");
    out.push_str(&format!(
        "  vetoes {:>4}  constitution vetoes {:>4}  escalations {:>4}\n\n",
        m.defence_vetoes, m.constitution_vetoes, m.attention_escalations
    ));

    // ── Interoception ─────────────────────────────────────────────────────────
    out.push_str("Interoception (means across snapshots)\n");
    out.push_str(&format!(
        "  snapshots {:>4}  thermal {:.3}  mem-pressure {:.3}  fin-budget {:.3}\n",
        m.interoceptive_snapshots,
        m.mean_thermal_load,
        m.mean_memory_pressure,
        m.mean_financial_budget
    ));

    out
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregator::{aggregate, AgentMetrics};
    use vita::audit::AuditEntry;

    fn basic_metrics() -> AgentMetrics {
        let entries = vec![
            AuditEntry::TaskStarted {
                agent_id: "reporter-agent".to_string(),
                task_id: 1,
                tier: 0,
                prompt: "hello".to_string(),
            },
            AuditEntry::TaskCompleted {
                agent_id: "reporter-agent".to_string(),
                task_id: 1,
                tokens_emitted: 77,
                response: "world".to_string(),
            },
        ];
        aggregate(&entries)
    }

    #[test]
    fn report_contains_agent_id() {
        let m = basic_metrics();
        let r = render_text_report(&m);
        assert!(r.contains("reporter-agent"));
    }

    #[test]
    fn report_contains_task_section() {
        let m = basic_metrics();
        let r = render_text_report(&m);
        assert!(r.contains("Tasks"));
        assert!(r.contains("started"));
        assert!(r.contains("completed"));
    }

    #[test]
    fn report_contains_gate_section() {
        let m = basic_metrics();
        let r = render_text_report(&m);
        assert!(r.contains("Striatal Gate"));
    }

    #[test]
    fn report_contains_cortex_section() {
        let m = basic_metrics();
        let r = render_text_report(&m);
        assert!(r.contains("Cortex"));
        assert!(r.contains("invoked"));
    }

    #[test]
    fn report_contains_defence_section() {
        let m = basic_metrics();
        let r = render_text_report(&m);
        assert!(r.contains("Defence"));
    }

    #[test]
    fn report_contains_interoception_section() {
        let m = basic_metrics();
        let r = render_text_report(&m);
        assert!(r.contains("Interoception"));
    }

    #[test]
    fn report_shows_token_count() {
        let m = basic_metrics();
        let r = render_text_report(&m);
        assert!(r.contains("77"));
    }

    #[test]
    fn empty_metrics_report_does_not_panic() {
        let m = aggregate(&[]);
        let r = render_text_report(&m);
        assert!(r.contains("unknown"));
    }
}
