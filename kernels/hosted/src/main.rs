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

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use interoception::HomeostaticMonitor;
use llm_backends::factory::BackendFactory;
use memory::VirtualContextManager;
use scheduler::Task;
use senses::{HumanGuidance, SensoryBridge};
use vita::{somatic_execution_loop, AuditEntry, LifecycleConfig, LifecycleManager};

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
            // ── E5.6 — Defence Layer ──────────────────────────────────────────
            AuditEntry::DefenceVeto {
                invocation_id,
                detector,
                action_blocked,
                reason,
                ..
            } => {
                println!(
                    "  🛡  DEFENCE VETO inv={invocation_id} detector={detector} \
                     action={action_blocked:?} reason={reason:?}"
                );
            }
            AuditEntry::AttentionDemandEscalated {
                invocation_id,
                veto_count,
                window_secs,
                ..
            } => {
                println!(
                    "  ⚠  ATTENTION ESCALATED inv={invocation_id} \
                     vetoes={veto_count} window={window_secs}s"
                );
            }
        }
    }
}

fn main() {
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
