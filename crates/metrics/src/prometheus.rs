//! Prometheus text-format renderer for [`AgentMetrics`].
//!
//! Produces an [OpenMetrics](https://openmetrics.io/)-compatible exposition
//! string that any Prometheus-compatible scraper (Prometheus, VictoriaMetrics,
//! Grafana Cloud) can ingest directly.
//!
//! ## Format notes
//!
//! - Every metric is prefixed with `anima_` to prevent collisions.
//! - The `agent` label is derived from [`AgentMetrics::agent_id`].
//! - Counter families (e.g. tasks) use a `status` label to distinguish
//!   outcomes rather than separate metric names.
//! - All rates and means are exposed as gauges.

use crate::aggregator::AgentMetrics;

/// Render `metrics` as a Prometheus text-format exposition string.
///
/// The returned string ends with a trailing newline; each metric family is
/// separated by a blank line.  The format is compatible with the
/// `text/plain; version=0.0.4` content type used by Prometheus scrapers.
pub fn render_prometheus(m: &AgentMetrics) -> String {
    let agent = &m.agent_id;
    let mut out = String::with_capacity(2048);

    // ── Tasks ─────────────────────────────────────────────────────────────────
    push_help(
        &mut out,
        "anima_tasks_total",
        "counter",
        "Tasks processed by outcome",
    );
    push_counter(
        &mut out,
        "anima_tasks_total",
        agent,
        &[("status", "started")],
        m.tasks_started,
    );
    push_counter(
        &mut out,
        "anima_tasks_total",
        agent,
        &[("status", "completed")],
        m.tasks_completed,
    );
    push_counter(
        &mut out,
        "anima_tasks_total",
        agent,
        &[("status", "failed")],
        m.tasks_failed,
    );

    push_help(
        &mut out,
        "anima_task_success_rate",
        "gauge",
        "Fraction of started tasks that completed successfully [0,1]",
    );
    push_gauge_f64(
        &mut out,
        "anima_task_success_rate",
        agent,
        &[],
        m.task_success_rate,
    );

    push_help(
        &mut out,
        "anima_tokens_emitted_total",
        "counter",
        "Total LLM tokens emitted by completed tasks",
    );
    push_counter(
        &mut out,
        "anima_tokens_emitted_total",
        agent,
        &[],
        m.total_tokens_emitted,
    );

    // ── Gate ─────────────────────────────────────────────────────────────────
    push_help(
        &mut out,
        "anima_gate_decisions_total",
        "counter",
        "Striatal Gate decisions by outcome",
    );
    push_counter(
        &mut out,
        "anima_gate_decisions_total",
        agent,
        &[("outcome", "invoked")],
        m.gate_invocations,
    );
    push_counter(
        &mut out,
        "anima_gate_decisions_total",
        agent,
        &[("outcome", "blocked")],
        m.gate_blocks,
    );

    push_help(
        &mut out,
        "anima_gate_invoke_rate",
        "gauge",
        "Fraction of gate decisions that resulted in invocation [0,1]",
    );
    push_gauge_f64(
        &mut out,
        "anima_gate_invoke_rate",
        agent,
        &[],
        m.gate_invoke_rate,
    );

    push_help(
        &mut out,
        "anima_gate_route_decisions_total",
        "counter",
        "Gate decisions by cost class",
    );
    push_counter(
        &mut out,
        "anima_gate_route_decisions_total",
        agent,
        &[("cost_class", "cheap_local")],
        m.gate_cheap_local,
    );
    push_counter(
        &mut out,
        "anima_gate_route_decisions_total",
        agent,
        &[("cost_class", "mid_tier")],
        m.gate_mid_tier,
    );
    push_counter(
        &mut out,
        "anima_gate_route_decisions_total",
        agent,
        &[("cost_class", "frontier")],
        m.gate_frontier,
    );

    push_help(
        &mut out,
        "anima_gate_overrides_total",
        "counter",
        "Gate decisions that bypassed the threshold via an override",
    );
    push_counter(
        &mut out,
        "anima_gate_overrides_total",
        agent,
        &[],
        m.gate_overrides,
    );

    push_help(
        &mut out,
        "anima_gate_mean_value_score",
        "gauge",
        "Mean gate value_score across all decisions",
    );
    push_gauge_f64(
        &mut out,
        "anima_gate_mean_value_score",
        agent,
        &[],
        m.gate_mean_value_score,
    );

    // ── Router ────────────────────────────────────────────────────────────────
    push_help(
        &mut out,
        "anima_router_modulations_total",
        "counter",
        "Route downgrades triggered by homeostatic pressure",
    );
    push_counter(
        &mut out,
        "anima_router_modulations_total",
        agent,
        &[],
        m.router_modulations,
    );

    // ── Memory ────────────────────────────────────────────────────────────────
    push_help(
        &mut out,
        "anima_memory_pressure_events_total",
        "counter",
        "Memory-pressure events by level",
    );
    push_counter(
        &mut out,
        "anima_memory_pressure_events_total",
        agent,
        &[("level", "normal")],
        m.memory_pressure_normal,
    );
    push_counter(
        &mut out,
        "anima_memory_pressure_events_total",
        agent,
        &[("level", "high_water")],
        m.memory_pressure_high_water,
    );
    push_counter(
        &mut out,
        "anima_memory_pressure_events_total",
        agent,
        &[("level", "critical")],
        m.memory_pressure_critical,
    );

    push_help(
        &mut out,
        "anima_sleep_cycles_total",
        "counter",
        "Sleep cycles entered",
    );
    push_counter(
        &mut out,
        "anima_sleep_cycles_total",
        agent,
        &[],
        m.sleep_cycles,
    );

    push_help(
        &mut out,
        "anima_sleep_phases_total",
        "counter",
        "Sleep-maintenance phase completions by outcome",
    );
    push_counter(
        &mut out,
        "anima_sleep_phases_total",
        agent,
        &[("outcome", "succeeded")],
        m.sleep_phases_succeeded,
    );
    push_counter(
        &mut out,
        "anima_sleep_phases_total",
        agent,
        &[("outcome", "failed")],
        m.sleep_phases_failed,
    );

    // ── Cortex ────────────────────────────────────────────────────────────────
    push_help(
        &mut out,
        "anima_cortex_invocations_total",
        "counter",
        "Cortex invocations by outcome",
    );
    push_counter(
        &mut out,
        "anima_cortex_invocations_total",
        agent,
        &[("outcome", "invoked")],
        m.cortex_invocations,
    );
    push_counter(
        &mut out,
        "anima_cortex_invocations_total",
        agent,
        &[("outcome", "completed")],
        m.cortex_completions,
    );
    push_counter(
        &mut out,
        "anima_cortex_invocations_total",
        agent,
        &[("outcome", "faulted")],
        m.cortex_faults,
    );

    push_help(
        &mut out,
        "anima_cortex_fault_rate",
        "gauge",
        "Fraction of cortex invocations that faulted [0,1]",
    );
    push_gauge_f64(
        &mut out,
        "anima_cortex_fault_rate",
        agent,
        &[],
        m.cortex_fault_rate,
    );

    push_help(
        &mut out,
        "anima_cortex_tool_calls_total",
        "counter",
        "Total tool calls made by completed cortex invocations",
    );
    push_counter(
        &mut out,
        "anima_cortex_tool_calls_total",
        agent,
        &[],
        m.cortex_total_tool_calls,
    );

    push_help(
        &mut out,
        "anima_cortex_mean_latency_ms",
        "gauge",
        "Mean latency from invocation to first tool action (ms)",
    );
    push_gauge_f64(
        &mut out,
        "anima_cortex_mean_latency_ms",
        agent,
        &[],
        m.cortex_mean_latency_ms,
    );

    // ── Defence ───────────────────────────────────────────────────────────────
    push_help(
        &mut out,
        "anima_defence_vetoes_total",
        "counter",
        "Defence-layer vetoes by type",
    );
    push_counter(
        &mut out,
        "anima_defence_vetoes_total",
        agent,
        &[("type", "defence")],
        m.defence_vetoes,
    );
    push_counter(
        &mut out,
        "anima_defence_vetoes_total",
        agent,
        &[("type", "constitution")],
        m.constitution_vetoes,
    );

    push_help(
        &mut out,
        "anima_attention_escalations_total",
        "counter",
        "Attention-demand escalations raised to the operator",
    );
    push_counter(
        &mut out,
        "anima_attention_escalations_total",
        agent,
        &[],
        m.attention_escalations,
    );

    // ── Interoception ─────────────────────────────────────────────────────────
    push_help(
        &mut out,
        "anima_interoceptive_snapshots_total",
        "counter",
        "Interoceptive signal snapshots published",
    );
    push_counter(
        &mut out,
        "anima_interoceptive_snapshots_total",
        agent,
        &[],
        m.interoceptive_snapshots,
    );

    push_help(
        &mut out,
        "anima_mean_thermal_load",
        "gauge",
        "Mean thermal_load across interoceptive snapshots [0,1]",
    );
    push_gauge_f64(
        &mut out,
        "anima_mean_thermal_load",
        agent,
        &[],
        m.mean_thermal_load,
    );

    push_help(
        &mut out,
        "anima_mean_memory_pressure",
        "gauge",
        "Mean memory_pressure across interoceptive snapshots [0,1]",
    );
    push_gauge_f64(
        &mut out,
        "anima_mean_memory_pressure",
        agent,
        &[],
        m.mean_memory_pressure,
    );

    push_help(
        &mut out,
        "anima_mean_financial_budget",
        "gauge",
        "Mean financial_budget remaining across interoceptive snapshots [0,1]",
    );
    push_gauge_f64(
        &mut out,
        "anima_mean_financial_budget",
        agent,
        &[],
        m.mean_financial_budget,
    );

    out
}

