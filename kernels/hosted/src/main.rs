//! Linux process emulation entry point — boots the somatic stack in-process
//! for local rapid CI and developer experimentation.
//!
//! # Backend selection (E1.3)
//!
//! The hosted kernel selects an LLM backend at startup via the
//! `ANIMA_BACKEND` environment variable.  Recognised values:
//!
//! | Value         | Backend                              |
//! |---------------|--------------------------------------|
//! | `anthropic`   | Anthropic Claude (fixture mode)      |
//! | `openai`      | OpenAI GPT (fixture mode)            |
//! | `mock`        | Built-in deterministic mock          |
//! | _(any other)_ | Falls back to `mock`                 |
//!
//! Example: `ANIMA_BACKEND=anthropic cargo run --bin anima-hosted`
//!
//! # Phase 1 M1.6 demo
//!
//! Two concurrent agents execute through a shared backend; their audit logs
//! are printed to stdout on completion.
//!
//! # `anima why` subcommand (E5.2)
//!
//! Running `cargo run --bin anima-hosted -- why` exercises the Striatal Gate on
//! a sample of representative events and prints the most recent `GateDecision`
//! audit entry in human-readable form, satisfying E5.2 exit criterion 3.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use interoception::HomeostaticMonitor;
use llm_backends::factory::BackendFactory;
use memory::VirtualContextManager;
use scheduler::Task;
use senses::{HumanGuidance, SensoryBridge};
use vita::gate::Gate;
use vita::{
    record_gate_decision, somatic_execution_loop, AuditEntry, EventFeatures, GateOverride,
    HomeostaticSignals, LifecycleConfig, LifecycleManager, SemanticClass, ThresholdGate,
};

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut future = Box::pin(future);
    loop {
        match Pin::as_mut(&mut future).poll(&mut cx) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

fn build_agent(
    agent_id: &str,
    policy: &str,
    backend: Arc<dyn scheduler::LlmBackend>,
    tasks: Vec<Task>,
    max_iterations: u32,
) -> LifecycleManager {
    let mut manager = LifecycleManager::new(
        agent_id,
        SensoryBridge::new(HumanGuidance::new(policy)),
        VirtualContextManager::with_capacity(0, 4096),
        LifecycleConfig { max_context: 4096 },
        HumanGuidance::new("boot"),
        backend,
        Some(max_iterations),
    );
    for task in tasks {
        manager.agenda.push(task);
    }
    manager
}

fn run_agent(mut manager: LifecycleManager) -> LifecycleManager {
    let mut monitor = HomeostaticMonitor::new(1.0, 0.5, 8);
    monitor.record_ttft(1.0);
    block_on(somatic_execution_loop(&mut manager, &monitor)).expect("lifecycle loop failed");
    manager
}

fn print_audit(manager: &LifecycleManager) {
    println!(
        "[{}] backend={} state={:?} dispatched={} audit={}",
        manager.agent_id,
        manager.backend.id(),
        manager.state,
        manager.scheduler.dispatched_tasks.len(),
        manager.audit.len()
    );
    for entry in manager.audit.entries() {
        match entry {
            AuditEntry::TaskStarted {
                task_id,
                tier,
                prompt,
                ..
            } => println!("  → started   task={task_id} tier={tier} prompt={prompt:?}"),
            AuditEntry::TaskCompleted {
                task_id,
                tokens_emitted,
                response,
                ..
            } => println!(
                "  ✓ completed task={task_id} tokens={tokens_emitted} response={response:?}"
            ),
            AuditEntry::TaskFailed { task_id, error, .. } => {
                println!("  ✗ failed    task={task_id} error={error}")
            }
            AuditEntry::SleepEntered { .. } => println!("  zzz sleep_entered"),
            AuditEntry::WakeEntered { .. } => println!("  ☀  wake_entered"),
            AuditEntry::SleepPhaseStarted { phase, .. } => {
                println!("  →   sleep_phase_started phase={phase}")
            }
            AuditEntry::SleepPhaseCompleted { phase, success, .. } => {
                let mark = if *success { "✓" } else { "✗" };
                println!("  {mark}   sleep_phase_completed phase={phase} success={success}");
            }
            // E5.1 cortex entries
            AuditEntry::CortexInvoked {
                task_id,
                latency_to_first_action_ms,
                ..
            } => println!(
                "  ⚙  cortex_invoked task={task_id} latency_ms={latency_to_first_action_ms}"
            ),
            AuditEntry::CortexCompleted {
                task_id,
                tool_calls,
                summary_len,
                ..
            } => println!(
                "  ✓  cortex_completed task={task_id} tool_calls={tool_calls} summary_len={summary_len}"
            ),
            AuditEntry::CortexFault { task_id, error, .. } => {
                println!("  ✗  cortex_fault task={task_id} error={error}")
            }
            // E5.2 gate decision entries
            AuditEntry::GateDecision {
                event_id,
                invoke,
                cost_class,
                urgency,
                novelty,
                value_score,
                threshold_applied,
                thermal_load,
                financial_budget,
                attention_demand,
                reasoning,
                override_active,
                ..
            } => {
                let verdict = if *invoke {
                    format!("INVOKE [{}]", cost_class.as_deref().unwrap_or("?"))
                } else {
                    "BLOCK".to_string()
                };
                let override_tag = if *override_active { " [OVERRIDE]" } else { "" };
                println!(
                    "  🔀 gate_decision event={event_id} verdict={verdict}{override_tag}"
                );
                println!(
                    "       urgency={urgency:.3} novelty={novelty:.3} \
                     value={value_score:.3} threshold={threshold_applied:.3}"
                );
                println!(
                    "       thermal={thermal_load:.3} financial_budget={financial_budget:.3} \
                     attention={attention_demand:.3}"
                );
                println!("       reasoning: {reasoning}");
            }
        }
    }
}

// ── `anima why` subcommand (E5.2 exit criterion 3) ───────────────────────────

/// Exercises the Striatal Gate on representative events, records the decisions
/// to an in-process audit log, and prints the most recent `GateDecision` entry.
///
/// Output format mirrors what a persistent audit-log reader would display.
fn cmd_why() {
    use vita::AuditLog;

    println!("anima why — Striatal Gate decision explainer (E5.2)\n");

    let gate = ThresholdGate::with_defaults();
    let mut log = AuditLog::new();
    let agent_id = "anima";

    // Sample scenarios covering the full range of gate outcomes.
    let scenarios: &[(&str, EventFeatures, HomeostaticSignals, GateOverride)] = &[
        (
            "background-cleanup",
            EventFeatures {
                urgency: 0.1,
                novelty: 0.05,
                semantic_class: SemanticClass::BackgroundTask,
                user_facing: false,
            },
            HomeostaticSignals::neutral(),
            GateOverride::None,
        ),
        (
            "user-question",
            EventFeatures {
                urgency: 0.7,
                novelty: 0.4,
                semantic_class: SemanticClass::UserQuery,
                user_facing: true,
            },
            HomeostaticSignals::neutral(),
            GateOverride::None,
        ),
        (
            "high-priority-under-thermal",
            EventFeatures {
                urgency: 0.8,
                novelty: 0.3,
                semantic_class: SemanticClass::UserQuery,
                user_facing: true,
            },
            HomeostaticSignals {
                thermal_load: 0.9,
                ..HomeostaticSignals::neutral()
            },
            GateOverride::None,
        ),
        (
            "operator-emergency",
            EventFeatures {
                urgency: 0.4,
                novelty: 0.2,
                semantic_class: SemanticClass::OperatorCommand,
                user_facing: false,
            },
            HomeostaticSignals::neutral(),
            GateOverride::OperatorForced {
                reason: "emergency shutdown initiated".to_string(),
            },
        ),
    ];

    for (event_id, event, signals, override_hint) in scenarios {
        let decision = gate.decide(event_id, event, signals, override_hint);
        record_gate_decision(&mut log, agent_id, &decision, event, signals);
    }

    println!("Evaluated {} scenarios. Full audit trail:\n", log.len());
    for entry in log.entries() {
        if let AuditEntry::GateDecision {
            event_id,
            invoke,
            cost_class,
            urgency,
            novelty,
            user_facing,
            semantic_class,
            value_score,
            threshold_applied,
            thermal_load,
            compute_pressure,
            memory_pressure,
            power_budget,
            financial_budget,
            attention_demand,
            reasoning,
            override_active,
            ..
        } = entry
        {
            let verdict = if *invoke {
                format!("✅ INVOKE [{}]", cost_class.as_deref().unwrap_or("?"))
            } else {
                "🚫 BLOCK".to_string()
            };
            let override_tag = if *override_active {
                "  ⚠ OVERRIDE ACTIVE"
            } else {
                ""
            };
            println!("━━━ event: {event_id}{override_tag}");
            println!("  verdict         : {verdict}");
            println!("  reasoning       : {reasoning}");
            println!("  ─ event features ─────────────────────────────");
            println!("  urgency         : {urgency:.3}");
            println!("  novelty         : {novelty:.3}");
            println!("  user_facing     : {user_facing}");
            println!("  semantic_class  : {semantic_class}");
            println!("  value_score     : {value_score:.4}");
            println!("  threshold       : {threshold_applied:.4}");
            println!("  ─ homeostatic signals ────────────────────────");
            println!("  thermal_load    : {thermal_load:.3}");
            println!("  compute_pressure: {compute_pressure:.3}");
            println!("  memory_pressure : {memory_pressure:.3}");
            println!("  power_budget    : {power_budget:.3}");
            println!("  financial_budget: {financial_budget:.3}");
            println!("  attention_demand: {attention_demand:.3}");
            println!();
        }
    }

    // Print the most recent GateDecision (satisfies E5.2 exit criterion 3).
    if let Some(AuditEntry::GateDecision {
        event_id,
        reasoning,
        invoke,
        cost_class,
        ..
    }) = log
        .entries()
        .iter()
        .rev()
        .find(|e| matches!(e, AuditEntry::GateDecision { .. }))
    {
        let verdict = if *invoke {
            format!("INVOKE [{}]", cost_class.as_deref().unwrap_or("?"))
        } else {
            "BLOCK".to_string()
        };
        println!("Most recent gate decision: event={event_id} verdict={verdict}");
        println!("Reasoning: {reasoning}");
    }
}

fn main() {
    // ── Subcommand dispatch ───────────────────────────────────────────────────
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("why") {
        cmd_why();
        return;
    }

    // ── Backend selection (E1.3) ─────────────────────────────────────────────
    let provider = std::env::var("ANIMA_BACKEND").unwrap_or_else(|_| "mock".to_string());
    let backend = BackendFactory::from_env_or_mock(&provider);
    println!(
        "anima-hosted: selected backend={} model={} max_ctx={}",
        backend.id(),
        backend.model_id(),
        backend.max_context_tokens()
    );

    // ── Two-agent demo (E1.6) ────────────────────────────────────────────────
    let agent_a = build_agent(
        "agent-a",
        "optimize-for-low-token-usage",
        Arc::clone(&backend),
        vec![
            Task::new(1, 0, "draft the morning status report"),
            Task::new(2, 1, "summarize overnight telemetry"),
        ],
        6,
    );

    let agent_b = build_agent(
        "agent-b",
        "prioritize-tooling-throughput",
        Arc::clone(&backend),
        vec![
            Task::new(101, 0, "answer the operator question"),
            Task::new(102, 2, "compact yesterday memory archive"),
        ],
        6,
    );

    println!("booting two somatic loops over a shared backend...\n");

    let handle_a = std::thread::spawn(move || run_agent(agent_a));
    let handle_b = std::thread::spawn(move || run_agent(agent_b));

    let agent_a = handle_a.join().expect("agent-a thread panicked");
    let agent_b = handle_b.join().expect("agent-b thread panicked");

    print_audit(&agent_a);
    println!();
    print_audit(&agent_b);
}
