#![forbid(unsafe_code)]
//! # AnimaOS Metrics — Epic E21
//!
//! Prometheus-compatible metrics export for the AnimaOS runtime.
//!
//! The [`MetricRegistry`] accumulates counters, gauges, and histogram
//! observations derived from [`vita::AuditEntry`] events.  Callers feed it
//! audit entries as they arrive; at any point [`MetricRegistry::render`]
//! produces a Prometheus exposition-format string suitable for a `/metrics`
//! HTTP endpoint or `anima-hosted metrics` CLI output.
//!
//! ## Metric families
//!
//! | Family | Type | Source |
//! |--------|------|--------|
//! | `anima_tasks_total` | Counter | `TaskStarted` (per tier) |
//! | `anima_task_completions_total` | Counter | `TaskCompleted` |
//! | `anima_task_failures_total` | Counter | `TaskFailed` |
//! | `anima_tokens_emitted_total` | Counter | `TaskCompleted` |
//! | `anima_sleep_cycles_total` | Counter | `SleepEntered` |
//! | `anima_gate_decisions_total` | Counter | `GateDecision` (outcome label) |
//! | `anima_gate_invocations_total` | Counter | `GateDecision` (cost_class label) |
//! | `anima_defence_vetoes_total` | Counter | `DefenceVeto` (detector label) |
//! | `anima_constitution_vetoes_total` | Counter | `ConstitutionVeto` (prohibition label) |
//! | `anima_cortex_invocations_total` | Counter | `CortexInvoked` |
//! | `anima_cortex_faults_total` | Counter | `CortexFault` |
//! | `anima_router_modulations_total` | Counter | `RouterModulated` |
//! | `anima_thermal_load` | Gauge | `InteroceptiveSnapshot` |
//! | `anima_compute_pressure` | Gauge | `InteroceptiveSnapshot` |
//! | `anima_memory_pressure_ratio` | Gauge | `InteroceptiveSnapshot` |
//! | `anima_power_budget_ratio` | Gauge | `InteroceptiveSnapshot` |
//! | `anima_financial_budget_ratio` | Gauge | `InteroceptiveSnapshot` |
//! | `anima_attention_demand` | Gauge | `InteroceptiveSnapshot` |
//! | `anima_aggregate_stress` | Gauge | `InteroceptiveSnapshot` |
//! | `anima_gate_value_score` | Gauge | `GateDecision` (last value) |
//! | `anima_gate_latency_ms_bucket` | Histogram | `CortexInvoked` (first-action latency) |

use std::collections::HashMap;
use std::fmt::Write as FmtWrite;

use vita::AuditEntry;

// ── Metric primitives ────────────────────────────────────────────────────────

/// A set of string labels attached to a metric sample.
pub type LabelSet = Vec<(String, String)>;

/// A single numeric observation with an optional label set.
#[derive(Debug, Clone)]
pub struct Sample {
    pub labels: LabelSet,
    pub value: f64,
}

impl Sample {
    #[allow(dead_code)]
    fn new(value: f64) -> Self {
        Self {
            labels: vec![],
            value,
        }
    }

    #[allow(dead_code)]
    fn labelled(labels: impl IntoIterator<Item = (&'static str, String)>, value: f64) -> Self {
        Self {
            labels: labels
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            value,
        }
    }
}

/// One counter value keyed by a label signature.
#[derive(Debug, Default, Clone)]
struct Counter(HashMap<String, f64>);

impl Counter {
    fn inc(&mut self, key: &str) {
        *self.0.entry(key.to_string()).or_insert(0.0) += 1.0;
    }

    fn samples(&self, labels_fn: impl Fn(&str) -> LabelSet) -> Vec<Sample> {
        let mut out: Vec<Sample> = self
            .0
            .iter()
            .map(|(k, &v)| Sample {
                labels: labels_fn(k),
                value: v,
            })
            .collect();
        out.sort_by(|a, b| a.labels.cmp(&b.labels));
        out
    }
}

/// Accumulated histogram observations.
#[derive(Debug, Clone)]
pub struct HistogramData {
    count: u64,
    sum: f64,
    /// Upper-bound boundaries.  We use fixed percentile-friendly buckets.
    buckets: Vec<(f64, u64)>,
}

impl Default for HistogramData {
    fn default() -> Self {
        Self::new()
    }
}

impl HistogramData {
    fn new() -> Self {
        Self {
            count: 0,
            sum: 0.0,
            buckets: LATENCY_BUCKETS.iter().map(|&b| (b, 0)).collect(),
        }
    }

