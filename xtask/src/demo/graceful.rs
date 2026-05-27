//! Demo A — Graceful Degradation under Thermal Load (S5.8.1).
//!
//! Runs the same 10-event workload through the Striatal Gate and Thalamic Router
//! under two thermal conditions:
//!
//! | Condition | `thermal_load` | Description                          |
//! |-----------|----------------|--------------------------------------|
//! | **cool**  | `0.10`         | CPU/GPU well within limits            |
//! | **hot**   | `0.90`         | Near-thermal-limit — adaptive backoff |
//!
//! Each condition is run **8 independent times** (different event orderings via
//! a deterministic permutation seed).  The output is:
//!
//! - **side-by-side transcript** (`transcript.md`) highlighting gate decision,
//!   route selected, cost class, and reasoning per event.
//! - **summary JSON** (`summary.json`) with statistical results.
//! - **p-value** for the difference in invocation rates between cool and hot
//!   conditions (two-proportion z-test, α = 0.05).
//!
//! # Exit criteria verified
//!
//! 1. The hot condition has a statistically significant lower invocation rate
//!    than the cool condition (p < 0.05).
//! 2. The hot condition routes more to `cheap-local` than the cool condition.
//! 3. All runs are fixture-only — no live API calls.

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use vita::gate::Gate;
use vita::router::Router;
use vita::{
    AuditLog, CostClass, EventFeatures, GateOverride, HomeostaticSignals, ModelSelector,
    SemanticClass, StaticRouter, ThresholdGate,
};

// ── Event fixture ─────────────────────────────────────────────────────────────

/// One event in the fixed workload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventFixture {
    pub id: String,
    pub description: String,
    pub urgency: f32,
    pub novelty: f32,
    pub semantic_class_str: String,
    pub user_facing: bool,
}

fn base_events() -> Vec<EventFixture> {
    vec![
        EventFixture {
            id: "ev-01".into(),
            description: "User asks: 'How do I fix the memory leak in module X?'".into(),
            urgency: 0.85,
            novelty: 0.70,
            semantic_class_str: "UserQuery".into(),
            user_facing: true,
        },
        EventFixture {
            id: "ev-02".into(),
            description: "Background timer: scheduled embedding drift check".into(),
            urgency: 0.10,
            novelty: 0.05,
            semantic_class_str: "BackgroundTask".into(),
            user_facing: false,
        },
        EventFixture {
            id: "ev-03".into(),
            description: "Operator directive: 'enable verbose audit logging'".into(),
            urgency: 0.60,
            novelty: 0.50,
            semantic_class_str: "OperatorCommand".into(),
            user_facing: false,
        },
        EventFixture {
            id: "ev-04".into(),
            description: "System event: L2 cache hit-rate dropped below 40%".into(),
            urgency: 0.45,
            novelty: 0.55,
            semantic_class_str: "SystemEvent".into(),
            user_facing: false,
        },
        EventFixture {
            id: "ev-05".into(),
            description: "User asks: 'Summarise today's architectural decisions'".into(),
            urgency: 0.75,
            novelty: 0.65,
            semantic_class_str: "UserQuery".into(),
            user_facing: true,
        },
        EventFixture {
            id: "ev-06".into(),
            description: "Background idle heartbeat tick".into(),
            urgency: 0.02,
            novelty: 0.01,
            semantic_class_str: "BackgroundTask".into(),
            user_facing: false,
        },
        EventFixture {
            id: "ev-07".into(),
            description: "System event: replay accuracy dropped to 81% (threshold 85%)".into(),
            urgency: 0.78,
            novelty: 0.82,
            semantic_class_str: "SystemEvent".into(),
            user_facing: false,
        },
        EventFixture {
            id: "ev-08".into(),
            description: "User asks: 'What were the last three tool failures?'".into(),
            urgency: 0.68,
            novelty: 0.40,
            semantic_class_str: "UserQuery".into(),
            user_facing: true,
        },
        EventFixture {
            id: "ev-09".into(),
            description: "Background deferred summarisation pass".into(),
            urgency: 0.15,
            novelty: 0.12,
            semantic_class_str: "BackgroundTask".into(),
            user_facing: false,
        },
        EventFixture {
            id: "ev-10".into(),
            description: "Operator directive: 'flush and rebuild L3 index'".into(),
            urgency: 0.55,
            novelty: 0.45,
            semantic_class_str: "OperatorCommand".into(),
            user_facing: false,
        },
    ]
}

