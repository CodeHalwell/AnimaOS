//! Linux process emulation entry point - boots the somatic stack in-process
//! for local rapid CI and developer experimentation.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use interoception::HomeostaticMonitor;
use memory::VirtualContextManager;
use scheduler::{Task, TaskAgenda};
use senses::{HumanGuidance, SensoryBridge};
use vita::{somatic_execution_loop, LifecycleConfig, LifecycleManager};

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

fn main() {
    let mut agenda = TaskAgenda::new();
    agenda.push(Task {
        id: 1,
        mlfq_level: 0,
    });
    agenda.push(Task {
        id: 2,
        mlfq_level: 1,
    });

    let mut manager = LifecycleManager::new(
        SensoryBridge::new(HumanGuidance {
            policy_hint: "optimize-for-low-token-usage".to_string(),
        }),
        VirtualContextManager::with_capacity(0, 4096),
        LifecycleConfig { max_context: 4096 },
        HumanGuidance {
            policy_hint: "boot".to_string(),
        },
        Some(5),
    );
    manager.agenda = agenda;

    let mut monitor = HomeostaticMonitor::new(1.0, 0.5, 8);
    monitor.record_ttft(1.0);

    println!("anima-hosted: booting somatic loop...");
    block_on(somatic_execution_loop(&mut manager, &monitor)).expect("lifecycle loop failed");

    println!(
        "anima-hosted: final state = {:?}, dispatched = {} tasks",
        manager.state,
        manager.scheduler.dispatched_tasks.len()
    );
}
