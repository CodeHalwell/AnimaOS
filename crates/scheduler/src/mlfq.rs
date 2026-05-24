//! Iteration-aware multi-level feedback queue with three priority tiers.

use std::collections::VecDeque;

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

/// Dispatches tasks to execution loops, tracking per-tier statistics.
#[derive(Debug, Default, Clone)]
pub struct IterationAwareMlfq {
    /// Captures tasks that were dispatched.
    pub dispatched_tasks: Vec<Task>,
    /// Per-tier dispatch counters.
    pub tier_counters: [u64; NUM_TIERS],
}

impl IterationAwareMlfq {
    /// Dispatches a task to an execution primitive.
    pub async fn dispatch_task(&mut self, task: Task) {
        let tier = (task.mlfq_level as usize).min(NUM_TIERS - 1);
        self.tier_counters[tier] = self.tier_counters[tier].saturating_add(1);
        self.dispatched_tasks.push(task);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_pulls_from_high_priority_first() {
        let mut agenda = TaskAgenda::new();
        agenda.push(Task {
            id: 2,
            mlfq_level: 2,
        });
        agenda.push(Task {
            id: 1,
            mlfq_level: 0,
        });

        let next = agenda.select_optimal_task().unwrap();
        assert_eq!(next.id, 1);
        let next = agenda.select_optimal_task().unwrap();
        assert_eq!(next.id, 2);
        assert!(agenda.select_optimal_task().is_none());
    }

    #[test]
    fn out_of_range_level_clamps_to_lowest_tier() {
        let mut agenda = TaskAgenda::new();
        agenda.push(Task {
            id: 99,
            mlfq_level: 250,
        });
        let next = agenda.select_optimal_task().unwrap();
        assert_eq!(next.id, 99);
    }
}