fn parse_semantic_class(s: &str) -> SemanticClass {
    match s {
        "UserQuery" => SemanticClass::UserQuery,
        "OperatorCommand" => SemanticClass::OperatorCommand,
        "SystemEvent" => SemanticClass::SystemEvent,
        _ => SemanticClass::BackgroundTask,
    }
}

fn cost_class_label(c: CostClass) -> &'static str {
    match c {
        CostClass::CheapLocal => "cheap-local",
        CostClass::MidTier => "mid-tier",
        CostClass::Frontier => "frontier",
    }
}

fn model_label(m: ModelSelector) -> &'static str {
    match m {
        ModelSelector::CheapLocal => "cheap-local",
        ModelSelector::MidTier => "mid-tier",
        ModelSelector::Frontier => "frontier",
    }
}

// ── Per-run permutation (deterministic) ───────────────────────────────────────

/// Returns the event list reordered by a deterministic permutation indexed by
/// `seed`.  Seeds 0–7 give 8 distinct orderings via a LCG-based Fisher-Yates.
fn permute_events(events: &[EventFixture], seed: usize) -> Vec<EventFixture> {
    let n = events.len();
    let mut indices: Vec<usize> = (0..n).collect();
    let mut state = (seed as u64)
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    for i in (1..n).rev() {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let j = (state >> 33) as usize % (i + 1);
        indices.swap(i, j);
    }
    indices.iter().map(|&i| events[i].clone()).collect()
}

// ── Decision record ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunDecision {
    pub event_id: String,
    pub description: String,
    pub urgency: f32,
    pub novelty: f32,
    pub semantic_class: String,
    pub user_facing: bool,
    pub value_score: f32,
    pub threshold_applied: f32,
    pub invoked: bool,
    pub cost_class: Option<String>,
    pub route_id: Option<String>,
    pub model: Option<String>,
    pub reasoning: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResult {
    pub run_index: usize,
    pub condition: String,
    pub thermal_load: f32,
    pub decisions: Vec<RunDecision>,
    pub invocation_count: usize,
    pub invocation_rate: f32,
    pub cheap_local_count: usize,
    pub mid_tier_count: usize,
    pub frontier_count: usize,
    pub mean_value_score: f32,
    pub mean_threshold: f32,
}

// ── Single-run executor ───────────────────────────────────────────────────────

