#![forbid(unsafe_code)]

//! Self-preservation plane: autonomous lifecycle director.

pub mod audit;
pub mod sleep;

pub use audit::{AuditEntry, AuditLog};
pub use sleep::{SleepMaintenanceReport, SleepRoutine, SleepRoutineOutcome};

use std::sync::{Arc, Mutex};
use std::time::Duration;

use interoception::HomeostaticMonitor;
use memory::VirtualContextManager;
use scheduler::{CancellationToken, IterationAwareMlfq, LlmBackend, TaskAgenda};
use senses::{HumanGuidance, SensoryBridge, SensoryBridgeError};

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
#[derive(Clone)]
pub struct LifecycleManager {
    /// Human-readable agent identifier (carried into audit entries).
    pub agent_id: String,
    /// Human sensory bridge.
    pub senses: SensoryBridge,
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
    /// LLM backend used to execute dispatched tasks.
    pub backend: Arc<dyn LlmBackend>,
    /// Per-agent audit log.
    pub audit: AuditLog,
    /// Cancellation handle for the dispatch currently in flight. A fresh
    /// [`CancellationToken`] is installed before every dispatch; external
    /// callers (signal handlers, stress monitors, timeouts) can trip the
    /// running task via [`LifecycleManager::cancel_current_task`].
    task_cancel: Arc<Mutex<CancellationToken>>,
    /// Optional iteration limit to allow bounded runs.
    pub max_iterations: Option<u32>,
    iterations: u32,
}

impl std::fmt::Debug for LifecycleManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LifecycleManager")
            .field("agent_id", &self.agent_id)
            .field("state", &self.state)
            .field("config", &self.config)
            .field("policy_bounds", &self.policy_bounds)
            .field("agenda_len", &self.agenda.len())
            .field("backend", &self.backend.id())
            .field("audit_len", &self.audit.len())
            .field("iterations", &self.iterations)
            .field("max_iterations", &self.max_iterations)
            .finish()
    }
}

