#![forbid(unsafe_code)]

//! Core job types — E32 S32.1.
//!
//! [`ScheduledJob`] is the central record persisted in the [`JobRegistry`].
//! It carries the schedule, payload, retry policy, and execution history for a
//! single recurring or one-shot task.

use serde::{Deserialize, Serialize};

use crate::schedule::JobSchedule;

// ── JobStatus ─────────────────────────────────────────────────────────────────

/// Lifecycle status of a [`ScheduledJob`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// The job is active and eligible to fire when its schedule is due.
    Active,
    /// The operator suspended the job; it will not fire until reactivated.
    Paused,
    /// A one-shot job that fired successfully; it will not fire again.
    Completed,
    /// The job exceeded its retry budget after consecutive failures.
    Failed,
}

impl std::fmt::Display for JobStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => f.write_str("active"),
            Self::Paused => f.write_str("paused"),
            Self::Completed => f.write_str("completed"),
            Self::Failed => f.write_str("failed"),
        }
    }
}

impl std::str::FromStr for JobStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "active" => Ok(Self::Active),
            "paused" => Ok(Self::Paused),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            other => Err(format!("unknown job status: {other}")),
        }
    }
}

// ── RetryPolicy ───────────────────────────────────────────────────────────────

/// Governs retry behaviour when a job execution fails.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Maximum number of consecutive failures before the job is marked
    /// [`JobStatus::Failed`] and stops being retried.
    pub max_attempts: u32,
    /// Minimum delay in seconds between retry attempts.
    ///
    /// The runner should respect this but is not required to implement
    /// back-off beyond enforcing the minimum delay at the call site.
    pub retry_delay_secs: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            retry_delay_secs: 60,
        }
    }
}

// ── LastRun ───────────────────────────────────────────────────────────────────

/// Outcome record for the most recent execution attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LastRun {
    /// Unix nanosecond timestamp at which this attempt started.
    pub fired_at_ns: u64,
    /// `true` when the execution succeeded.
    pub success: bool,
    /// Wall-clock duration of the attempt in milliseconds.
    pub duration_ms: u64,
    /// Error description when `success` is `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 1-based attempt counter (resets to 1 on a successful run).
    pub attempt: u32,
}

// ── ScheduledJob ──────────────────────────────────────────────────────────────

/// A scheduled task persisted in the [`JobRegistry`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduledJob {
    /// Stable opaque identifier produced by [`make_job_id`].
    pub job_id: String,
    /// Human-readable description of the task.
    pub description: String,
    /// Workspace this job belongs to; empty string means global (no workspace).
    pub workspace_id: String,
    /// Opaque payload forwarded to the task executor when the job fires.
    pub payload: String,
    /// Schedule controlling when this job fires.
    pub schedule: JobSchedule,
    /// Current lifecycle status.
    pub status: JobStatus,
    /// Retry configuration.
    pub retry_policy: RetryPolicy,
    /// Unix nanosecond timestamp at which the job was registered.
    pub created_at_ns: u64,
    /// Number of consecutive failed attempts without an intervening success.
    pub consecutive_failures: u32,
    /// Outcome of the most recent execution attempt, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<LastRun>,
}

impl ScheduledJob {
    /// Creates a new [`JobStatus::Active`] job with the default [`RetryPolicy`].
    pub fn new(
        description: impl Into<String>,
        workspace_id: impl Into<String>,
        payload: impl Into<String>,
        schedule: JobSchedule,
        now_ns: u64,
    ) -> Self {
        let description = description.into();
        let job_id = make_job_id(&description, now_ns);
        Self {
            job_id,
            description,
            workspace_id: workspace_id.into(),
            payload: payload.into(),
            schedule,
            status: JobStatus::Active,
            retry_policy: RetryPolicy::default(),
            created_at_ns: now_ns,
            consecutive_failures: 0,
            last_run: None,
        }
    }

    /// Returns `true` when the job is eligible for evaluation by the runner.
    pub fn is_active(&self) -> bool {
        self.status == JobStatus::Active
    }
}

