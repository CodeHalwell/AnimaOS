//! Linux process emulation entry point - boots the somatic stack in-process
//! for local rapid CI and developer experimentation.
//!
//! Phase 1 M1.6 demo: senses → vita → scheduler → LlmBackend → audit log,
//! with two concurrent agents executing through a shared mock backend.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use interoception::HomeostaticMonitor;
use memory::VirtualContextManager;
use scheduler::{MockLlmBackend, Task};
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
    backend: Arc<MockLlmBackend>,
    tasks: Vec<Task>,
    max_iterations: u32,
) -> LifecycleManager {
    let mut manager = LifecycleManager::new(
        agent_id,
        SensoryBridge::new(HumanGuidance {
            policy_hint: policy.to_string(),
        }),
        VirtualContextManager::with_capacity(0, 4096),
        LifecycleConfig { max_context: 4096 },
        HumanGuidance {
            policy_hint: "boot".to_string(),
        },
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
        "[{}] final state = {:?}, dispatched = {} tasks, audit entries = {}",
        manager.agent_id,
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
            } => println!("  - started   task={task_id} tier={tier} prompt={prompt:?}"),
            AuditEntry::TaskCompleted {
                task_id,
                tokens_emitted,
                response,
                ..
            } => println!(
                "  - completed task={task_id} tokens={tokens_emitted} response={response:?}"
            ),
            AuditEntry::TaskFailed { task_id, error, .. } => {
                println!("  - failed    task={task_id} error={error}")
            }
            AuditEntry::SleepEntered { .. } => println!("  - sleep_entered"),
            AuditEntry::WakeEntered { .. } => println!("  - wake_entered"),
        }
    }
}

fn main() {
    let backend = Arc::new(MockLlmBackend::new());

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

    println!("anima-hosted: booting two somatic loops over a shared mock backend...");

    let handle_a = std::thread::spawn(move || run_agent(agent_a));
    let handle_b = std::thread::spawn(move || run_agent(agent_b));

    let agent_a = handle_a.join().expect("agent-a thread panicked");
    let agent_b = handle_b.join().expect("agent-b thread panicked");

    print_audit(&agent_a);
    print_audit(&agent_b);
}