impl LifecycleManager {
    /// Constructs a new manager in the awake state.
    pub fn new(
        agent_id: impl Into<String>,
        senses: SensoryBridge,
        memory: VirtualContextManager,
        config: LifecycleConfig,
        initial_bounds: HumanGuidance,
        backend: Arc<dyn LlmBackend>,
        max_iterations: Option<u32>,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            senses,
            memory,
            scheduler: IterationAwareMlfq::default(),
            agenda: TaskAgenda::new(),
            state: LifecycleState::Awake,
            policy_bounds: initial_bounds,
            config,
            backend,
            audit: AuditLog::new(),
            task_cancel: Arc::new(Mutex::new(CancellationToken::new())),
            max_iterations,
            iterations: 0,
        }
    }

    /// Returns a clone of the cancellation handle for the dispatch currently
    /// in flight (or the most recently installed one between dispatches).
    /// Clones share state with the live token, so tripping the clone cancels
    /// the running backend stream.
    pub fn current_cancel_handle(&self) -> CancellationToken {
        self.task_cancel.lock().expect("poisoned").clone()
    }

    /// Trips the cancellation token for the dispatch currently in flight.
    /// Safe to call from any thread that holds a reference to the manager.
    pub fn cancel_current_task(&self) {
        self.task_cancel.lock().expect("poisoned").cancel();
    }

    /// Installs a fresh [`CancellationToken`] and returns a clone bound to it.
    /// Called by [`somatic_execution_loop`] immediately before each dispatch
    /// so previous cancellation requests do not affect the next task.
    fn install_fresh_cancel(&self) -> CancellationToken {
        let fresh = CancellationToken::new();
        let handle = fresh.clone();
        *self.task_cancel.lock().expect("poisoned") = fresh;
        handle
    }

    /// Applies new human policy bounds.
    pub fn update_policy_bounds(&mut self, guidance: HumanGuidance) {
        self.policy_bounds = guidance;
    }

    /// Transitions into sleep state and runs the standard maintenance suite.
    pub async fn transition_to_sleep_state(&mut self) -> Result<(), LifecycleError> {
        if self.state != LifecycleState::Sleep {
            self.state = LifecycleState::Sleep;
            self.audit.push(AuditEntry::SleepEntered {
                agent_id: self.agent_id.clone(),
            });
        }
        let _ = sleep::run_default_maintenance();
        Ok(())
    }

    /// Transitions back into the active waking state.
    pub fn transition_to_waking_state(&mut self) {
        if self.state != LifecycleState::Awake {
            self.state = LifecycleState::Awake;
            self.audit.push(AuditEntry::WakeEntered {
                agent_id: self.agent_id.clone(),
            });
        }
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
        // Apply starvation-prevention boost before selecting the next task.
        // When `boost_interval > 0`, this promotes any Medium/Low tasks that
        // have been waiting for a full `boost_interval`-dispatch window.
        lifecycle.scheduler.check_and_boost(&mut lifecycle.agenda);

        let human_guidance = lifecycle.senses.read_active_bounds()?;
        lifecycle.update_policy_bounds(human_guidance);

        let active_tokens = lifecycle.memory.get_l1_token_count();
        let stress_index =
            monitor.compute_systemic_stress_index(active_tokens, lifecycle.config.max_context);

        let is_idle = if lifecycle.agenda.is_empty() && stress_index < 0.4 {
            lifecycle.transition_to_sleep_state().await?;
            true
        } else if let Some(task) = lifecycle.agenda.select_optimal_task() {
            if lifecycle.state == LifecycleState::Sleep {
                lifecycle.transition_to_waking_state();
            }
            let agent_id = lifecycle.agent_id.clone();
            let task_id = task.id;
            let tier = task.mlfq_level;
            let prompt = task.prompt.clone();

            lifecycle.audit.push(AuditEntry::TaskStarted {
                agent_id: agent_id.clone(),
                task_id,
                tier,
                prompt,
            });

            let cancel = lifecycle.install_fresh_cancel();
            let backend = Arc::clone(&lifecycle.backend);
            let dispatch_result = lifecycle
                .scheduler
                .dispatch_task(task, &*backend, &cancel)
                .await;

            match dispatch_result {
                Ok(outcome) => {
                    lifecycle.memory.add_tokens(outcome.tokens_emitted);
                    lifecycle.audit.push(AuditEntry::TaskCompleted {
                        agent_id,
                        task_id,
                        tokens_emitted: outcome.tokens_emitted,
                        response: outcome.response,
                    });
                }
                Err(error) => {
                    lifecycle.audit.push(AuditEntry::TaskFailed {
                        agent_id,
                        task_id,
                        error: format!("{error:?}"),
                    });
                }
            }
            false
        } else {
            true
        };

        if is_idle {
            std::thread::sleep(Duration::from_millis(1));
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
    use interoception::HomeostaticMonitor;
    use scheduler::{MockLlmBackend, Task};
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut future = Box::pin(future);
        loop {
            match future.as_mut().poll(&mut cx) {
                Poll::Ready(value) => return value,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    fn manager(agent_id: &str, max_iterations: Option<u32>) -> LifecycleManager {
        LifecycleManager::new(
            agent_id,
            SensoryBridge::new(HumanGuidance {
                policy_hint: "test".to_string(),
            }),
            VirtualContextManager::with_capacity(0, 1000),
            LifecycleConfig { max_context: 1000 },
            HumanGuidance {
                policy_hint: "initial".to_string(),
            },
            Arc::new(MockLlmBackend::new()),
            max_iterations,
        )
    }

    #[test]
    fn lifecycle_enters_sleep_when_idle_and_low_stress() {
        let mut m = manager("agent-a", Some(1));
        let mut monitor = HomeostaticMonitor::new(1.0, 0.5, 16);
        monitor.record_ttft(0.0);

        block_on(somatic_execution_loop(&mut m, &monitor)).unwrap();

        assert_eq!(m.state, LifecycleState::Sleep);
        assert!(m
            .audit
            .entries()
            .iter()
            .any(|e| matches!(e, AuditEntry::SleepEntered { .. })));
    }

    #[test]
    fn lifecycle_dispatches_available_tasks_and_records_audit() {
        let mut m = manager("agent-b", Some(1));
        m.agenda.push(Task::new(42, 1, "hello mock backend"));

        let mut monitor = HomeostaticMonitor::new(1.0, 0.5, 16);
        monitor.record_ttft(2.0);

        block_on(somatic_execution_loop(&mut m, &monitor)).unwrap();

        assert_eq!(m.state, LifecycleState::Awake);
        assert_eq!(m.scheduler.dispatched_tasks.len(), 1);
        assert_eq!(m.scheduler.dispatched_tasks[0].id, 42);

        let entries = m.audit.entries();
        assert!(matches!(
            entries.first(),
            Some(AuditEntry::TaskStarted { task_id: 42, .. })
        ));
        let completed = entries
            .iter()
            .find_map(|e| match e {
                AuditEntry::TaskCompleted {
                    task_id,
                    tokens_emitted,
                    response,
                    ..
                } if *task_id == 42 => Some((*tokens_emitted, response.clone())),
                _ => None,
            })
            .expect("expected TaskCompleted entry");
        assert_eq!(completed.0, 3);
        assert_eq!(completed.1, "hello mock backend ");
    }

    #[test]
    fn cancel_current_task_trips_handle_observed_externally() {
        let m = manager("agent-c", None);

        // A handle obtained before installation must remain tied to the
        // token currently stored on the manager.
        let pre = m.current_cancel_handle();
        assert!(!pre.is_cancelled());

        // install_fresh_cancel must replace the stored token and return a
        // clone that shares state with future current_cancel_handle reads.
        let installed = m.install_fresh_cancel();
        let observed = m.current_cancel_handle();
        assert!(!installed.is_cancelled());
        assert!(!observed.is_cancelled());

        // Tripping via the public API must propagate to both the freshly
        // installed handle and any later reads.
        m.cancel_current_task();
        assert!(installed.is_cancelled());
        assert!(observed.is_cancelled());
        // The pre-install handle is now detached and stays untripped.
        assert!(!pre.is_cancelled());
    }
}