    fn observe(&mut self, v: f64) {
        self.count += 1;
        self.sum += v;
        for (bound, count) in &mut self.buckets {
            if v <= *bound {
                *count += 1;
            }
        }
    }
}

/// Fixed latency histogram bucket upper bounds (milliseconds).
const LATENCY_BUCKETS: &[f64] = &[
    5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0,
];

// ── MetricRegistry ────────────────────────────────────────────────────────────

/// Central store for all AnimaOS agent metrics.
///
/// Feed audit entries with [`update`] and render Prometheus text at any time
/// with [`render`].
///
/// [`update`]: MetricRegistry::update
/// [`render`]: MetricRegistry::render
#[derive(Debug, Default, Clone)]
pub struct MetricRegistry {
    // Counters
    tasks_by_tier: Counter,
    task_completions: f64,
    task_failures: f64,
    tokens_emitted: f64,
    sleep_cycles: f64,
    gate_decisions: Counter,
    gate_invocations: Counter,
    defence_vetoes: Counter,
    constitution_vetoes: Counter,
    cortex_invocations: f64,
    cortex_faults: f64,
    router_modulations: f64,

    // Gauges (last observed value)
    thermal_load: Option<f64>,
    compute_pressure: Option<f64>,
    memory_pressure: Option<f64>,
    power_budget: Option<f64>,
    financial_budget: Option<f64>,
    attention_demand: Option<f64>,
    aggregate_stress: Option<f64>,
    gate_value_score: Option<f64>,

    // Histograms
    cortex_latency_ms: HistogramData,
}

impl MetricRegistry {
    /// Create a fresh registry with all counters at zero.
    pub fn new() -> Self {
        Self {
            cortex_latency_ms: HistogramData::new(),
            ..Default::default()
        }
    }

    /// Update the registry from a single [`AuditEntry`].
    ///
    /// This is the primary ingestion path.  Call it for every entry in order.
    pub fn update(&mut self, entry: &AuditEntry) {
        match entry {
            AuditEntry::TaskStarted { tier, .. } => {
                self.tasks_by_tier.inc(&tier.to_string());
            }
            AuditEntry::TaskCompleted { tokens_emitted, .. } => {
                self.task_completions += 1.0;
                self.tokens_emitted += f64::from(*tokens_emitted);
            }
            AuditEntry::TaskFailed { .. } => {
                self.task_failures += 1.0;
            }
            AuditEntry::SleepEntered { .. } => {
                self.sleep_cycles += 1.0;
            }
            AuditEntry::GateDecision {
                invoke,
                cost_class,
                value_score,
                ..
            } => {
                let outcome = if *invoke { "invoke" } else { "block" };
                self.gate_decisions.inc(outcome);
                if *invoke {
                    let cc = cost_class.as_deref().unwrap_or("unknown");
                    self.gate_invocations.inc(cc);
                }
                self.gate_value_score = Some(f64::from(*value_score));
            }
            AuditEntry::DefenceVeto { detector, .. } => {
                self.defence_vetoes.inc(detector);
            }
            AuditEntry::ConstitutionVeto { prohibition_id, .. } => {
                self.constitution_vetoes.inc(prohibition_id);
            }
            AuditEntry::CortexInvoked {
                latency_to_first_action_ms,
                ..
            } => {
                self.cortex_invocations += 1.0;
                self.cortex_latency_ms
                    .observe(*latency_to_first_action_ms as f64);
            }
            AuditEntry::CortexFault { .. } => {
                self.cortex_faults += 1.0;
            }
            AuditEntry::RouterModulated { .. } => {
                self.router_modulations += 1.0;
            }
            AuditEntry::InteroceptiveSnapshot {
                thermal_load,
                compute_pressure,
                memory_pressure,
                power_budget,
                financial_budget,
                attention_demand,
                aggregate_stress,
                ..
            } => {
                self.thermal_load = Some(f64::from(*thermal_load));
                self.compute_pressure = Some(f64::from(*compute_pressure));
                self.memory_pressure = Some(f64::from(*memory_pressure));
                self.power_budget = Some(f64::from(*power_budget));
                self.financial_budget = Some(f64::from(*financial_budget));
                self.attention_demand = Some(f64::from(*attention_demand));
                self.aggregate_stress = Some(f64::from(*aggregate_stress));
            }
            _ => {}
        }
    }

