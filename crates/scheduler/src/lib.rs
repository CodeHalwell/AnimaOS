#![forbid(unsafe_code)]

use std::collections::VecDeque;

/// Task primitive for autonomous dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// Stable task identifier.
    pub id: u64,
    /// Lower values indicate higher urgency.
    pub mlfq_level: u8,
}

/// Iteration-aware queue for next task selection.
#[derive(Debug, Default, Clone)]
pub struct TaskAgenda {
    queue: VecDeque<Task>,
}

impl TaskAgenda {
    /// Creates an empty agenda.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a task into the queue.
    pub fn push(&mut self, task: Task) {
        self.queue.push_back(task);
    }

    /// Returns true if no tasks are currently queued.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Selects the next task using a stable MLFQ-like ordering.
    pub fn select_optimal_task(&mut self) -> Option<Task> {
        let mut best_index = None;
        let mut best_level = u8::MAX;

        for (idx, task) in self.queue.iter().enumerate() {
            if task.mlfq_level < best_level {
                best_level = task.mlfq_level;
                best_index = Some(idx);
            }
        }

        best_index.and_then(|idx| self.queue.remove(idx))
    }
}

/// Dispatches tasks to execution loops.
#[derive(Debug, Default, Clone)]
pub struct IterationAwareMlfq {
    /// Captures tasks that were dispatched.
    pub dispatched_tasks: Vec<Task>,
}

impl IterationAwareMlfq {
    /// Dispatches a task to an execution primitive.
    pub async fn dispatch_task(&mut self, task: Task) {
        self.dispatched_tasks.push(task);
    }
}
