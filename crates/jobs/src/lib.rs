#![forbid(unsafe_code)]

//! Scheduled job and cron engine — Epic E32.
//!
//! # Scope
//!
//! AnimaOS operators and agents can register one-shot or recurring tasks that
//! the hosted kernel dispatches via the existing MLFQ scheduler.  This crate
//! provides the data model, schedule evaluation, persistence, and runner
//! plumbing; the hosted kernel wires the runner into the somatic loop.
//!
//! # Architecture
//!
//! ```text
//!  Operator / agent
//!      │
//!      ▼  JobRegistry::add(ScheduledJob)
//!  JobRegistry  ──── flush() ──── jobs.json (atomic write)
//!      │
//!      ▼  JobRunner::poll(registry, now_ns)
//!  Vec<job_id>  (due jobs)
//!      │
//!      ▼  dispatch payload → vita MLFQ (caller's responsibility)
//!  record_run_result(registry, job_id, RunResult, now_ns)
//! ```
//!
//! # Modules
//!
//! | Module | Contents |
//! |---|---|
//! | [`job`] | [`job::ScheduledJob`], [`job::JobStatus`], [`job::RetryPolicy`], [`job::LastRun`], [`job::make_job_id`] |
//! | [`schedule`] | [`schedule::JobSchedule`], [`schedule::is_cron_due`], [`schedule::validate_cron`] |
//! | [`registry`] | [`registry::JobRegistry`], [`registry::JobRegistryError`] |
//! | [`runner`] | [`runner::JobRunner`], [`runner::RunResult`], [`runner::due_job_ids`], [`runner::record_run_result`] |
//! | [`finetune_trigger`] | [`finetune_trigger::FineTuneTrigger`], [`finetune_trigger::FineTuneProposalPayload`] — corpus-growth fine-tune proposal (E32↔E8) |

pub mod finetune_trigger;
pub mod job;
pub mod registry;
pub mod runner;
pub mod schedule;

// Re-export the most commonly used types.
pub use finetune_trigger::{FineTuneProposalPayload, FineTuneTrigger};
pub use job::{make_job_id, JobStatus, LastRun, RetryPolicy, ScheduledJob};
pub use registry::{JobRegistry, JobRegistryError};
pub use runner::{due_job_ids, record_run_result, JobRunner, RunResult};
pub use schedule::{validate_cron, JobSchedule};