    /// Ingest a slice of audit entries in order.
    pub fn update_all(&mut self, entries: &[AuditEntry]) {
        for e in entries {
            self.update(e);
        }
    }

    /// Render the registry in Prometheus exposition format (text/plain; version=0.0.4).
    ///
    /// The returned string is ready to be served on a `/metrics` HTTP endpoint.
    pub fn render(&self) -> String {
        let mut buf = String::with_capacity(4096);
        self.write_counters(&mut buf);
        self.write_gauges(&mut buf);
        self.write_histogram(&mut buf);
        buf
    }

    // ── Rendering helpers ─────────────────────────────────────────────────────

    fn write_counters(&self, buf: &mut String) {
        // tasks_total
        write_family_header(
            buf,
            "anima_tasks_total",
            "counter",
            "Tasks dispatched from the MLFQ agenda, by tier.",
        );
        for s in self
            .tasks_by_tier
            .samples(|k| vec![("tier".to_string(), k.to_string())])
        {
            write_sample(buf, "anima_tasks_total", &s.labels, s.value);
        }

        write_counter_scalar(
            buf,
            "anima_task_completions_total",
            "Tasks completed with a response from the backend.",
            self.task_completions,
        );
        write_counter_scalar(
            buf,
            "anima_task_failures_total",
            "Tasks that failed or were cancelled.",
            self.task_failures,
        );
        write_counter_scalar(
            buf,
            "anima_tokens_emitted_total",
            "Total tokens emitted by the LLM backend across all completed tasks.",
            self.tokens_emitted,
        );
        write_counter_scalar(
            buf,
            "anima_sleep_cycles_total",
            "Lifecycle sleep-cycle entries (SleepEntered events).",
            self.sleep_cycles,
        );

        // gate_decisions_total
        write_family_header(
            buf,
            "anima_gate_decisions_total",
            "counter",
            "Striatal Gate decisions, by outcome (invoke|block).",
        );
        for s in self
            .gate_decisions
            .samples(|k| vec![("outcome".to_string(), k.to_string())])
        {
            write_sample(buf, "anima_gate_decisions_total", &s.labels, s.value);
        }
        if self.gate_decisions.0.is_empty() {
            write_sample(
                buf,
                "anima_gate_decisions_total",
                &[("outcome".to_string(), "invoke".to_string())],
                0.0,
            );
            write_sample(
                buf,
                "anima_gate_decisions_total",
                &[("outcome".to_string(), "block".to_string())],
                0.0,
            );
        }

        // gate_invocations_total
        write_family_header(
            buf,
            "anima_gate_invocations_total",
            "counter",
            "Gate invocations that passed, by cost class.",
        );
        for s in self
            .gate_invocations
            .samples(|k| vec![("cost_class".to_string(), k.to_string())])
        {
            write_sample(buf, "anima_gate_invocations_total", &s.labels, s.value);
        }

        // defence_vetoes_total
        write_family_header(
            buf,
            "anima_defence_vetoes_total",
            "counter",
            "Defence layer vetoes, by detector name.",
        );
        for s in self
            .defence_vetoes
            .samples(|k| vec![("detector".to_string(), k.to_string())])
        {
            write_sample(buf, "anima_defence_vetoes_total", &s.labels, s.value);
        }

        // constitution_vetoes_total
        write_family_header(
            buf,
            "anima_constitution_vetoes_total",
            "counter",
            "Constitution charter vetoes, by prohibition id.",
        );
        for s in self
            .constitution_vetoes
            .samples(|k| vec![("prohibition_id".to_string(), k.to_string())])
        {
            write_sample(buf, "anima_constitution_vetoes_total", &s.labels, s.value);
        }

        write_counter_scalar(
            buf,
            "anima_cortex_invocations_total",
            "Successful cortex invocations (CortexInvoked events).",
            self.cortex_invocations,
        );
        write_counter_scalar(
            buf,
            "anima_cortex_faults_total",
            "Cortex process faults or unrecoverable errors.",
            self.cortex_faults,
        );
        write_counter_scalar(
            buf,
            "anima_router_modulations_total",
            "Thalamic Router downgrade events (RouterModulated).",
            self.router_modulations,
        );
    }

