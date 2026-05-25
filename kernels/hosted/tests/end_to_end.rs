//! End-to-end integration test for Phase 1 M1.6: two concurrent agents executing
//! tasks through the full senses → vita → scheduler → LlmBackend → audit pipeline.

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
    id: &str,
    backend: Arc<MockLlmBackend>,
    tasks: Vec<Task>,
    max_iterations: u32,
) -> LifecycleManager {
    let mut manager = LifecycleManager::new(
        id,
        SensoryBridge::new(HumanGuidance {
            policy_hint: format!("policy-for-{id}"),
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
    block_on(somatic_execution_loop(&mut manager, &monitor)).unwrap();
    manager
}

fn assert_task_completed(manager: &LifecycleManager, task_id: u64, expected_response: &str) {
    let found = manager.audit.entries().iter().any(|entry| {
        matches!(
            entry,
            AuditEntry::TaskCompleted { task_id: id, response, .. }
                if *id == task_id && response == expected_response
        )
    });
    assert!(
        found,
        "agent {} missing TaskCompleted for task {task_id}",
        manager.agent_id
    );
}

#[test]
fn two_concurrent_agents_complete_tasks_through_shared_backend() {
    let backend = Arc::new(MockLlmBackend::new());

    let agent_a = build_agent(
        "agent-a",
        Arc::clone(&backend),
        vec![
            Task::new(1, 0, "alpha beta"),
            Task::new(2, 1, "gamma delta epsilon"),
        ],
        6,
    );

    let agent_b = build_agent(
        "agent-b",
        Arc::clone(&backend),
        vec![Task::new(101, 0, "one two three four")],
        6,
    );

    let handle_a = std::thread::spawn(move || run_agent(agent_a));
    let handle_b = std::thread::spawn(move || run_agent(agent_b));

    let agent_a = handle_a.join().unwrap();
    let agent_b = handle_b.join().unwrap();

    assert_eq!(agent_a.scheduler.dispatched_tasks.len(), 2);
    assert_eq!(agent_b.scheduler.dispatched_tasks.len(), 1);

    assert_task_completed(&agent_a, 1, "alpha beta ");
    assert_task_completed(&agent_a, 2, "gamma delta epsilon ");
    assert_task_completed(&agent_b, 101, "one two three four ");

    // Each TaskStarted has a matching TaskCompleted (no orphaned starts).
    for manager in [&agent_a, &agent_b] {
        let starts = manager
            .audit
            .entries()
            .iter()
            .filter(|e| matches!(e, AuditEntry::TaskStarted { .. }))
            .count();
        let completions = manager
            .audit
            .entries()
            .iter()
            .filter(|e| matches!(e, AuditEntry::TaskCompleted { .. }))
            .count();
        assert_eq!(
            starts, completions,
            "agent {} has mismatched start/complete pairs",
            manager.agent_id
        );
    }
}