// ── make_job_id ───────────────────────────────────────────────────────────────

/// Produces a stable, slug-like job identifier from the description and a
/// nanosecond creation timestamp.
///
/// The identifier has the form `job-<slug>-<hex>` where `<slug>` is the first
/// 24 characters of the lowercased description with non-alphanumeric characters
/// replaced by `-`, and `<hex>` is a hex suffix that mixes the full millisecond
/// timestamp with a process-local monotonic counter so that jobs created with
/// the same description in the same millisecond do not collide.
pub fn make_job_id(description: &str, now_ns: u64) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let slug: String = description
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(24)
        .collect();
    let slug = slug.trim_end_matches('-').to_owned();

    // Use the full millisecond timestamp (48 bits is plenty until well past the
    // year 10000) and mix in a 16-bit process-local counter so that identical
    // descriptions created in the same millisecond still get distinct IDs.
    let millis = now_ns / 1_000_000;
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed) & 0xFFFF;
    let suffix = (millis << 16) | counter;
    format!("job-{slug}-{suffix:x}")
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_job_id_produces_slug_with_hex_suffix() {
        let id = make_job_id("Send daily report", 1_705_312_200_000_000_000);
        assert!(id.starts_with("job-send-daily-report"), "id={id}");
        assert!(id.contains('-'));
    }

    #[test]
    fn make_job_id_different_for_different_timestamps() {
        let id1 = make_job_id("task", 1_000_000_000_000_000);
        let id2 = make_job_id("task", 2_000_000_000_000_000);
        assert_ne!(id1, id2);
    }

    #[test]
    fn make_job_id_handles_special_characters() {
        let id = make_job_id("Hello, World!", 0);
        assert!(id.starts_with("job-"), "id={id}");
        // no commas or exclamation marks in output
        assert!(!id.contains(','));
        assert!(!id.contains('!'));
    }

    #[test]
    fn job_status_display() {
        assert_eq!(JobStatus::Active.to_string(), "active");
        assert_eq!(JobStatus::Paused.to_string(), "paused");
        assert_eq!(JobStatus::Completed.to_string(), "completed");
        assert_eq!(JobStatus::Failed.to_string(), "failed");
    }

    #[test]
    fn job_status_from_str_round_trips() {
        for label in &["active", "paused", "completed", "failed"] {
            let s: JobStatus = label.parse().expect(label);
            assert_eq!(s.to_string(), *label);
        }
    }

    #[test]
    fn job_status_from_str_rejects_unknown() {
        assert!("unknown".parse::<JobStatus>().is_err());
    }

    #[test]
    fn new_job_is_active() {
        let job = ScheduledJob::new(
            "backup",
            "",
            "{}",
            crate::schedule::JobSchedule::Immediate,
            0,
        );
        assert_eq!(job.status, JobStatus::Active);
        assert!(job.is_active());
        assert!(job.last_run.is_none());
        assert_eq!(job.consecutive_failures, 0);
    }

    #[test]
    fn new_job_has_default_retry_policy() {
        let job = ScheduledJob::new("x", "", "", crate::schedule::JobSchedule::Immediate, 0);
        assert_eq!(job.retry_policy.max_attempts, 3);
        assert_eq!(job.retry_policy.retry_delay_secs, 60);
    }

    #[test]
    fn paused_job_is_not_active() {
        let mut job = ScheduledJob::new("x", "", "", crate::schedule::JobSchedule::Immediate, 0);
        job.status = JobStatus::Paused;
        assert!(!job.is_active());
    }

    #[test]
    fn job_serialises_to_json_and_back() {
        let job = ScheduledJob::new(
            "round-trip test",
            "ws-1",
            r#"{"key":"value"}"#,
            crate::schedule::JobSchedule::Cron {
                expression: "0 * * * *".to_owned(),
            },
            42_000_000_000,
        );
        let json = serde_json::to_string(&job).expect("serialise");
        let restored: ScheduledJob = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(job, restored);
    }
}
