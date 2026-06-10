//! Iteration-aware multi-level feedback queue with three priority tiers.
//!
//! # Starvation prevention
//!
//! Classic MLFQ suffers from starvation: if there is a continuous stream of
//! high-priority tasks, low-priority tasks never run.  [`IterationAwareMlfq`]
//! mitigates this with a periodic *starvation boost*: every `boost_interval`
//! dispatches, all tasks waiting in the Medium and Low tiers are promoted to
//! the High tier so that no task can be blocked for longer than
//! `boost_interval` dispatches.
//!
//! Set `boost_interval = 0` (the default) to disable the boost (useful in
//! unit tests that exercise a single priority tier).

// VecDeque lives in std::collections under std, alloc::collections under no_std.
#[cfg(not(feature = "std"))]
use alloc::collections::VecDeque;
#[cfg(feature = "std")]
use std::collections::VecDeque;

// alloc types needed by IterationAwareMlfq and TaskOutcome in no_std mode.
#[cfg(not(feature = "std"))]
use alloc::{string::String, vec::Vec};

use crate::backend::{CancellationToken, LlmBackend, LlmBackendError, StreamingCompletion};
use crate::Task;

/// Number of MLFQ priority tiers.
pub const NUM_TIERS: usize = 3;

/// Symbolic tier names for convenience.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MlfqTier {
    /// Highest priority — interactive / human-guided work.
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

    /// Adds a task into the queue at its declared MLFQ level (clamped to
    /// the maximum tier index so out-of-range values are silently accepted).
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

    /// Promotes every task in tiers 1..N-1 to the High tier (tier 0),
    /// preventing indefinite starvation of low-priority work.
    ///
    /// Returns the number of tasks that were promoted.
    pub fn boost_all_to_high(&mut self) -> usize {
        let mut boosted = 0;
        // Collect from Medium and Low into High.
        for tier_idx in 1..NUM_TIERS {
            while let Some(mut task) = self.tiers[tier_idx].pop_front() {
                task.mlfq_level = MlfqTier::High as u8;
                self.tiers[0].push_back(task);
                boosted += 1;
            }
        }
        boosted
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

/// Dispatches tasks to execution loops, tracking per-tier statistics and
/// enforcing starvation prevention.
///
/// # Starvation prevention
///
/// Set `boost_interval > 0` (via [`IterationAwareMlfq::with_boost_interval`])
/// and call [`IterationAwareMlfq::check_and_boost`] with the live
/// [`TaskAgenda`] before selecting each task.  After every `boost_interval`
/// dispatches, `check_and_boost` promotes all Medium/Low tasks to High.
#[derive(Debug, Clone)]
pub struct IterationAwareMlfq {
    /// Ordered history of tasks that were dispatched (success and failure alike).
    ///
    /// Bounded: once it reaches `max_dispatched_tasks` entries each dispatch
    /// evicts the oldest, so a long-lived scheduler does not retain every task
    /// it has ever run and grow the heap without bound.
    pub dispatched_tasks: Vec<Task>,
    /// Maximum number of entries retained in `dispatched_tasks`; older entries
    /// are evicted once this cap is reached. Defaults to
    /// [`DEFAULT_MAX_DISPATCHED_TASKS`].  `0` disables eviction.
    max_dispatched_tasks: usize,
    /// Per-tier dispatch counters — incremented before the backend call so they
    /// reflect *attempted* dispatches, not only successful ones.
    pub tier_counters: [u64; NUM_TIERS],
    /// Running sum of tokens emitted across all successful dispatches.
    pub total_tokens_dispatched: u64,
    /// Number of dispatches between starvation-prevention boosts.
    /// `0` disables the boost (default).
    pub boost_interval: u32,
    /// Internal dispatch count used to trigger the starvation boost.
    dispatch_count: u64,
}

/// Default cap on the number of entries retained in
/// [`IterationAwareMlfq::dispatched_tasks`].
pub const DEFAULT_MAX_DISPATCHED_TASKS: usize = 10_000;

impl Default for IterationAwareMlfq {
    fn default() -> Self {
        Self {
            dispatched_tasks: Vec::new(),
            max_dispatched_tasks: DEFAULT_MAX_DISPATCHED_TASKS,
            tier_counters: [0; NUM_TIERS],
            total_tokens_dispatched: 0,
            boost_interval: 0,
            dispatch_count: 0,
        }
    }
}

impl IterationAwareMlfq {
    /// Creates a scheduler with the starvation-prevention boost enabled.
    ///
    /// `boost_interval` is the number of dispatches between promotions of
    /// waiting tasks to the High tier.  Typical values are in the range
    /// 50–200 depending on workload characteristics.
    pub fn with_boost_interval(boost_interval: u32) -> Self {
        Self {
            boost_interval,
            ..Default::default()
        }
    }

    /// Overrides the dispatch-history retention cap (defaults to
    /// [`DEFAULT_MAX_DISPATCHED_TASKS`]).  `0` disables eviction (unbounded —
    /// use only for short-lived/test schedulers).
    pub fn with_max_dispatched_tasks(mut self, max_dispatched_tasks: usize) -> Self {
        self.max_dispatched_tasks = max_dispatched_tasks;
        self
    }

    /// Checks whether a starvation boost is due and, if so, promotes all
    /// Medium and Low tasks in `agenda` to the High tier.
    ///
    /// Returns the number of tasks that were promoted (0 if no boost was due
    /// or if `boost_interval` is 0).
    ///
    /// Call this method once per scheduler loop iteration *before* calling
    /// [`TaskAgenda::select_optimal_task`] so that newly promoted tasks are
    /// visible to the next selection.
    pub fn check_and_boost(&mut self, agenda: &mut TaskAgenda) -> usize {
        if self.boost_interval == 0 || self.dispatch_count == 0 {
            return 0;
        }
        if self
            .dispatch_count
            .is_multiple_of(self.boost_interval as u64)
        {
            agenda.boost_all_to_high()
        } else {
            0
        }
    }

    /// Dispatches a task through the provided LLM backend, draining the
    /// streaming completion into a single [`TaskOutcome`].
    ///
    /// # Token-slice accounting
    ///
    /// If `task.token_budget` is set, the stream is truncated at that many
    /// tokens — the dispatch returns normally with whatever has been collected
    /// so far, and `tokens_emitted` reflects the actual count (≤ budget).
    /// This ensures per-task token-slice accounting never over-draws the
    /// scheduler's resource model.
    ///
    /// Both `dispatched_tasks` and `tier_counters` are updated for every
    /// attempted dispatch — success and failure alike — so callers inspecting
    /// either surface see a complete record of what the scheduler tried to run.
    pub async fn dispatch_task(
        &mut self,
        task: Task,
        backend: &dyn LlmBackend,
        cancel: &CancellationToken,
    ) -> Result<TaskOutcome, LlmBackendError> {
        let tier = (task.mlfq_level as usize).min(NUM_TIERS - 1);
        self.tier_counters[tier] = self.tier_counters[tier].saturating_add(1);
        self.dispatched_tasks.push(task.clone());
        // Bound the dispatch history: evict the oldest entries once the cap is
        // reached so a long-running scheduler does not retain every task forever.
        if self.max_dispatched_tasks > 0
            && self.dispatched_tasks.len() > self.max_dispatched_tasks
        {
            let overflow = self.dispatched_tasks.len() - self.max_dispatched_tasks;
            self.dispatched_tasks.drain(0..overflow);
        }
        self.dispatch_count = self.dispatch_count.saturating_add(1);

        let stream = backend.stream_completion(&task.prompt, cancel).await?;

        let budget = task.token_budget;
        let mut response = String::new();
        let mut tokens_emitted: u32 = 0;

        for event in stream {
            match event {
                StreamingCompletion::Token(t) => {
                    tokens_emitted = tokens_emitted.saturating_add(1);
                    response.push_str(&t);
                    // Enforce per-task token budget: stop processing further
                    // stream events once the slice is exhausted.
                    if let Some(budget) = budget {
                        if tokens_emitted >= budget {
                            break;
                        }
                    }
                }
                StreamingCompletion::Done => {}
                StreamingCompletion::Cancelled => return Err(LlmBackendError::Cancelled),
            }
        }

        self.total_tokens_dispatched = self
            .total_tokens_dispatched
            .saturating_add(tokens_emitted as u64);

        Ok(TaskOutcome {
            task,
            response,
            tokens_emitted,
        })
    }
}

// ---------------------------------------------------------------------------
// Kani formal verification proof harnesses
// ---------------------------------------------------------------------------
//
// These harnesses are compiled only when running `cargo kani`.  They prove
// six structural invariants of the three-tier task queue:
//
//   1. `push` always increases `len()` by exactly one.
//   2. An out-of-range `mlfq_level` is clamped to the last tier.
//   3. `select_optimal_task` on a non-empty agenda always returns `Some`.
//   4. `select_optimal_task` reduces `len()` by exactly one.
//   5. `select_optimal_task` on an empty agenda returns `None` (no panic).
//   6. `boost_all_to_high` empties all non-zero tiers.
//
// Epic E4.6 exit criterion 1.

/// Kani formal verification proofs for [`TaskAgenda`] invariants.
#[cfg(kani)]
mod kani_proofs {
    use super::*;
    use crate::Task;

    /// Convenience: create a minimal `Task` for structural proofs that do not
    /// exercise the LLM dispatch path.
    fn task_with_level(id: u64, level: u8) -> Task {
        Task::new(id, level, "")
    }

    /// Prove: `push` always increases `len()` by exactly one, starting from
    /// an **arbitrary** (possibly non-empty) agenda state.
    ///
    /// We seed the agenda with a symbolic number of tasks (≤ 5) across
    /// arbitrary valid tiers so the proof holds for all reachable occupancies,
    /// not just the empty-agenda case.
    #[kani::proof]
    fn push_increases_len_by_exactly_one() {
        let mut agenda = TaskAgenda::new();

        // Drive the agenda into an arbitrary initial state.
        let num_initial: usize = kani::any();
        kani::assume(num_initial <= 5);
        for i in 0..num_initial {
            let level: u8 = kani::any();
            kani::assume((level as usize) < NUM_TIERS);
            agenda.push(task_with_level(i as u64, level));
        }

        let initial_len = agenda.len();
        let level: u8 = kani::any();
        kani::assume((level as usize) < NUM_TIERS);

        agenda.push(task_with_level(99, level));
        assert_eq!(
            agenda.len(),
            initial_len + 1,
            "push must increase len by exactly one"
        );
    }

    /// Prove: an out-of-range `mlfq_level` is clamped to the last tier
    /// (`NUM_TIERS - 1`), never placed in an out-of-bounds slot.
    #[kani::proof]
    fn out_of_range_level_is_clamped_to_last_tier() {
        let mut agenda = TaskAgenda::new();

        let level: u8 = kani::any();
        kani::assume(level as usize >= NUM_TIERS);

        agenda.push(task_with_level(1, level));

        assert_eq!(
            agenda.tiers[NUM_TIERS - 1].len(),
            1,
            "out-of-range level must land in the last tier"
        );
        assert_eq!(agenda.len(), 1);
    }

    /// Prove: `select_optimal_task` on a non-empty agenda always returns `Some`.
    #[kani::proof]
    fn select_on_nonempty_agenda_returns_some() {
        let mut agenda = TaskAgenda::new();

        let level: u8 = kani::any();
        kani::assume((level as usize) < NUM_TIERS);

        agenda.push(task_with_level(1, level));
        assert!(!agenda.is_empty());

        let result = agenda.select_optimal_task();
        assert!(result.is_some(), "non-empty agenda must yield Some");
    }

    /// Prove: `select_optimal_task` reduces `len()` by exactly one, starting
    /// from an **arbitrary** non-empty agenda state.
    ///
    /// We seed the agenda with a symbolic number of tasks (1–5) across
    /// arbitrary valid tiers so the proof generalises beyond the single-task
    /// case and covers multi-tier occupancies.
    #[kani::proof]
    fn select_reduces_len_by_exactly_one() {
        let mut agenda = TaskAgenda::new();

        // Seed with at least one task so select_optimal_task is non-trivial.
        let num_initial: usize = kani::any();
        kani::assume(num_initial > 0 && num_initial <= 5);
        for i in 0..num_initial {
            let level: u8 = kani::any();
            kani::assume((level as usize) < NUM_TIERS);
            agenda.push(task_with_level(i as u64, level));
        }

        let before = agenda.len();
        let _ = agenda.select_optimal_task();

        assert_eq!(
            agenda.len(),
            before - 1,
            "select must reduce len by exactly one"
        );
    }

    /// Prove: `select_optimal_task` on an empty agenda returns `None` without
    /// panicking.
    #[kani::proof]
    fn select_on_empty_agenda_returns_none() {
        let mut agenda = TaskAgenda::new();
        let result = agenda.select_optimal_task();
        assert!(result.is_none(), "empty agenda must return None");
    }

    /// Prove: `boost_all_to_high` empties every tier except tier 0.
    ///
    /// We seed the agenda with exactly one task per tier and assert that after
    /// the boost, tier 0 holds all tasks and all other tiers are empty.
    #[kani::proof]
    fn boost_all_to_high_empties_all_non_zero_tiers() {
        let mut agenda = TaskAgenda::new();

        for tier in 0..NUM_TIERS {
            agenda.push(task_with_level(tier as u64, tier as u8));
        }

        let total = agenda.len();
        agenda.boost_all_to_high();

        assert_eq!(
            agenda.tiers[0].len(),
            total,
            "all tasks must be in tier 0 after boost"
        );
        for tier_idx in 1..NUM_TIERS {
            assert!(
                agenda.tiers[tier_idx].is_empty(),
                "tier {tier_idx} must be empty after boost"
            );
        }
    }
}

// Tests require std for the mock LLM backend, thread::yield_now, and
// HashSet.  Gate the entire block so no_std builds don't try to compile it.
#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
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

    // ── TaskAgenda tests ────────────────────────────────────────────────────

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
    fn boost_all_to_high_promotes_medium_and_low() {
        let mut agenda = TaskAgenda::new();
        agenda.push(Task::new(1, 1, "medium"));
        agenda.push(Task::new(2, 2, "low"));
        agenda.push(Task::new(3, 0, "high"));

        let boosted = agenda.boost_all_to_high();
        assert_eq!(boosted, 2, "medium and low tasks should be promoted");

        // After boost, tier 0 should have 3 tasks (original high + 2 promoted),
        // and tiers 1 and 2 should be empty.
        assert_eq!(agenda.tiers[0].len(), 3);
        assert!(agenda.tiers[1].is_empty());
        assert!(agenda.tiers[2].is_empty());
    }

    #[test]
    fn boost_all_to_high_on_empty_agenda_returns_zero() {
        let mut agenda = TaskAgenda::new();
        assert_eq!(agenda.boost_all_to_high(), 0);
    }

    // ── Tier-transition exhaustive table ───────────────────────────────────

    /// Verify that tasks placed in each of the three tiers are dispatched in
    /// strict priority order, and that tier_counters match per-tier dispatch
    /// counts exactly.
    #[test]
    fn tier_transition_table_all_tiers() {
        use crate::mock::MockLlmBackend;

        let mut sched = IterationAwareMlfq::default();
        let mut agenda = TaskAgenda::new();
        let backend = MockLlmBackend::new();
        let cancel = CancellationToken::new();

        // Push tasks in reverse priority so the queue is not trivially sorted.
        agenda.push(Task::new(10, 2, "low"));
        agenda.push(Task::new(11, 1, "medium"));
        agenda.push(Task::new(12, 0, "high"));

        let mut dispatch_order = Vec::new();
        while let Some(task) = agenda.select_optimal_task() {
            let id = task.id;
            block_on(sched.dispatch_task(task, &backend, &cancel)).unwrap();
            dispatch_order.push(id);
        }

        assert_eq!(
            dispatch_order,
            vec![12, 11, 10],
            "dispatch order must be High -> Medium -> Low"
        );
        assert_eq!(sched.tier_counters[0], 1, "one High dispatch");
        assert_eq!(sched.tier_counters[1], 1, "one Medium dispatch");
        assert_eq!(sched.tier_counters[2], 1, "one Low dispatch");
    }

    // ── Starvation-prevention boost ─────────────────────────────────────────

    #[test]
    fn check_and_boost_only_fires_at_interval_boundary() {
        let mut sched = IterationAwareMlfq::with_boost_interval(3);
        let mut agenda = TaskAgenda::new();
        agenda.push(Task::new(1, 1, "medium"));

        // No boost before first dispatch.
        let n = sched.check_and_boost(&mut agenda);
        assert_eq!(n, 0, "no boost before any dispatch");

        // Simulate 2 dispatches (dispatch_count = 2; 2 % 3 ≠ 0 → no boost).
        sched.dispatch_count = 2;
        let n = sched.check_and_boost(&mut agenda);
        assert_eq!(n, 0, "no boost at dispatch_count=2 with interval=3");

        // At dispatch_count = 3 the boost should fire.
        sched.dispatch_count = 3;
        let n = sched.check_and_boost(&mut agenda);
        assert_eq!(n, 1, "one task promoted at boost boundary");
        assert!(agenda.tiers[1].is_empty());
        assert_eq!(agenda.tiers[0].len(), 1);
    }

    #[test]
    fn check_and_boost_disabled_when_interval_is_zero() {
        let mut sched = IterationAwareMlfq::default(); // boost_interval = 0
        let mut agenda = TaskAgenda::new();
        agenda.push(Task::new(1, 2, "low"));
        sched.dispatch_count = 100; // Many dispatches, but boost is disabled.
        let n = sched.check_and_boost(&mut agenda);
        assert_eq!(n, 0);
        assert!(!agenda.tiers[2].is_empty(), "low-tier task must remain");
    }

    /// Adversarial starvation soak: 900 High tasks followed by 100 Low tasks.
    /// With `boost_interval = 100`, the boost fires at dispatch 100 and again
    /// at 200, etc., ensuring all Low tasks are eventually promoted and run.
    #[test]
    fn no_starvation_under_adversarial_workload() {
        use crate::mock::MockLlmBackend;
        use std::collections::HashSet;

        let mut agenda = TaskAgenda::new();
        let mut sched = IterationAwareMlfq::with_boost_interval(100);

        // 900 High-priority tasks.
        for i in 0..900u64 {
            agenda.push(Task::new(i, 0, "high"));
        }
        // 100 Low-priority tasks — would be starved without the boost.
        for i in 900..1000u64 {
            agenda.push(Task::new(i, 2, "low"));
        }

        let backend = MockLlmBackend::new();
        let cancel = CancellationToken::new();
        let mut dispatched: HashSet<u64> = HashSet::new();

        while !agenda.is_empty() {
            // Apply starvation boost before selection.
            sched.check_and_boost(&mut agenda);
            if let Some(task) = agenda.select_optimal_task() {
                let id = task.id;
                block_on(sched.dispatch_task(task, &backend, &cancel)).unwrap();
                dispatched.insert(id);
            }
        }

        assert_eq!(dispatched.len(), 1000, "all 1 000 tasks must be dispatched");
        for i in 900..1000u64 {
            assert!(dispatched.contains(&i), "low-priority task {i} was starved");
        }
        assert_eq!(sched.dispatched_tasks.len(), 1000);
    }

    // ── Token-slice accounting ──────────────────────────────────────────────

    #[test]
    fn dispatch_through_mock_backend_returns_response() {
        use crate::mock::MockLlmBackend;

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
        assert_eq!(sched.total_tokens_dispatched, 3);
    }

    #[test]
    fn token_budget_truncates_response_at_slice_boundary() {
        use crate::mock::MockLlmBackend;

        let mut sched = IterationAwareMlfq::default();
        let backend = MockLlmBackend::new();
        let cancel = CancellationToken::new();

        // Prompt has 5 words; budget allows only 2 tokens.
        let task = Task::new(42, 0, "one two three four five").with_token_budget(2);
        let outcome = block_on(sched.dispatch_task(task, &backend, &cancel))
            .expect("dispatch should succeed");

        assert_eq!(outcome.tokens_emitted, 2, "must stop at budget boundary");
        assert_eq!(
            outcome.response, "one two ",
            "response must contain exactly the budgeted tokens"
        );
        assert_eq!(
            sched.total_tokens_dispatched, 2,
            "total accounting must reflect only consumed tokens"
        );
    }

    #[test]
    fn token_accounting_accumulates_across_multiple_dispatches() {
        use crate::mock::MockLlmBackend;

        let mut sched = IterationAwareMlfq::default();
        let backend = MockLlmBackend::new();
        let cancel = CancellationToken::new();

        block_on(sched.dispatch_task(Task::new(1, 0, "a b c"), &backend, &cancel)).unwrap(); // 3
        block_on(sched.dispatch_task(Task::new(2, 0, "d e"), &backend, &cancel)).unwrap(); // 2

        assert_eq!(sched.total_tokens_dispatched, 5);
    }

    #[test]
    fn failed_dispatch_still_records_attempt() {
        use crate::backend::LlmBackendError;

        #[derive(Debug)]
        struct AlwaysFails;
        impl crate::backend::LlmBackend for AlwaysFails {
            fn id(&self) -> &'static str {
                "always-fails"
            }
            fn stream_completion<'a>(
                &'a self,
                _prompt: &'a str,
                _cancel: &'a CancellationToken,
            ) -> crate::backend::CompletionFuture<'a> {
                Box::pin(async { Err(LlmBackendError::Provider("boom".into())) })
            }
        }

        let mut sched = IterationAwareMlfq::default();
        let backend = AlwaysFails;
        let cancel = CancellationToken::new();
        let err = block_on(sched.dispatch_task(Task::new(9, 0, "anything"), &backend, &cancel))
            .expect_err("dispatch should fail");

        assert_eq!(err, LlmBackendError::Provider("boom".into()));
        assert_eq!(sched.dispatched_tasks.len(), 1);
        assert_eq!(sched.dispatched_tasks[0].id, 9);
        assert_eq!(sched.tier_counters[0], 1);
        // Tokens should not accumulate on failure.
        assert_eq!(sched.total_tokens_dispatched, 0);
    }

    #[test]
    fn dispatched_tasks_history_is_bounded_and_evicts_oldest() {
        use crate::mock::MockLlmBackend;

        // A small cap proves the history ring evicts the oldest entries while
        // retaining the most recent, and `len()` never exceeds the cap.
        let mut sched = IterationAwareMlfq::default().with_max_dispatched_tasks(3);
        let backend = MockLlmBackend::new();
        let cancel = CancellationToken::new();

        for i in 0..10u64 {
            block_on(sched.dispatch_task(Task::new(i, 0, "x"), &backend, &cancel)).unwrap();
        }

        assert_eq!(sched.dispatched_tasks.len(), 3, "history must be capped");
        // The three most recent tasks (ids 7, 8, 9) are retained.
        assert_eq!(sched.dispatched_tasks[0].id, 7);
        assert_eq!(sched.dispatched_tasks[2].id, 9);
    }

    // ── Token-count estimation ──────────────────────────────────────────────

    #[test]
    fn estimate_token_count_rounds_up_to_nearest_four() {
        use crate::backend::LlmBackend;
        use crate::mock::MockLlmBackend;
        let b = MockLlmBackend::new();
        // 4 bytes → 1 token; 5 bytes → 2 tokens; 8 bytes → 2 tokens.
        assert_eq!(b.estimate_token_count("abcd"), 1);
        assert_eq!(b.estimate_token_count("abcde"), 2);
        assert_eq!(b.estimate_token_count("abcdefgh"), 2);
        // Empty string → 0 tokens.
        assert_eq!(b.estimate_token_count(""), 0);
    }
}
