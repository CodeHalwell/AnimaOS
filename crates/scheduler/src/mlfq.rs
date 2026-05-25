//! Iteration-aware multi-level feedback queue with three priority tiers.

use std::collections::VecDeque;

use crate::backend::{CancellationToken, LlmBackend, LlmBackendError, StreamingCompletion};
use crate::Task;

/// Number of MLFQ priority tiers.
pub const NUM_TIERS: usize = 3;

/// Symbolic tier names for convenience.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MlfqTier {
    /// Highest priority - interactive / human-guided work.
    High = 0,
    /// Default tier for background tasks.
    Medium = 1,
    /// Bulk / consolidation work.
    Low = 2,
}

impl MlfqTier {
    /// Maps the tier into its 0-based index.
    pub fn index(self) -> usize {
        self as usize
    }
}

/// Iteration-aware queue for next task selection.
#[derive(Debug, Default, Clone)]
pub struct TaskAgenda {
    tiers: [VecDeque<Task>; NUM_TIERS],
}

impl TaskAgenda {
    /// Creates an empty agenda.
    pub fn new() -> Self {
        Self {
            tiers: Default::default(),
        }
    }

    /// Adds a task into the queue at its declared MLFQ level (clamped).
    pub fn push(&mut self, task: Task) {
        let tier = (task.mlfq_level as usize).min(NUM_TIERS - 1);
        self.tiers[tier].push_back(task);
    }

    /// Returns true if no tasks are currently queued in any tier.
    pub fn is_empty(&self) -> bool {
        self.tiers.iter().all(|t| t.is_empty())
    }

    /// Total number of pending tasks across all tiers.
    pub fn len(&self) -> usize {
        self.tiers.iter().map(|t| t.len()).sum()
    }

    /// Selects the next task from the highest-priority non-empty tier.
    pub fn select_optimal_task(&mut self) -> Option<Task> {
        for tier in self.tiers.iter_mut() {
            if let Some(task) = tier.pop_front() {
                return Some(task);
            }
        }
        None
    }
}

/// Result of a successful dispatch through an [`LlmBackend`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskOutcome {
    /// Task that was executed.
    pub task: Task,
    /// Concatenated response text from streamed tokens.
    pub response: String,
    /// Number of [`StreamingCompletion::Token`] events observed.
    pub tokens_emitted: u32,
}

/// Dispatches tasks to execution loops, tracking per-tier statistics.
#[derive(Debug, Default, Clone)]
pub struct IterationAwareMlfq {
    /// Captures tasks that were dispatched.
    pub dispatched_tasks: Vec<Task>,
    /// Per-tier dispatch counters.
    pub tier_counters: [u64; NUM_TIERS],
}

impl IterationAwareMlfq {
    /// Dispatches a task through the provided LLM backend, draining the streaming
    /// completion into a single [`TaskOutcome`]. Records per-tier dispatch stats
    /// regardless of success or failure so accounting stays honest.
    pub async fn dispatch_task(
        &mut self,
        task: Task,
        backend: &dyn LlmBackend,
        cancel: &CancellationToken,
    ) -> Result<TaskOutcome, LlmBackendError> {
        let tier = (task.mlfq_level as usize).min(NUM_TIERS - 1);
        self.tier_counters[tier] = self.tier_counters[tier].saturating_add(1);

        let stream = backend.stream_completion(&task.prompt, cancel).await?;

        let mut response = String::new();
        let mut tokens_emitted: u32 = 0;
        for event in stream {
            match event {
                StreamingCompletion::Token(t) => {
                    tokens_emitted = tokens_emitted.saturating_add(1);
                    response.push_str(&t);
                }
                StreamingCompletion::Done => {}
                StreamingCompletion::Cancelled => return Err(LlmBackendError::Cancelled),
            }
        }

        self.dispatched_tasks.push(task.clone());
        Ok(TaskOutcome {
            task,
            response,
            tokens_emitted,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_pulls_from_high_priority_first() {
        let mut agenda = TaskAgenda::new();
        agenda.push(Task::new(2, 2, ""));
        agenda.push(Task::new(1, 0, ""));

        let next = agenda.select_optimal_task().unwrap();
        assert_eq!(next.id, 1);
        let next = agenda.select_optimal_task().unwrap();
        assert_eq!(next.id, 2);
        assert!(agenda.select_optimal_task().is_none());
    }

    #[test]
    fn out_of_range_level_clamps_to_lowest_tier() {
        let mut agenda = TaskAgenda::new();
        agenda.push(Task::new(99, 250, ""));
        let next = agenda.select_optimal_task().unwrap();
        assert_eq!(next.id, 99);
    }

    #[test]
    fn dispatch_through_mock_backend_returns_response() {
        use crate::backend::CancellationToken;
        use crate::mock::MockLlmBackend;
        use std::future::Future;
        use std::pin::Pin;
        use std::task::{Context, Poll, Waker};

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

        let mut sched = IterationAwareMlfq::default();
        let backend = MockLlmBackend::new();
        let cancel = CancellationToken::new();
        let outcome =
            block_on(sched.dispatch_task(Task::new(7, 1, "alpha beta gamma"), &backend, &cancel))
                .expect("dispatch should succeed");

        assert_eq!(outcome.task.id, 7);
        assert_eq!(outcome.tokens_emitted, 3);
        assert_eq!(outcome.response, "alpha beta gamma ");
        assert_eq!(sched.dispatched_tasks.len(), 1);
        assert_eq!(sched.tier_counters[1], 1);
    }
}
