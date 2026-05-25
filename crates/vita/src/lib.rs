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
use scheduler::{CancellationToken, IterationAwareMlfq, LlmBackend, Task, TaskAgenda};
use senses::{HumanGuidance, SensoryBridge, SensoryBridgeError, SensoryPriority};

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
    /// Cancellation handle for the dispatch currently in flight.  A fresh
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

    /// Transitions into the sleep state, emitting a [`AuditEntry::SleepEntered`]
    /// entry followed by audited maintenance-phase entries for each of the four
    /// sleep routines (E3.4 exit criterion 1).
    ///
    /// Calling this method when already in the sleep state is a no-op to
    /// prevent duplicate transitions from the somatic loop.
    pub async fn transition_to_sleep_state(&mut self) -> Result<(), LifecycleError> {
        if self.state != LifecycleState::Sleep {
            self.state = LifecycleState::Sleep;
            self.audit.push(AuditEntry::SleepEntered {
                agent_id: self.agent_id.clone(),
            });
            // Run all four maintenance phases with per-phase audit entries.
            let agent_id = self.agent_id.clone();
            sleep::run_maintenance_audited(&agent_id, &mut self.audit);
        }
        Ok(())
    }

    /// Transitions back into the active waking state, emitting a
    /// [`AuditEntry::WakeEntered`] entry.
    pub fn transition_to_waking_state(&mut self) {
        if self.state != LifecycleState::Awake {
            self.state = LifecycleState::Awake;
            self.audit.push(AuditEntry::WakeEntered {
                agent_id: self.agent_id.clone(),
            });
        }
    }

    /// Explicitly runs one complete sleep-maintenance cycle (all four phases)
    /// with full audit logging.
    ///
    /// Unlike [`transition_to_sleep_state`], this method always runs the cycle
    /// regardless of the current state — useful for on-demand maintenance and
    /// the E3.4 soak test.
    pub fn run_sleep_cycle(&mut self) -> SleepMaintenanceReport {
        let agent_id = self.agent_id.clone();
        sleep::run_maintenance_audited(&agent_id, &mut self.audit)
    }

    fn should_stop(&self) -> bool {
        self.max_iterations
            .map(|limit| self.iterations >= limit)
            .unwrap_or(false)
    }
}

// ── Sensory-priority → MLFQ-tier mapping ─────────────────────────────────────

/// Maps a [`SensoryPriority`] to the numeric MLFQ tier used by [`Task::new`].
///
/// | Priority | MLFQ tier | Notes                          |
/// |----------|-----------|-------------------------------|
/// | Critical | 0         | Highest — interrupt-level      |
/// | High     | 0         | Highest — operator urgency     |
/// | Normal   | 1         | Medium — standard interaction  |
/// | Low      | 2         | Lowest — background work       |
fn priority_to_mlfq_tier(priority: SensoryPriority) -> u8 {
    match priority {
        SensoryPriority::Critical | SensoryPriority::High => 0,
        SensoryPriority::Normal => 1,
        SensoryPriority::Low => 2,
    }
}

