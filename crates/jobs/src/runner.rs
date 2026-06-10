#![forbid(unsafe_code)]

//! Stateless job runner — E32 S32.4.
//!
//! The runner evaluates which active jobs are due and records execution
//! outcomes.  Actual dispatch (pushing a task onto the MLFQ agenda) is the
//! caller's responsibility; the runner only answers "which jobs should fire?"
//! and "how should I update the registry after a run?"

use crate::{
    job::{JobStatus, LastRun, ScheduledJob},
    registry::JobRegistry,
    schedule::JobSchedule,
};

// ── RunResult ─────────────────────────────────────────────────────────────────

/// Outcome of a single job execution attempt.
#[derive(Debug, Clone, PartialEq)]
pub struct RunResult {
    /// Identifier of the job that ran.
    pub job_id: String,
    /// `true` when the execution succeeded.
    pub success: bool,
    /// Wall-clock duration of the attempt in milliseconds.
    pub duration_ms: u64,
    /// Error description when `success` is `false`.
    pub error: Option<String>,
    /// 1-based attempt counter.
    pub attempt: u32,
}

impl RunResult {
    /// Convenience constructor for a successful result.
    pub fn success(job_id: impl Into<String>, duration_ms: u64, attempt: u32) -> Self {
        Self {
            job_id: job_id.into(),
            success: true,
            duration_ms,
            error: None,
            attempt,
        }
    }

    /// Convenience constructor for a failed result.
    pub fn failure(
        job_id: impl Into<String>,
        duration_ms: u64,
        attempt: u32,
        error: impl Into<String>,
    ) -> Self {
        Self {
            job_id: job_id.into(),
            success: false,
            duration_ms,
            error: Some(error.into()),
            attempt,
        }
    }
}

// ── due_job_ids ───────────────────────────────────────────────────────────────

/// Returns the `job_id` of every active job whose schedule is due at `now_ns`.
///
/// Inactive jobs (`Paused`, `Completed`, `Failed`) are excluded.
pub fn due_job_ids(registry: &JobRegistry, now_ns: u64) -> Vec<String> {
    let mut ids: Vec<String> = registry
        .iter()
        .filter(|(_, job)| job.is_active())
        .filter(|(_, job)| job_is_due(job, now_ns))
        .map(|(id, _)| id.to_owned())
        .collect();
    // Deterministic order for consistent audit entries.
    ids.sort();
    ids
}

/// Determines whether an active `job` is due to fire at `now_ns`.
///
/// For recurring (cron) jobs this delegates straight to
/// [`JobSchedule::is_due`].  For one-shot jobs ([`JobSchedule::Immediate`] /
/// [`JobSchedule::Once`]) it additionally handles the retry case: a one-shot
/// job that has only ever *failed* and still has retry budget remains due once
/// `retry_delay_secs` has elapsed since the last attempt, rather than being
/// permanently retired after its first failure.
fn job_is_due(job: &ScheduledJob, now_ns: u64) -> bool {
    match &job.last_run {
        // Never fired: defer entirely to the schedule.
        None => {
            let last_fired = 0;
            job.schedule.is_due(now_ns, last_fired)
        }
        // A prior successful run on a one-shot job is terminal (the status will
        // already be Completed, so this is mostly defensive).
        Some(last) if last.success && is_one_shot(&job.schedule) => false,
        // A prior *failed* run on a one-shot job: eligible to retry once the
        // configured delay has elapsed, provided retry budget remains. (Budget
        // exhaustion flips the status to Failed in `record_run_result`, so such
        // jobs are filtered out earlier by `is_active`.)
        Some(last) if !last.success && is_one_shot(&job.schedule) => {
            let delay_ns = job
                .retry_policy
                .retry_delay_secs
                .saturating_mul(1_000_000_000);
            now_ns >= last.fired_at_ns.saturating_add(delay_ns)
        }
        // Recurring jobs: defer to the schedule using the last firing time.
        Some(last) => job.schedule.is_due(now_ns, last.fired_at_ns),
    }
}

// ── record_run_result ─────────────────────────────────────────────────────────

