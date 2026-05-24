#![forbid(unsafe_code)]

use memory::VirtualContextManager;
use observe::HomeostaticMonitor;
use scheduler::{IterationAwareMlfq, TaskAgenda};
use sensory_bridge::{HumanGuidance, SensoryBridge, SensoryBridgeError};

/// Lifecycle runtime state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleState {
    /// Active sensorimotor execution loop.
    Awake,
    /// Maintenance and consolidation mode.
    Sleep,
}

/// Runtime limits supplied to lifecycle control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleConfig {
    /// Maximum context window available to the active agent.
    pub max_context: u32,
}

/// Errors from lifecycle operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleError {
    /// Sensory bridge read failed.
    SensoryInput(SensoryBridgeError),
}

impl From<SensoryBridgeError> for LifecycleError {
    fn from(value: SensoryBridgeError) -> Self {
        Self::SensoryInput(value)
    }
}

/// Primary autonomous lifecycle manager.
#[derive(Debug, Clone)]
pub struct LifecycleManager {
    /// Human sensory bridge.
    pub sensory_bridge: SensoryBridge,
    /// Active working memory.
    pub memory: VirtualContextManager,
    /// Task scheduler.
    pub scheduler: IterationAwareMlfq,
    /// Agenda of pending tasks.
    pub agenda: TaskAgenda,
    /// Active lifecycle state.
    pub state: LifecycleState,
    /// Current policy bounds consumed from the human signal channel.
    pub policy_bounds: HumanGuidance,
    /// Runtime configuration.
    pub config: LifecycleConfig,
    /// Optional iteration limit to allow bounded runs.
    pub max_iterations: Option<u32>,
    iterations: u32,
}

impl LifecycleManager {
    /// Applies new human policy bounds.
    pub fn update_policy_bounds(&mut self, guidance: HumanGuidance) {
        self.policy_bounds = guidance;
    }

    /// Transitions into sleep state.
    pub async fn transition_to_sleep_state(&mut self) -> Result<(), LifecycleError> {
        self.state = LifecycleState::Sleep;
        Ok(())
    }

    fn should_stop(&self) -> bool {
        self.max_iterations
            .map(|limit| self.iterations >= limit)
            .unwrap_or(false)
    }
}

/// Autonomous lifecycle control loop for waking/sleep transitions.
pub async fn somatic_execution_loop(
    lifecycle: &mut LifecycleManager,
    monitor: &HomeostaticMonitor,
) -> Result<(), LifecycleError> {
    loop {
        let human_guidance = lifecycle.sensory_bridge.read_active_bounds().await?;
        lifecycle.update_policy_bounds(human_guidance);

        let active_tokens = lifecycle.memory.get_l1_token_count();
        let stress_index =
            monitor.compute_systemic_stress_index(active_tokens, lifecycle.config.max_context);

        if lifecycle.agenda.is_empty() && stress_index < 0.4 {
            lifecycle.transition_to_sleep_state().await?;
        } else if let Some(task) = lifecycle.agenda.select_optimal_task() {
            lifecycle.scheduler.dispatch_task(task).await;
        } else {
            std::thread::yield_now();
        }

        lifecycle.iterations = lifecycle.iterations.saturating_add(1);
        if lifecycle.should_stop() {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scheduler::Task;
    use std::collections::VecDeque;
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll, Waker};

    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut future = Pin::from(Box::new(future));
        loop {
            match future.as_mut().poll(&mut cx) {
                Poll::Ready(value) => return value,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    #[test]
    fn lifecycle_enters_sleep_when_idle_and_low_stress() {
        let mut manager = LifecycleManager {
            sensory_bridge: SensoryBridge::new(HumanGuidance {
                policy_hint: "low-cost".to_string(),
            }),
            memory: VirtualContextManager::new(10),
            scheduler: IterationAwareMlfq::default(),
            agenda: TaskAgenda::new(),
            state: LifecycleState::Awake,
            policy_bounds: HumanGuidance {
                policy_hint: "initial".to_string(),
            },
            config: LifecycleConfig { max_context: 1000 },
            max_iterations: Some(1),
            iterations: 0,
        };

        let monitor = HomeostaticMonitor {
            rolling_ttft: VecDeque::new(),
            baseline_ttft: 1.0,
            beta: 0.5,
        };

        let result = block_on(somatic_execution_loop(&mut manager, &monitor));

        assert!(result.is_ok());
        assert_eq!(manager.state, LifecycleState::Sleep);
        assert_eq!(manager.policy_bounds.policy_hint, "low-cost");
    }

    #[test]
    fn lifecycle_dispatches_available_tasks() {
        let mut agenda = TaskAgenda::new();
        agenda.push(Task {
            id: 42,
            mlfq_level: 1,
        });

        let mut manager = LifecycleManager {
            sensory_bridge: SensoryBridge::new(HumanGuidance {
                policy_hint: "prioritize-tooling".to_string(),
            }),
            memory: VirtualContextManager::new(400),
            scheduler: IterationAwareMlfq::default(),
            agenda,
            state: LifecycleState::Awake,
            policy_bounds: HumanGuidance {
                policy_hint: "initial".to_string(),
            },
            config: LifecycleConfig { max_context: 800 },
            max_iterations: Some(1),
            iterations: 0,
        };

        let monitor = HomeostaticMonitor {
            rolling_ttft: VecDeque::from(vec![2.0]),
            baseline_ttft: 1.0,
            beta: 0.5,
        };

        let result = block_on(somatic_execution_loop(&mut manager, &monitor));

        assert!(result.is_ok());
        assert_eq!(manager.state, LifecycleState::Awake);
        assert_eq!(manager.scheduler.dispatched_tasks.len(), 1);
        assert_eq!(manager.scheduler.dispatched_tasks[0].id, 42);
    }
}