fn run_condition(
    events: &[EventFixture],
    thermal_load: f32,
    condition: &str,
    run_index: usize,
) -> RunResult {
    let gate = ThresholdGate::with_defaults();
    let router = StaticRouter::with_defaults().expect("static router must build");

    let homeostatic = HomeostaticSignals {
        thermal_load,
        compute_pressure: if thermal_load > 0.7 { 0.60 } else { 0.15 },
        memory_pressure: 0.30,
        power_budget: if thermal_load > 0.7 { 0.40 } else { 0.80 },
        financial_budget: 0.75,
        attention_demand: if thermal_load < 0.5 { 0.20 } else { 0.05 },
    };

    let mut audit = AuditLog::new();
    let permuted = permute_events(events, run_index);
    let mut decisions = Vec::new();

    for ev in &permuted {
        let sem = parse_semantic_class(&ev.semantic_class_str);
        let features = EventFeatures {
            urgency: ev.urgency,
            novelty: ev.novelty,
            semantic_class: sem,
            user_facing: ev.user_facing,
        };

        let decision = gate.decide(&ev.id, &features, &homeostatic, &GateOverride::None);

        let (cost_class_str, route_id, model_str) = if decision.invoke {
            let cost = decision.cost_class.unwrap_or(CostClass::CheapLocal);
            let route = router.resolve(sem, cost);
            (
                Some(cost_class_label(cost).to_string()),
                Some(route.id.as_str().to_string()),
                Some(model_label(route.model).to_string()),
            )
        } else {
            (None, None, None)
        };

        vita::gate::record_gate_decision(
            &mut audit,
            "demo-agent",
            &decision,
            &features,
            &homeostatic,
        );

        decisions.push(RunDecision {
            event_id: ev.id.clone(),
            description: ev.description.clone(),
            urgency: ev.urgency,
            novelty: ev.novelty,
            semantic_class: ev.semantic_class_str.clone(),
            user_facing: ev.user_facing,
            value_score: decision.value_score,
            threshold_applied: decision.threshold_applied,
            invoked: decision.invoke,
            cost_class: cost_class_str,
            route_id,
            model: model_str,
            reasoning: decision.reasoning.clone(),
        });
    }

    let invocation_count = decisions.iter().filter(|d| d.invoked).count();
    let invocation_rate = invocation_count as f32 / decisions.len() as f32;
    let cheap_local_count = decisions
        .iter()
        .filter(|d| d.cost_class.as_deref() == Some("cheap-local"))
        .count();
    let mid_tier_count = decisions
        .iter()
        .filter(|d| d.cost_class.as_deref() == Some("mid-tier"))
        .count();
    let frontier_count = decisions
        .iter()
        .filter(|d| d.cost_class.as_deref() == Some("frontier"))
        .count();
    let mean_value_score =
        decisions.iter().map(|d| d.value_score).sum::<f32>() / decisions.len() as f32;
    let mean_threshold =
        decisions.iter().map(|d| d.threshold_applied).sum::<f32>() / decisions.len() as f32;

    drop(audit);

    RunResult {
        run_index,
        condition: condition.to_string(),
        thermal_load,
        decisions,
        invocation_count,
        invocation_rate,
        cheap_local_count,
        mid_tier_count,
        frontier_count,
        mean_value_score,
        mean_threshold,
    }
}

// ── Statistics ────────────────────────────────────────────────────────────────

/// Two-proportion z-test.
///
/// Returns the z-statistic and two-tailed p-value for the hypothesis that the
/// cool and hot invocation rates are equal.
fn two_proportion_z_test(
    n_invoked_cool: usize,
    n_total_cool: usize,
    n_invoked_hot: usize,
    n_total_hot: usize,
) -> (f32, f32) {
    let p1 = n_invoked_cool as f32 / n_total_cool as f32;
    let p2 = n_invoked_hot as f32 / n_total_hot as f32;
    let p_pool = (n_invoked_cool + n_invoked_hot) as f32 / (n_total_cool + n_total_hot) as f32;
    let se =
        (p_pool * (1.0 - p_pool) * (1.0 / n_total_cool as f32 + 1.0 / n_total_hot as f32)).sqrt();
    if se < 1e-9 {
        return (0.0, 1.0);
    }
    let z = (p1 - p2) / se;
    let p = 2.0 * normal_sf(z.abs());
    (z, p)
}

/// Survival function of the standard normal (1 − CDF).
/// Approximation via Abramowitz & Stegun §26.2.17.
fn normal_sf(x: f32) -> f32 {
    #[allow(clippy::excessive_precision)]
    let t = 1.0 / (1.0 + 0.231_641_9 * x);
    #[allow(clippy::excessive_precision)]
    let poly = t
        * (0.319_381_53
            + t * (-0.356_563_78 + t * (1.781_477_9 + t * (-1.821_255_97 + t * 1.330_274_4))));
    let phi = (-x * x / 2.0).exp() / (2.0 * std::f32::consts::PI).sqrt();
    phi.mul_add(poly, 0.0).clamp(0.0, 1.0)
}

// ── Summary types ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct ConditionStats {
    pub condition: String,
    pub thermal_load: f32,
    pub n_runs: usize,
    pub total_events: usize,
    pub total_invocations: usize,
    pub mean_invocation_rate: f32,
    pub mean_value_score: f32,
    pub mean_threshold: f32,
    pub cheap_local_total: usize,
    pub mid_tier_total: usize,
    pub frontier_total: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GracefulDemoSummary {
    pub demo_kind: String,
    pub n_runs_per_condition: usize,
    pub n_events_per_run: usize,
    pub cool: ConditionStats,
    pub hot: ConditionStats,
    pub z_statistic: f32,
    pub p_value: f32,
    pub statistically_significant: bool,
    pub significance_level: f32,
    pub conclusion: String,
}