/// Updates a job in the registry to reflect the outcome of an execution.
///
/// Transitions:
/// - Success on a one-shot job → [`JobStatus::Completed`] (terminal).
/// - Success on a recurring job → `consecutive_failures` reset to 0.
/// - Failure → `consecutive_failures` incremented; if it reaches
///   `retry_policy.max_attempts` → [`JobStatus::Failed`] (terminal). Otherwise
///   the job stays [`JobStatus::Active`] and remains retry-eligible. For
///   one-shot jobs this means a failure does *not* retire the job: it stays due
///   (after `retry_policy.retry_delay_secs`) until it either succeeds or
///   exhausts its retry budget. See [`due_job_ids`].
pub fn record_run_result(
    registry: &mut JobRegistry,
    job_id: &str,
    result: &RunResult,
    now_ns: u64,
) {
    let Some(job) = registry.get_mut(job_id) else {
        return;
    };

    job.last_run = Some(LastRun {
        fired_at_ns: now_ns,
        success: result.success,
        duration_ms: result.duration_ms,
        error: result.error.clone(),
        attempt: result.attempt,
    });

    if result.success {
        job.consecutive_failures = 0;
        if is_one_shot(&job.schedule) {
            job.status = JobStatus::Completed;
        }
    } else {
        job.consecutive_failures += 1;
        if job.consecutive_failures >= job.retry_policy.max_attempts {
            job.status = JobStatus::Failed;
        }
    }
}

fn is_one_shot(schedule: &JobSchedule) -> bool {
    matches!(schedule, JobSchedule::Immediate | JobSchedule::Once { .. })
}

// ── JobRunner ─────────────────────────────────────────────────────────────────

/// A stateless runner that polls a [`JobRegistry`] for due jobs.
///
/// Callers drive the dispatch loop: call [`JobRunner::poll`] to get the IDs of
/// due jobs, execute them externally, then call [`record_run_result`] for each
/// outcome to update the registry.
pub struct JobRunner {
    agent_id: String,
}