    fn write_gauges(&self, buf: &mut String) {
        let gauges: &[(&str, &str, Option<f64>)] = &[
            (
                "anima_thermal_load",
                "CPU/GPU thermal occupancy (0=cool, 1=throttled).",
                self.thermal_load,
            ),
            (
                "anima_compute_pressure",
                "Compute-pipeline saturation (0=idle, 1=saturated).",
                self.compute_pressure,
            ),
            (
                "anima_memory_pressure_ratio",
                "Working-memory fill fraction (0=empty, 1=full).",
                self.memory_pressure,
            ),
            (
                "anima_power_budget_ratio",
                "Available power budget fraction (1=AC/full, 0=flat).",
                self.power_budget,
            ),
            (
                "anima_financial_budget_ratio",
                "Remaining financial API budget fraction.",
                self.financial_budget,
            ),
            (
                "anima_attention_demand",
                "User attention level (1=foreground, 0=absent).",
                self.attention_demand,
            ),
            (
                "anima_aggregate_stress",
                "Weighted aggregate homeostatic stress.",
                self.aggregate_stress,
            ),
            (
                "anima_gate_value_score",
                "Last Striatal Gate value score computed.",
                self.gate_value_score,
            ),
        ];
        for (name, help, val) in gauges {
            if let Some(v) = val {
                write_family_header(buf, name, "gauge", help);
                write_sample(buf, name, &[], *v);
            }
        }
    }

    fn write_histogram(&self, buf: &mut String) {
        let hist = &self.cortex_latency_ms;
        let name = "anima_gate_latency_ms";
        write_family_header(
            buf,
            name,
            "histogram",
            "Cortex first-action latency in milliseconds from sensory packet arrival.",
        );
        for (bound, count) in &hist.buckets {
            let le = if bound.is_infinite() {
                "+Inf".to_string()
            } else {
                format!("{bound}")
            };
            let labels = vec![("le".to_string(), le)];
            let _ = writeln!(buf, "{}_bucket{} {}", name, format_labels(&labels), count);
        }
        let inf_labels = vec![("le".to_string(), "+Inf".to_string())];
        let _ = writeln!(
            buf,
            "{}_bucket{} {}",
            name,
            format_labels(&inf_labels),
            hist.count
        );
        let _ = writeln!(buf, "{}_sum {}", name, hist.sum);
        let _ = writeln!(buf, "{}_count {}", name, hist.count);
    }