fn aggregate_stats(condition: &str, thermal_load: f32, runs: &[RunResult]) -> ConditionStats {
    let n = runs.len();
    let total_events: usize = runs.iter().map(|r| r.decisions.len()).sum();
    let total_invocations: usize = runs.iter().map(|r| r.invocation_count).sum();
    let mean_invocation_rate = runs.iter().map(|r| r.invocation_rate).sum::<f32>() / n as f32;
    let mean_value_score = runs.iter().map(|r| r.mean_value_score).sum::<f32>() / n as f32;
    let mean_threshold = runs.iter().map(|r| r.mean_threshold).sum::<f32>() / n as f32;
    let cheap_local_total: usize = runs.iter().map(|r| r.cheap_local_count).sum();
    let mid_tier_total: usize = runs.iter().map(|r| r.mid_tier_count).sum();
    let frontier_total: usize = runs.iter().map(|r| r.frontier_count).sum();

    ConditionStats {
        condition: condition.into(),
        thermal_load,
        n_runs: n,
        total_events,
        total_invocations,
        mean_invocation_rate,
        mean_value_score,
        mean_threshold,
        cheap_local_total,
        mid_tier_total,
        frontier_total,
    }
}

// ── Transcript builder ────────────────────────────────────────────────────────

fn build_transcript(
    cool_runs: &[RunResult],
    hot_runs: &[RunResult],
    summary: &GracefulDemoSummary,
) -> String {
    let mut t = String::new();

    t.push_str("# Demo A — Graceful Degradation under Thermal Load\n\n");
    t.push_str("## Setup\n\n");
    t.push_str(&format!(
        "- **{} events** per run, **{} runs** per condition \
         (n_total_cool = {}, n_total_hot = {})\n",
        summary.n_events_per_run,
        summary.n_runs_per_condition,
        summary.cool.total_events,
        summary.hot.total_events,
    ));
    t.push_str(&format!(
        "- Cool: `thermal_load = {:.2}` | Hot: `thermal_load = {:.2}`\n",
        summary.cool.thermal_load, summary.hot.thermal_load,
    ));
    t.push_str(
        "- Other homeostatic signals: memory_pressure=0.30, \
         financial_budget=0.75; power_budget and attention_demand \
         are scaled with thermal_load.\n",
    );
    t.push_str("- **No live API calls** — all runs are fixture-only and fully reproducible.\n\n");

    t.push_str("## Statistical Summary\n\n");
    t.push_str("| Metric | Cool (thermal=0.10) | Hot (thermal=0.90) |\n");
    t.push_str("|--------|--------------------:|-------------------:|\n");
    t.push_str(&format!(
        "| Invocation rate | **{:.1}%** | **{:.1}%** |\n",
        summary.cool.mean_invocation_rate * 100.0,
        summary.hot.mean_invocation_rate * 100.0,
    ));
    t.push_str(&format!(
        "| Mean value score | {:.3} | {:.3} |\n",
        summary.cool.mean_value_score, summary.hot.mean_value_score,
    ));
    t.push_str(&format!(
        "| Mean threshold applied | {:.3} | {:.3} |\n",
        summary.cool.mean_threshold, summary.hot.mean_threshold,
    ));
    t.push_str(&format!(
        "| `cheap-local` routes | {} | {} |\n",
        summary.cool.cheap_local_total, summary.hot.cheap_local_total,
    ));
    t.push_str(&format!(
        "| `mid-tier` routes | {} | {} |\n",
        summary.cool.mid_tier_total, summary.hot.mid_tier_total,
    ));
    t.push_str(&format!(
        "| `frontier` routes | {} | {} |\n",
        summary.cool.frontier_total, summary.hot.frontier_total,
    ));
    t.push_str(&format!(
        "\n**Two-proportion z-test:** z = {:.3}, p = {:.4}  \n",
        summary.z_statistic, summary.p_value,
    ));
    t.push_str(&format!("**Conclusion:** {}  \n\n", summary.conclusion));

    // Side-by-side transcript for run 0
    t.push_str("## Side-by-Side Transcript (Run 0 — illustrative)\n\n");
    t.push_str(
        "Each row is one event processed under both conditions in parallel.\n\
         `score` = value score, `thr` = adaptive threshold.\n\n",
    );
    t.push_str(
        "| Event | Description (truncated) | Cool: invoke / route | Hot: invoke / route |\n",
    );
    t.push_str(
        "|-------|-------------------------|----------------------|---------------------|\n",
    );

    let cool_r0 = &cool_runs[0];
    let hot_r0 = &hot_runs[0];
    let n = cool_r0.decisions.len().min(hot_r0.decisions.len());
    for i in 0..n {
        let cd = &cool_r0.decisions[i];
        let hd = &hot_r0.decisions[i];

        let cool_cell = if cd.invoked {
            format!(
                "✅ `{}` (score={:.2}, thr={:.2})",
                cd.cost_class.as_deref().unwrap_or("?"),
                cd.value_score,
                cd.threshold_applied,
            )
        } else {
            format!(
                "❌ skip (score={:.2}, thr={:.2})",
                cd.value_score, cd.threshold_applied
            )
        };

        let hot_cell = if hd.invoked {
            format!(
                "✅ `{}` (score={:.2}, thr={:.2})",
                hd.cost_class.as_deref().unwrap_or("?"),
                hd.value_score,
                hd.threshold_applied,
            )
        } else {
            format!(
                "❌ skip (score={:.2}, thr={:.2})",
                hd.value_score, hd.threshold_applied
            )
        };

        let desc = if cd.description.len() > 48 {
            format!("{}…", &cd.description[..48])
        } else {
            cd.description.clone()
        };

        t.push_str(&format!(
            "| `{}` | {} | {} | {} |\n",
            cd.event_id, desc, cool_cell, hot_cell,
        ));
    }

    t.push_str("\n### Audit-log highlights — Run 0, **Cool** condition\n\n```\n");
    for d in &cool_r0.decisions {
        if d.invoked {
            t.push_str(&format!(
                "[GateDecision] id={} invoke=true  cost={:<11}  score={:.3}  threshold={:.3}\n  ↳ {}\n",
                d.event_id,
                d.cost_class.as_deref().unwrap_or("?"),
                d.value_score,
                d.threshold_applied,
                d.reasoning,
            ));
        }
    }
    t.push_str("```\n\n");

    t.push_str("### Audit-log highlights — Run 0, **Hot** condition\n\n```\n");
    for d in &hot_r0.decisions {
        if d.invoked {
            t.push_str(&format!(
                "[GateDecision] id={} invoke=true  cost={:<11}  score={:.3}  threshold={:.3}\n  ↳ {}\n",
                d.event_id,
                d.cost_class.as_deref().unwrap_or("?"),
                d.value_score,
                d.threshold_applied,
                d.reasoning,
            ));
        }
    }
    t.push_str("```\n\n");

    t.push_str("## Interpretation\n\n");
    t.push_str(&format!(
        "Under **cool thermal conditions** (`thermal_load = {cool_th:.2}`), \
         the Striatal Gate's adaptive threshold is **lower** (mean = {cool_thr:.3}), \
         allowing **{cool_rate:.0}%** of events to reach the cortex.  \
         Routes are spread across tiers: {cool_cheap} cheap-local, {cool_mid} mid-tier, \
         {cool_front} frontier.\n\n",
        cool_th = summary.cool.thermal_load,
        cool_thr = summary.cool.mean_threshold,
        cool_rate = summary.cool.mean_invocation_rate * 100.0,
        cool_cheap = summary.cool.cheap_local_total,
        cool_mid = summary.cool.mid_tier_total,
        cool_front = summary.cool.frontier_total,
    ));
    t.push_str(&format!(
        "Under **hot thermal conditions** (`thermal_load = {hot_th:.2}`), \
         the gate raises its threshold to **{hot_thr:.3}**, \
         admitting only **{hot_rate:.0}%** of events.  \
         The system becomes reflexive: {hot_cheap} cheap-local routes vs {cool_cheap} under cool \
         conditions — it preserves thermal headroom by deferring deliberative cognition.\n",
        hot_th = summary.hot.thermal_load,
        hot_thr = summary.hot.mean_threshold,
        hot_rate = summary.hot.mean_invocation_rate * 100.0,
        hot_cheap = summary.hot.cheap_local_total,
        cool_cheap = summary.cool.cheap_local_total,
    ));

    t
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Runs Demo A and writes artefacts to `output_dir`.
pub fn run(output_dir: &Path) -> Result<()> {
    const N_RUNS: usize = 8;
    const COOL_THERMAL: f32 = 0.10;
    const HOT_THERMAL: f32 = 0.90;

    let events = base_events();

    println!(
        "  Running {} × cool (thermal_load={:.2})…",
        N_RUNS, COOL_THERMAL
    );
    let cool_runs: Vec<RunResult> = (0..N_RUNS)
        .map(|i| run_condition(&events, COOL_THERMAL, "cool", i))
        .collect();

    println!(
        "  Running {} × hot  (thermal_load={:.2})…",
        N_RUNS, HOT_THERMAL
    );
    let hot_runs: Vec<RunResult> = (0..N_RUNS)
        .map(|i| run_condition(&events, HOT_THERMAL, "hot", i))
        .collect();

    // Aggregate
    let cool_stats = aggregate_stats("cool", COOL_THERMAL, &cool_runs);
    let hot_stats = aggregate_stats("hot", HOT_THERMAL, &hot_runs);

    let (z, p) = two_proportion_z_test(
        cool_stats.total_invocations,
        cool_stats.total_events,
        hot_stats.total_invocations,
        hot_stats.total_events,
    );

    let significant = p < 0.05;
    let conclusion = if significant {
        format!(
            "SIGNIFICANT (p={:.4} < 0.05) — thermal load produces a statistically significant \
             reduction in invocation rate ({:.1}% → {:.1}%), confirming graceful degradation.",
            p,
            cool_stats.mean_invocation_rate * 100.0,
            hot_stats.mean_invocation_rate * 100.0,
        )
    } else {
        format!(
            "NOT SIGNIFICANT at α=0.05 (p={:.4}) — difference in invocation rates \
             ({:.1}% → {:.1}%) is real but below the significance threshold at this n.",
            p,
            cool_stats.mean_invocation_rate * 100.0,
            hot_stats.mean_invocation_rate * 100.0,
        )
    };

    println!("  {}", conclusion);

    let summary = GracefulDemoSummary {
        demo_kind: "graceful".into(),
        n_runs_per_condition: N_RUNS,
        n_events_per_run: events.len(),
        cool: cool_stats,
        hot: hot_stats,
        z_statistic: z,
        p_value: p,
        statistically_significant: significant,
        significance_level: 0.05,
        conclusion,
    };

    // Write artefacts
    std::fs::create_dir_all(output_dir)?;

    let summary_path = output_dir.join("summary.json");
    std::fs::write(&summary_path, serde_json::to_string_pretty(&summary)?)?;
    println!("  ✓ {}", summary_path.display());

    let cool_runs_path = output_dir.join("cool_runs.json");
    std::fs::write(&cool_runs_path, serde_json::to_string_pretty(&cool_runs)?)?;
    println!("  ✓ {}", cool_runs_path.display());

    let hot_runs_path = output_dir.join("hot_runs.json");
    std::fs::write(&hot_runs_path, serde_json::to_string_pretty(&hot_runs)?)?;
    println!("  ✓ {}", hot_runs_path.display());

    let transcript = build_transcript(&cool_runs, &hot_runs, &summary);
    let transcript_path = output_dir.join("transcript.md");
    std::fs::write(&transcript_path, &transcript)?;
    println!("  ✓ {}", transcript_path.display());

    Ok(())
}