impl JobRunner {
    /// Creates a new runner for the given agent.
    pub fn new(agent_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
        }
    }

    /// Returns the agent identifier this runner is associated with.
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Returns the IDs of all active jobs that are due to fire at `now_ns`.
    pub fn poll(&self, registry: &JobRegistry, now_ns: u64) -> Vec<String> {
        due_job_ids(registry, now_ns)
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{job::ScheduledJob, schedule::JobSchedule};

    fn immediate_job(name: &str) -> ScheduledJob {
        ScheduledJob::new(name, "", "{}", JobSchedule::Immediate, 0)
    }

    fn once_job(name: &str, at_ns: u64) -> ScheduledJob {
        ScheduledJob::new(name, "", "{}", JobSchedule::Once { at_ns }, 0)
    }

    fn cron_job(name: &str, expr: &str) -> ScheduledJob {
        ScheduledJob::new(
            name,
            "",
            "{}",
            JobSchedule::Cron {
                expression: expr.to_owned(),
            },
            0,
        )
    }

    const NOW: u64 = 1_705_311_000_000_000_000; // 2024-01-15 09:30:00 UTC (minute=30, hour=9)

    #[test]
    fn due_job_ids_returns_immediate_job() {
        let mut reg = JobRegistry::in_memory();
        let job = immediate_job("fire-me");
        let id = job.job_id.clone();
        reg.add(job).unwrap();

        let due = due_job_ids(&reg, NOW);
        assert_eq!(due, vec![id]);
    }

    #[test]
    fn due_job_ids_skips_paused_job() {
        let mut reg = JobRegistry::in_memory();
        let mut job = immediate_job("paused");
        job.status = JobStatus::Paused;
        reg.add(job).unwrap();

        assert!(due_job_ids(&reg, NOW).is_empty());
    }

    #[test]
    fn due_job_ids_skips_completed_job() {
        let mut reg = JobRegistry::in_memory();
        let mut job = immediate_job("done");
        job.status = JobStatus::Completed;
        reg.add(job).unwrap();

        assert!(due_job_ids(&reg, NOW).is_empty());
    }

    #[test]
    fn due_job_ids_skips_failed_job() {
        let mut reg = JobRegistry::in_memory();
        let mut job = immediate_job("exhausted");
        job.status = JobStatus::Failed;
        reg.add(job).unwrap();

        assert!(due_job_ids(&reg, NOW).is_empty());
    }

    #[test]
    fn due_job_ids_skips_once_job_not_yet_due() {
        let mut reg = JobRegistry::in_memory();
        let future = NOW + 60_000_000_000; // 60 s in the future
        reg.add(once_job("future", future)).unwrap();

        assert!(due_job_ids(&reg, NOW).is_empty());
    }

    #[test]
    fn due_job_ids_includes_once_job_past_threshold() {
        let mut reg = JobRegistry::in_memory();
        let past = NOW - 1_000_000_000;
        let job = once_job("past", past);
        let id = job.job_id.clone();
        reg.add(job).unwrap();

        let due = due_job_ids(&reg, NOW);
        assert_eq!(due, vec![id]);
    }

    #[test]
    fn due_job_ids_includes_matching_cron_job() {
        let mut reg = JobRegistry::in_memory();
        // "30 9 * * *" matches 09:30
        let job = cron_job("morning", "30 9 * * *");
        let id = job.job_id.clone();
        reg.add(job).unwrap();

        let due = due_job_ids(&reg, NOW);
        assert_eq!(due, vec![id]);
    }

    #[test]
    fn due_job_ids_result_is_sorted() {
        let mut reg = JobRegistry::in_memory();
        // Both are immediate and never fired; IDs depend on make_job_id logic.
        reg.add(immediate_job("zzz")).unwrap();
        reg.add(immediate_job("aaa")).unwrap();

        let due = due_job_ids(&reg, NOW);
        let mut sorted = due.clone();
        sorted.sort();
        assert_eq!(due, sorted, "due_job_ids must return sorted IDs");
    }

    #[test]
    fn record_run_result_updates_last_run() {
        let mut reg = JobRegistry::in_memory();
        let job = immediate_job("update-test");
        let id = job.job_id.clone();
        reg.add(job).unwrap();

        let result = RunResult::success(&id, 42, 1);
        record_run_result(&mut reg, &id, &result, NOW);

        let last = reg.get(&id).unwrap().last_run.as_ref().unwrap();
        assert!(last.success);
        assert_eq!(last.duration_ms, 42);
        assert_eq!(last.attempt, 1);
    }

    #[test]
    fn record_run_result_completes_immediate_job_on_success() {
        let mut reg = JobRegistry::in_memory();
        let job = immediate_job("one-shot");
        let id = job.job_id.clone();
        reg.add(job).unwrap();

        let result = RunResult::success(&id, 10, 1);
        record_run_result(&mut reg, &id, &result, NOW);

        assert_eq!(reg.get(&id).unwrap().status, JobStatus::Completed);
    }

    #[test]
    fn record_run_result_completes_once_job_on_success() {
        let mut reg = JobRegistry::in_memory();
        let job = once_job("single", NOW - 1);
        let id = job.job_id.clone();
        reg.add(job).unwrap();

        let result = RunResult::success(&id, 5, 1);
        record_run_result(&mut reg, &id, &result, NOW);

        assert_eq!(reg.get(&id).unwrap().status, JobStatus::Completed);
    }

    #[test]
    fn record_run_result_does_not_complete_cron_job_on_success() {
        let mut reg = JobRegistry::in_memory();
        let job = cron_job("recurring", "* * * * *");
        let id = job.job_id.clone();
        reg.add(job).unwrap();

        let result = RunResult::success(&id, 1, 1);
        record_run_result(&mut reg, &id, &result, NOW);

        // Cron jobs stay Active after a successful run
        assert_eq!(reg.get(&id).unwrap().status, JobStatus::Active);
        assert_eq!(reg.get(&id).unwrap().consecutive_failures, 0);
    }

    #[test]
    fn record_run_result_increments_consecutive_failures() {
        let mut reg = JobRegistry::in_memory();
        let job = immediate_job("flaky");
        let id = job.job_id.clone();
        reg.add(job).unwrap();

        let result = RunResult::failure(&id, 1, 1, "timeout");
        record_run_result(&mut reg, &id, &result, NOW);

        assert_eq!(reg.get(&id).unwrap().consecutive_failures, 1);
        assert_eq!(reg.get(&id).unwrap().status, JobStatus::Active);
    }

    #[test]
    fn one_shot_job_reaches_failed_after_max_attempts_via_real_cycle() {
        // Drive a one-shot job through the real is_due → run → record cycle and
        // assert it reaches Failed exactly after `max_attempts` failures, rather
        // than becoming an un-retryable zombie after the first failure.
        let mut reg = JobRegistry::in_memory();
        let job = immediate_job("doomed");
        let id = job.job_id.clone();
        let max = job.retry_policy.max_attempts; // 3
        let delay_ns = job.retry_policy.retry_delay_secs * 1_000_000_000;
        reg.add(job).unwrap();

        let mut now = NOW;
        for expected_attempt in 1..=max {
            // The job must be due on each attempt while budget remains.
            let due = due_job_ids(&reg, now);
            assert_eq!(
                due,
                vec![id.clone()],
                "job should be due on attempt {expected_attempt}"
            );

            let result = RunResult::failure(&id, 1, expected_attempt, "boom");
            record_run_result(&mut reg, &id, &result, now);

            if expected_attempt < max {
                // Still has budget → Active, but not due until retry delay elapses.
                assert_eq!(reg.get(&id).unwrap().status, JobStatus::Active);
                assert!(
                    due_job_ids(&reg, now).is_empty(),
                    "must not be immediately re-due within retry delay"
                );
                // Advance past the retry delay for the next iteration.
                now += delay_ns + 1;
            }
        }

        // Budget exhausted → Failed and no longer due.
        assert_eq!(reg.get(&id).unwrap().status, JobStatus::Failed);
        assert_eq!(reg.get(&id).unwrap().consecutive_failures, max);
        assert!(due_job_ids(&reg, now + delay_ns * 10).is_empty());
    }

    #[test]
    fn one_shot_retry_eligibility_respects_retry_delay() {
        let mut reg = JobRegistry::in_memory();
        let job = immediate_job("retry-delay");
        let id = job.job_id.clone();
        let delay_ns = job.retry_policy.retry_delay_secs * 1_000_000_000;
        reg.add(job).unwrap();

        // First attempt fails.
        assert_eq!(due_job_ids(&reg, NOW), vec![id.clone()]);
        record_run_result(&mut reg, &id, &RunResult::failure(&id, 1, 1, "boom"), NOW);

        // Within the retry delay window: not due.
        assert!(due_job_ids(&reg, NOW).is_empty());
        assert!(due_job_ids(&reg, NOW + delay_ns - 1).is_empty());

        // At exactly the retry delay boundary: due again.
        assert_eq!(due_job_ids(&reg, NOW + delay_ns), vec![id.clone()]);
        assert_eq!(reg.get(&id).unwrap().status, JobStatus::Active);
    }

    #[test]
    fn one_shot_job_not_due_after_success() {
        let mut reg = JobRegistry::in_memory();
        let job = immediate_job("done-once");
        let id = job.job_id.clone();
        reg.add(job).unwrap();

        assert_eq!(due_job_ids(&reg, NOW), vec![id.clone()]);
        record_run_result(&mut reg, &id, &RunResult::success(&id, 1, 1), NOW);

        assert_eq!(reg.get(&id).unwrap().status, JobStatus::Completed);
        // Completed jobs are inactive and excluded.
        assert!(due_job_ids(&reg, NOW + 1_000_000_000_000).is_empty());
    }

    #[test]
    fn record_run_result_resets_failures_on_success() {
        let mut reg = JobRegistry::in_memory();
        // Use a cron job so it stays Active after success
        let job = cron_job("recover", "* * * * *");
        let id = job.job_id.clone();
        reg.add(job).unwrap();

        // Two failures
        for attempt in 1..=2 {
            let result = RunResult::failure(&id, 1, attempt, "err");
            record_run_result(&mut reg, &id, &result, NOW + attempt as u64);
        }
        assert_eq!(reg.get(&id).unwrap().consecutive_failures, 2);

        // One success resets the counter
        let success = RunResult::success(&id, 5, 3);
        record_run_result(&mut reg, &id, &success, NOW + 3);
        assert_eq!(reg.get(&id).unwrap().consecutive_failures, 0);
    }

    #[test]
    fn record_run_result_is_no_op_for_missing_job() {
        let mut reg = JobRegistry::in_memory();
        // Should not panic
        let result = RunResult::success("nonexistent-job", 1, 1);
        record_run_result(&mut reg, "nonexistent-job", &result, NOW);
        assert!(reg.is_empty());
    }

    #[test]
    fn job_runner_new_stores_agent_id() {
        let runner = JobRunner::new("agent-a");
        assert_eq!(runner.agent_id(), "agent-a");
    }

    #[test]
    fn job_runner_poll_delegates_to_due_job_ids() {
        let mut reg = JobRegistry::in_memory();
        let job = immediate_job("poll-test");
        let id = job.job_id.clone();
        reg.add(job).unwrap();

        let runner = JobRunner::new("agent-test");
        let due = runner.poll(&reg, NOW);
        assert_eq!(due, vec![id]);
    }
}