/// Autonomous lifecycle control loop.
///
/// Each iteration:
/// 1. **Starvation boost** — promotes waiting Medium/Low tasks to High if the
///    boost interval has elapsed.
/// 2. **Sensory ingestion** — drains all pending priority-tagged packets from
///    the senses bridge and enqueues them as MLFQ tasks (E3.3 exit criterion 1:
///    text and voice reach `vita` as priority-tagged packets).
/// 3. **Policy update** — refreshes the active policy bounds from the bridge.
/// 4. **Stress check** — computes the homeostatic stress index via the
///    interoception monitor.
/// 5. **Sleep / wake decision** — the agent sleeps when the agenda is empty
///    (normal or emergency maintenance); it wakes when the agenda is non-empty,
///    which happens automatically when sensory packets are converted to tasks in
///    step 2 (E3.4 exit criterion: wake on sensory event).
/// 6. **Task dispatch** — selects and dispatches the optimal task through the
///    LLM backend, updating working memory and recording the outcome.
pub async fn somatic_execution_loop(
    lifecycle: &mut LifecycleManager,
    monitor: &HomeostaticMonitor,
) -> Result<(), LifecycleError> {
    // Sensory-derived task IDs start at 2^63 to avoid collisions with
    // caller-supplied IDs which conventionally use the lower half.
    let mut sensory_task_id: u64 = 1u64 << 63;

    loop {
        // ── 1. Starvation-prevention boost ───────────────────────────────────
        lifecycle.scheduler.check_and_boost(&mut lifecycle.agenda);

        // ── 2. Sensory ingestion (E3.3) ───────────────────────────────────────
        // Drain all pending priority-tagged packets and promote them to agenda
        // tasks.  This also serves as the sensory-wake trigger for E3.4:
        // if packets arrive while the agent is sleeping, they are converted to
        // tasks here, and the subsequent agenda check transitions back to Awake.
        while let Some(pkt) = lifecycle.senses.next_prioritized_packet() {
            use senses::SensoryPacket;
            let tier = priority_to_mlfq_tier(pkt.priority);
            let prompt = match &pkt.packet {
                SensoryPacket::Text(t) => t.clone(),
                SensoryPacket::Pcm(samples) => format!("[PCM {} samples]", samples.len()),
            };
            lifecycle
                .agenda
                .push(Task::new(sensory_task_id, tier, prompt));
            sensory_task_id = sensory_task_id.wrapping_add(1);
        }

        // ── 3. Policy update ──────────────────────────────────────────────────
        let human_guidance = lifecycle.senses.read_active_bounds()?;
        lifecycle.update_policy_bounds(human_guidance);

        // ── 4. Stress index ───────────────────────────────────────────────────
        let active_tokens = lifecycle.memory.get_l1_token_count();
        let _stress_index =
            monitor.compute_systemic_stress_index(active_tokens, lifecycle.config.max_context);

        // ── 5. Sleep / wake decision (E3.4) ──────────────────────────────────
        // Sleep whenever the agenda is empty; wake when a task is available.
        // Sensory packets converted in step 2 re-populate the agenda, providing
        // the implicit "wake on sensory event" trigger.
        let is_idle = if lifecycle.agenda.is_empty() {
            lifecycle.transition_to_sleep_state().await?;
            true
        } else if let Some(task) = lifecycle.agenda.select_optimal_task() {
            // ── 6. Task dispatch ──────────────────────────────────────────────
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use interoception::HomeostaticMonitor;
    use scheduler::{MockLlmBackend, Task};
    use senses::SensoryPriority;
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
            SensoryBridge::new(HumanGuidance::new("test")),
            VirtualContextManager::with_capacity(0, 1000),
            LifecycleConfig { max_context: 1000 },
            HumanGuidance::new("initial"),
            Arc::new(MockLlmBackend::new()),
            max_iterations,
        )
    }

    // ── Existing tests (backward-compat) ──────────────────────────────────────

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

        let pre = m.current_cancel_handle();
        assert!(!pre.is_cancelled());

        let installed = m.install_fresh_cancel();
        let observed = m.current_cancel_handle();
        assert!(!installed.is_cancelled());
        assert!(!observed.is_cancelled());

        m.cancel_current_task();
        assert!(installed.is_cancelled());
        assert!(observed.is_cancelled());
        assert!(!pre.is_cancelled());
    }

    // ── E3.3 — Sensory Bridge integration ────────────────────────────────────

    #[test]
    fn text_packet_reaches_vita_and_is_dispatched_as_high_priority_task() {
        // Inject a High-priority text packet before running the loop.
        let mut m = manager("agent-sensory", Some(2));
        m.senses
            .packetize_text_checked("urgent operator query", SensoryPriority::High)
            .expect("valid text should be accepted");

        let monitor = HomeostaticMonitor::new(1.0, 0.5, 16);

        block_on(somatic_execution_loop(&mut m, &monitor)).unwrap();

        // The sensory packet should have been converted to a task and dispatched.
        assert_eq!(
            m.scheduler.dispatched_tasks.len(),
            1,
            "one sensory-derived task should have been dispatched"
        );
        // A High-priority sensory packet maps to MLFQ tier 0 (High).
        assert_eq!(
            m.scheduler.dispatched_tasks[0].mlfq_level, 0,
            "high-priority sensory task must be in MLFQ tier 0"
        );
    }

    #[test]
    fn voice_pcm_packet_reaches_vita_and_is_dispatched_as_task() {
        let mut m = manager("agent-pcm", Some(2));
        m.senses
            .packetize_pcm_checked(vec![100i16; 160], SensoryPriority::Normal)
            .expect("valid PCM frame should be accepted");

        let monitor = HomeostaticMonitor::new(1.0, 0.5, 16);

        block_on(somatic_execution_loop(&mut m, &monitor)).unwrap();

        assert_eq!(
            m.scheduler.dispatched_tasks.len(),
            1,
            "PCM sensory packet should produce one dispatched task"
        );
    }

    #[test]
    fn critical_sensory_packet_maps_to_mlfq_tier_zero() {
        let mut m = manager("agent-critical", Some(2));
        m.senses
            .packetize_text_checked("CRITICAL override", SensoryPriority::Critical)
            .expect("valid text");

        let monitor = HomeostaticMonitor::new(1.0, 0.5, 16);
        block_on(somatic_execution_loop(&mut m, &monitor)).unwrap();

        assert_eq!(m.scheduler.dispatched_tasks[0].mlfq_level, 0);
    }

    #[test]
    fn low_priority_sensory_packet_maps_to_mlfq_tier_two() {
        let mut m = manager("agent-low", Some(2));
        m.senses
            .packetize_text_checked("background note", SensoryPriority::Low)
            .expect("valid text");

        let monitor = HomeostaticMonitor::new(1.0, 0.5, 16);
        block_on(somatic_execution_loop(&mut m, &monitor)).unwrap();

        assert_eq!(m.scheduler.dispatched_tasks[0].mlfq_level, 2);
    }

    #[test]
    fn sensory_event_during_sleep_triggers_wake_transition() {
        let monitor = HomeostaticMonitor::new(1.0, 0.5, 16);

        // Run 1 idle iteration so the agent enters sleep.
        let mut m1 = manager("agent-wake", Some(1));
        block_on(somatic_execution_loop(&mut m1, &monitor)).unwrap();
        assert_eq!(m1.state, LifecycleState::Sleep, "should enter sleep");

        // Inject a sensory packet and run one more iteration.
        // Use the unchecked method (no policy bounds required for test inputs).
        m1.senses.packetize_text("wake me up");
        m1.max_iterations = Some(2);
        block_on(somatic_execution_loop(&mut m1, &monitor)).unwrap();

        // The sensory packet should have been converted to a task and dispatched.
        assert_eq!(
            m1.scheduler.dispatched_tasks.len(),
            1,
            "sensory event during sleep should produce a dispatched task"
        );
        assert_eq!(
            m1.state,
            LifecycleState::Awake,
            "agent should be awake after dispatch"
        );
    }

    // ── E3.4 — Wake/Sleep transitions audited end-to-end ─────────────────────

    #[test]
    fn sleep_transition_audits_all_four_phases_in_order() {
        let mut m = manager("agent-sleep-audit", Some(1));
        let monitor = HomeostaticMonitor::new(1.0, 0.5, 16);

        block_on(somatic_execution_loop(&mut m, &monitor)).unwrap();

        assert_eq!(m.state, LifecycleState::Sleep);

        let entries = m.audit.entries();
        // Expect: SleepEntered + 4×(PhaseStarted + PhaseCompleted) = 9 entries.
        assert_eq!(entries.len(), 9, "should have 9 audit entries");

        assert!(matches!(entries[0], AuditEntry::SleepEntered { .. }));

        let phase_names = [
            "MemoryPruning",
            "GenerativeReplay",
            "DreamExploration",
            "PolicyCompilation",
        ];
        for (i, phase) in phase_names.iter().enumerate() {
            let start_idx = 1 + i * 2;
            let complete_idx = start_idx + 1;
            assert!(
                matches!(
                    &entries[start_idx],
                    AuditEntry::SleepPhaseStarted { phase: p, .. } if p == phase
                ),
                "entry {start_idx} should be SleepPhaseStarted for {phase}"
            );
            assert!(
                matches!(
                    &entries[complete_idx],
                    AuditEntry::SleepPhaseCompleted { phase: p, success: true, .. } if p == phase
                ),
                "entry {complete_idx} should be SleepPhaseCompleted for {phase}"
            );
        }
    }

    #[test]
    fn wake_transition_is_audited_after_sleep() {
        // Iteration 1: no tasks → sleep (audited).
        // Iteration 2: task arrives → wake (audited) → dispatch.
        let mut m = manager("agent-wake-audit", Some(2));
        m.agenda.push(Task::new(99, 0, "wake-up task"));

        let monitor = HomeostaticMonitor::new(1.0, 0.5, 16);

        // Pre-drain the agenda so first iteration sleeps.
        // Actually, with a task in the agenda, iteration 1 dispatches.
        // Use a different scenario: run 1 idle, then 1 with task.
        let mut m_idle = manager("agent-wake-audit", Some(1));
        block_on(somatic_execution_loop(&mut m_idle, &monitor)).unwrap();
        assert_eq!(m_idle.state, LifecycleState::Sleep);

        // Push a task and allow one more iteration.
        m_idle.agenda.push(Task::new(99, 0, "wake-up task"));
        m_idle.max_iterations = Some(2);
        block_on(somatic_execution_loop(&mut m_idle, &monitor)).unwrap();

        let entries = m_idle.audit.entries();
        let has_wake = entries
            .iter()
            .any(|e| matches!(e, AuditEntry::WakeEntered { .. }));
        assert!(has_wake, "WakeEntered must appear in the audit log");
    }

    /// E3.4 exit criterion 2: 100 consecutive sleep cycles complete without error.
    #[test]
    fn one_hundred_sleep_cycles_complete_without_error() {
        let mut m = manager("soak-agent", None);

        for cycle in 0..100 {
            let report = m.run_sleep_cycle();
            assert!(
                report.all_completed(),
                "sleep cycle {cycle} reported incomplete: {report:?}"
            );
        }

        // Verify: 100 cycles × 4 phases = 400 SleepPhaseCompleted entries,
        // all with success = true.
        let completed_count = m
            .audit
            .entries()
            .iter()
            .filter(|e| matches!(e, AuditEntry::SleepPhaseCompleted { success: true, .. }))
            .count();
        assert_eq!(
            completed_count, 400,
            "expected 400 successful SleepPhaseCompleted entries (100 cycles × 4 phases)"
        );

        // All started phases must have a paired completion.
        let started_count = m
            .audit
            .entries()
            .iter()
            .filter(|e| matches!(e, AuditEntry::SleepPhaseStarted { .. }))
            .count();
        assert_eq!(
            started_count, 400,
            "started count must equal completed count"
        );
    }
}