    /// Return a concise human-readable summary (for `anima-hosted metrics` CLI).
    pub fn summary(&self) -> String {
        let mut buf = String::new();
        let total_tasks: f64 = self.tasks_by_tier.0.values().sum();
        let _ = writeln!(buf, "Tasks dispatched : {total_tasks:.0}");
        let _ = writeln!(buf, "  Completed      : {:.0}", self.task_completions);
        let _ = writeln!(buf, "  Failed         : {:.0}", self.task_failures);
        let _ = writeln!(buf, "  Tokens emitted : {:.0}", self.tokens_emitted);
        let _ = writeln!(buf, "Sleep cycles     : {:.0}", self.sleep_cycles);
        let invoke = self.gate_decisions.0.get("invoke").copied().unwrap_or(0.0);
        let block = self.gate_decisions.0.get("block").copied().unwrap_or(0.0);
        let _ = writeln!(
            buf,
            "Gate decisions   : {:.0} invoke / {:.0} block",
            invoke, block
        );
        let _ = writeln!(
            buf,
            "Defence vetoes   : {:.0}",
            self.defence_vetoes.0.values().sum::<f64>()
        );
        let _ = writeln!(
            buf,
            "Cortex invocations: {:.0} / faults: {:.0}",
            self.cortex_invocations, self.cortex_faults
        );
        let _ = writeln!(buf, "Router modulations: {:.0}", self.router_modulations);
        if let Some(v) = self.aggregate_stress {
            let _ = writeln!(buf, "Aggregate stress : {:.3}", v);
        }
        if let Some(v) = self.financial_budget {
            let _ = writeln!(buf, "Financial budget : {:.1}%", v * 100.0);
        }
        if let Some(v) = self.thermal_load {
            let _ = writeln!(buf, "Thermal load     : {:.1}%", v * 100.0);
        }
        buf
    }
}

// ── Formatting helpers ────────────────────────────────────────────────────────

fn write_family_header(buf: &mut String, name: &str, kind: &str, help: &str) {
    let _ = writeln!(buf, "# HELP {name} {help}");
    let _ = writeln!(buf, "# TYPE {name} {kind}");
}

fn write_counter_scalar(buf: &mut String, name: &str, help: &str, value: f64) {
    write_family_header(buf, name, "counter", help);
    write_sample(buf, name, &[], value);
}

fn write_sample(buf: &mut String, name: &str, labels: &[(String, String)], value: f64) {
    let _ = writeln!(buf, "{}{} {}", name, format_labels(labels), value);
}

fn format_labels(labels: &[(String, String)]) -> String {
    if labels.is_empty() {
        return String::new();
    }
    let inner: Vec<String> = labels
        .iter()
        .map(|(k, v)| format!("{}=\"{}\"", k, escape_label_value(v)))
        .collect();
    format!("{{{}}}", inner.join(","))
}

fn escape_label_value(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

// ── Public convenience ────────────────────────────────────────────────────────

/// Build a [`MetricRegistry`] from a complete slice of audit entries.
///
/// Convenience wrapper around `MetricRegistry::new()` + `update_all`.
pub fn registry_from_audit(entries: &[AuditEntry]) -> MetricRegistry {
    let mut r = MetricRegistry::new();
    r.update_all(entries);
    r
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn task_started(tier: u8) -> AuditEntry {
        AuditEntry::TaskStarted {
            agent_id: "a".into(),
            task_id: 1,
            tier,
            prompt: "hello".into(),
        }
    }

    fn task_completed(tokens: u32) -> AuditEntry {
        AuditEntry::TaskCompleted {
            agent_id: "a".into(),
            task_id: 1,
            tokens_emitted: tokens,
            response: "ok".into(),
        }
    }

    fn task_failed() -> AuditEntry {
        AuditEntry::TaskFailed {
            agent_id: "a".into(),
            task_id: 1,
            error: "timeout".into(),
        }
    }

    fn gate_invoke(cost_class: &str, value: f32) -> AuditEntry {
        AuditEntry::GateDecision {
            agent_id: "a".into(),
            event_id: "e1".into(),
            invoke: true,
            cost_class: Some(cost_class.into()),
            urgency: 0.8,
            novelty: 0.5,
            user_facing: true,
            semantic_class: "query".into(),
            value_score: value,
            threshold_applied: 0.4,
            thermal_load: 0.0,
            compute_pressure: 0.0,
            memory_pressure: 0.0,
            power_budget: 1.0,
            financial_budget: 1.0,
            attention_demand: 0.5,
            reasoning: "test".into(),
            override_active: false,
        }
    }

    fn gate_block() -> AuditEntry {
        AuditEntry::GateDecision {
            agent_id: "a".into(),
            event_id: "e2".into(),
            invoke: false,
            cost_class: None,
            urgency: 0.1,
            novelty: 0.1,
            user_facing: false,
            semantic_class: "background".into(),
            value_score: 0.2,
            threshold_applied: 0.4,
            thermal_load: 0.0,
            compute_pressure: 0.0,
            memory_pressure: 0.0,
            power_budget: 1.0,
            financial_budget: 1.0,
            attention_demand: 0.0,
            reasoning: "blocked".into(),
            override_active: false,
        }
    }

    fn defence_veto(detector: &str) -> AuditEntry {
        AuditEntry::DefenceVeto {
            agent_id: "a".into(),
            invocation_id: "inv1".into(),
            detector: detector.into(),
            action_blocked: "some action".into(),
            reason: "injection detected".into(),
        }
    }

    fn interoceptive_snapshot(stress: f32) -> AuditEntry {
        AuditEntry::InteroceptiveSnapshot {
            agent_id: "a".into(),
            tick_ns: 1_000_000,
            thermal_load: 0.3,
            compute_pressure: 0.2,
            memory_pressure: 0.4,
            power_budget: 0.9,
            financial_budget: 0.8,
            attention_demand: 0.5,
            aggregate_stress: stress,
        }
    }

    fn cortex_invoked(latency_ms: u64) -> AuditEntry {
        AuditEntry::CortexInvoked {
            task_id: "t1".into(),
            latency_to_first_action_ms: latency_ms,
        }
    }

    fn cortex_fault() -> AuditEntry {
        AuditEntry::CortexFault {
            task_id: "t1".into(),
            error: "crash".into(),
        }
    }

    fn router_modulated() -> AuditEntry {
        AuditEntry::RouterModulated {
            agent_id: "a".into(),
            event_id: "e1".into(),
            requested_route_id: "frontier".into(),
            effective_route_id: "mid-tier".into(),
            reason: "financial pressure".into(),
        }
    }

    #[test]
    fn task_counters_accumulate_correctly() {
        let mut r = MetricRegistry::new();
        r.update(&task_started(0));
        r.update(&task_started(0));
        r.update(&task_started(2));
        r.update(&task_completed(100));
        r.update(&task_failed());

        let s = r.summary();
        assert!(s.contains("Tasks dispatched : 3"));
        assert!(s.contains("Completed      : 1"));
        assert!(s.contains("Failed         : 1"));
        assert!(s.contains("Tokens emitted : 100"));
    }

    #[test]
    fn tasks_by_tier_labels_in_prometheus_output() {
        let mut r = MetricRegistry::new();
        r.update(&task_started(0));
        r.update(&task_started(1));
        r.update(&task_started(0));

        let out = r.render();
        assert!(out.contains("anima_tasks_total{tier=\"0\"} 2"));
        assert!(out.contains("anima_tasks_total{tier=\"1\"} 1"));
    }

    #[test]
    fn gate_decisions_split_by_outcome() {
        let mut r = MetricRegistry::new();
        r.update(&gate_invoke("MidTier", 0.7));
        r.update(&gate_invoke("Frontier", 0.9));
        r.update(&gate_block());

        let out = r.render();
        assert!(out.contains("anima_gate_decisions_total{outcome=\"invoke\"} 2"));
        assert!(out.contains("anima_gate_decisions_total{outcome=\"block\"} 1"));
    }

    #[test]
    fn gate_invocations_keyed_by_cost_class() {
        let mut r = MetricRegistry::new();
        r.update(&gate_invoke("MidTier", 0.7));
        r.update(&gate_invoke("MidTier", 0.8));
        r.update(&gate_invoke("Frontier", 0.95));

        let out = r.render();
        assert!(out.contains("anima_gate_invocations_total{cost_class=\"MidTier\"} 2"));
        assert!(out.contains("anima_gate_invocations_total{cost_class=\"Frontier\"} 1"));
    }

    #[test]
    fn defence_vetoes_labelled_by_detector() {
        let mut r = MetricRegistry::new();
        r.update(&defence_veto("PromptInjectionDetector"));
        r.update(&defence_veto("PromptInjectionDetector"));
        r.update(&defence_veto("RewardHackingDetector"));

        let out = r.render();
        assert!(out.contains("anima_defence_vetoes_total{detector=\"PromptInjectionDetector\"} 2"));
        assert!(out.contains("anima_defence_vetoes_total{detector=\"RewardHackingDetector\"} 1"));
    }

    #[test]
    fn interoceptive_snapshot_updates_gauges() {
        let mut r = MetricRegistry::new();
        r.update(&interoceptive_snapshot(0.6));

        let out = r.render();
        assert!(out.contains("anima_aggregate_stress 0.6"));
        assert!(out.contains("anima_thermal_load 0.3"));
        assert!(out.contains("anima_financial_budget_ratio 0.8"));
    }

    #[test]
    fn cortex_latency_histogram_buckets_are_cumulative() {
        let mut r = MetricRegistry::new();
        r.update(&cortex_invoked(20));
        r.update(&cortex_invoked(80));
        r.update(&cortex_invoked(300));

        let out = r.render();
        // 20ms ≤ 25 bucket → count=1
        assert!(out.contains("anima_gate_latency_ms_bucket{le=\"25\"} 1"));
        // 20 and 80 ≤ 100 bucket → count=2
        assert!(out.contains("anima_gate_latency_ms_bucket{le=\"100\"} 2"));
        // all three ≤ +Inf → count=3
        assert!(out.contains("anima_gate_latency_ms_count 3"));
        assert!(out.contains("anima_gate_latency_ms_sum 400"));
    }

    #[test]
    fn cortex_faults_counter_increments() {
        let mut r = MetricRegistry::new();
        r.update(&cortex_fault());
        r.update(&cortex_fault());

        let out = r.render();
        assert!(out.contains("anima_cortex_faults_total 2"));
    }

    #[test]
    fn router_modulations_counter_increments() {
        let mut r = MetricRegistry::new();
        r.update(&router_modulated());
        r.update(&router_modulated());

        let out = r.render();
        assert!(out.contains("anima_router_modulations_total 2"));
    }

    #[test]
    fn sleep_cycles_counter_increments() {
        let mut r = MetricRegistry::new();
        r.update(&AuditEntry::SleepEntered {
            agent_id: "a".into(),
        });
        r.update(&AuditEntry::SleepEntered {
            agent_id: "a".into(),
        });

        let out = r.render();
        assert!(out.contains("anima_sleep_cycles_total 2"));
    }

    #[test]
    fn gate_value_score_gauge_reflects_last_observation() {
        let mut r = MetricRegistry::new();
        r.update(&gate_invoke("CheapLocal", 0.55));
        r.update(&gate_invoke("Frontier", 0.91));

        let out = r.render();
        assert!(out.contains("anima_gate_value_score 0.91"));
    }

    #[test]
    fn render_contains_type_and_help_lines() {
        let r = MetricRegistry::new();
        let out = r.render();
        assert!(out.contains("# HELP anima_tasks_total"));
        assert!(out.contains("# TYPE anima_tasks_total counter"));
        assert!(out.contains("# HELP anima_gate_latency_ms"));
        assert!(out.contains("# TYPE anima_gate_latency_ms histogram"));
    }

    #[test]
    fn registry_from_audit_convenience_constructor() {
        let entries = vec![
            task_started(0),
            task_completed(50),
            interoceptive_snapshot(0.3),
        ];
        let r = registry_from_audit(&entries);
        assert_eq!(r.task_completions, 1.0);
        assert_eq!(r.tokens_emitted, 50.0);
        assert!(r.aggregate_stress.is_some());
    }

    #[test]
    fn summary_reports_all_key_fields() {
        let mut r = MetricRegistry::new();
        r.update(&task_started(0));
        r.update(&task_completed(200));
        r.update(&gate_invoke("MidTier", 0.7));
        r.update(&defence_veto("GoalDriftMonitor"));
        r.update(&cortex_invoked(45));
        r.update(&interoceptive_snapshot(0.5));

        let s = r.summary();
        assert!(s.contains("Tasks dispatched"));
        assert!(s.contains("Gate decisions"));
        assert!(s.contains("Defence vetoes"));
        assert!(s.contains("Cortex invocations"));
        assert!(s.contains("Aggregate stress"));
        assert!(s.contains("Financial budget"));
    }

    #[test]
    fn label_escaping_handles_special_characters() {
        let labels = vec![("name".to_string(), r#"foo\"bar"#.to_string())];
        let formatted = format_labels(&labels);
        assert!(formatted.contains(r#"foo\\\"bar"#));
    }

    #[test]
    fn constitution_vetoes_labelled_by_prohibition_id() {
        let mut r = MetricRegistry::new();
        r.update(&AuditEntry::ConstitutionVeto {
            agent_id: "a".into(),
            invocation_id: "i1".into(),
            prohibition_id: "P3".into(),
            clause_text: "no deception".into(),
            action_blocked: "lie".into(),
            proposal_type: "CortexAction".into(),
        });

        let out = r.render();
        assert!(out.contains("anima_constitution_vetoes_total{prohibition_id=\"P3\"} 1"));
    }
}