// ── private helpers ───────────────────────────────────────────────────────────

fn push_help(out: &mut String, name: &str, kind: &str, help: &str) {
    out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} {kind}\n"));
}

fn push_counter(out: &mut String, name: &str, agent: &str, extra: &[(&str, &str)], value: u64) {
    let labels = build_labels(agent, extra);
    out.push_str(&format!("{name}{{{labels}}} {value}\n"));
}

fn push_gauge_f64(out: &mut String, name: &str, agent: &str, extra: &[(&str, &str)], value: f64) {
    let labels = build_labels(agent, extra);
    out.push_str(&format!("{name}{{{labels}}} {value:.6}\n"));
}

fn build_labels(agent: &str, extra: &[(&str, &str)]) -> String {
    let mut parts = vec![format!("agent=\"{agent}\"")];
    for (k, v) in extra {
        parts.push(format!("{k}=\"{v}\""));
    }
    parts.join(",")
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregator::AgentMetrics;

    fn sample_metrics() -> AgentMetrics {
        AgentMetrics {
            agent_id: "test-agent".to_string(),
            window_entries: 10,
            tasks_started: 5,
            tasks_completed: 4,
            tasks_failed: 1,
            task_success_rate: 0.8,
            total_tokens_emitted: 1000,
            gate_decisions: 3,
            gate_invocations: 2,
            gate_blocks: 1,
            gate_invoke_rate: 0.667,
            gate_cheap_local: 1,
            gate_mid_tier: 1,
            gate_frontier: 0,
            gate_overrides: 0,
            gate_value_score_sum: 1.5,
            gate_mean_value_score: 0.5,
            router_modulations: 1,
            memory_pressure_normal: 2,
            memory_pressure_high_water: 1,
            memory_pressure_critical: 0,
            sleep_cycles: 1,
            sleep_phases_succeeded: 4,
            sleep_phases_failed: 0,
            cortex_invocations: 2,
            cortex_completions: 1,
            cortex_faults: 1,
            cortex_fault_rate: 0.5,
            cortex_total_tool_calls: 3,
            cortex_latency_sum_ms: 600,
            cortex_mean_latency_ms: 300.0,
            defence_vetoes: 0,
            constitution_vetoes: 0,
            attention_escalations: 0,
            interoceptive_snapshots: 2,
            thermal_load_sum: 0.3,
            mean_thermal_load: 0.15,
            memory_pressure_sum: 0.4,
            mean_memory_pressure: 0.2,
            financial_budget_sum: 1.8,
            mean_financial_budget: 0.9,
        }
    }

    #[test]
    fn prometheus_output_contains_metric_names() {
        let m = sample_metrics();
        let out = render_prometheus(&m);
        assert!(out.contains("anima_tasks_total"));
        assert!(out.contains("anima_gate_decisions_total"));
        assert!(out.contains("anima_cortex_invocations_total"));
        assert!(out.contains("anima_defence_vetoes_total"));
        assert!(out.contains("anima_sleep_cycles_total"));
    }

    #[test]
    fn prometheus_output_contains_agent_label() {
        let m = sample_metrics();
        let out = render_prometheus(&m);
        assert!(out.contains("agent=\"test-agent\""));
    }

    #[test]
    fn prometheus_output_contains_help_and_type_lines() {
        let m = sample_metrics();
        let out = render_prometheus(&m);
        assert!(out.contains("# HELP anima_tasks_total"));
        assert!(out.contains("# TYPE anima_tasks_total counter"));
        assert!(out.contains("# HELP anima_task_success_rate"));
        assert!(out.contains("# TYPE anima_task_success_rate gauge"));
    }

    #[test]
    fn prometheus_output_contains_task_values() {
        let m = sample_metrics();
        let out = render_prometheus(&m);
        assert!(out.contains("status=\"started\"} 5"));
        assert!(out.contains("status=\"completed\"} 4"));
        assert!(out.contains("status=\"failed\"} 1"));
    }

    #[test]
    fn prometheus_output_contains_cost_class_labels() {
        let m = sample_metrics();
        let out = render_prometheus(&m);
        assert!(out.contains("cost_class=\"cheap_local\""));
        assert!(out.contains("cost_class=\"mid_tier\""));
        assert!(out.contains("cost_class=\"frontier\""));
    }

    #[test]
    fn prometheus_output_contains_memory_pressure_levels() {
        let m = sample_metrics();
        let out = render_prometheus(&m);
        assert!(out.contains("level=\"normal\""));
        assert!(out.contains("level=\"high_water\""));
        assert!(out.contains("level=\"critical\""));
    }

    #[test]
    fn prometheus_output_contains_sleep_outcomes() {
        let m = sample_metrics();
        let out = render_prometheus(&m);
        assert!(out.contains("outcome=\"succeeded\"} 4"));
        assert!(out.contains("outcome=\"failed\"} 0"));
    }

    #[test]
    fn prometheus_output_for_empty_metrics_is_valid() {
        let m = AgentMetrics {
            agent_id: "idle".to_string(),
            window_entries: 0,
            tasks_started: 0,
            tasks_completed: 0,
            tasks_failed: 0,
            task_success_rate: 0.0,
            total_tokens_emitted: 0,
            gate_decisions: 0,
            gate_invocations: 0,
            gate_blocks: 0,
            gate_invoke_rate: 0.0,
            gate_cheap_local: 0,
            gate_mid_tier: 0,
            gate_frontier: 0,
            gate_overrides: 0,
            gate_value_score_sum: 0.0,
            gate_mean_value_score: 0.0,
            router_modulations: 0,
            memory_pressure_normal: 0,
            memory_pressure_high_water: 0,
            memory_pressure_critical: 0,
            sleep_cycles: 0,
            sleep_phases_succeeded: 0,
            sleep_phases_failed: 0,
            cortex_invocations: 0,
            cortex_completions: 0,
            cortex_faults: 0,
            cortex_fault_rate: 0.0,
            cortex_total_tool_calls: 0,
            cortex_latency_sum_ms: 0,
            cortex_mean_latency_ms: 0.0,
            defence_vetoes: 0,
            constitution_vetoes: 0,
            attention_escalations: 0,
            interoceptive_snapshots: 0,
            thermal_load_sum: 0.0,
            mean_thermal_load: 0.0,
            memory_pressure_sum: 0.0,
            mean_memory_pressure: 0.0,
            financial_budget_sum: 0.0,
            mean_financial_budget: 0.0,
        };
        let out = render_prometheus(&m);
        // All lines should start with '#' (help/type) or 'anima_'
        for line in out.lines() {
            assert!(
                line.starts_with('#') || line.starts_with("anima_"),
                "unexpected line: {line}"
            );
        }
    }
}
