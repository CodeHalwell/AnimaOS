//! # analytics — E25 Performance Analytics and Spend Reporting
//!
//! A pure, side-effect-free analytics crate that folds a `&[AuditEntry]`
//! slice into structured reports covering:
//!
//! - **Token usage** (`TokenReport`) — total spend, per-task statistics,
//!   and breakdown by MLFQ dispatch tier.
//! - **Cortex latency** (`LatencyReport`) — time-to-first-action
//!   percentiles (p50/p95/p99) and cortex reliability metrics (fault rate,
//!   mean tool calls per completion).
//! - **Gate analytics** (`GateReport`) — invocation rate, cost-class
//!   distribution, router modulation frequency, and gate efficiency.
//! - **Agent health** (`HealthReport`) — a composite score (0–1) with a
//!   letter grade (A–F) and actionable recommendations when a factor falls
//!   below its healthy operating range.
//! - **Summary** (`SummaryReport`) — all four sub-reports in one struct.
//!
//! The top-level entry point is [`AnalyticsEngine`], which exposes one
//! method per report type plus a [`AnalyticsEngine::summary_report`]
//! convenience method.
//!
//! ## Design principles
//!
//! - **No I/O** — every function is a pure fold; callers supply the entry
//!   slice (from an in-memory `AuditLog`, a deserialized JSONL file, or a
//!   test fixture).
//! - **No unsafe code** — `#![forbid(unsafe_code)]` is declared at the
//!   crate root.
//! - **No new audit entry variants** — E25 does not require changes to
//!   `vita::audit::AuditEntry`; it works with the entries already written by
//!   Stages 1–8.

#![forbid(unsafe_code)]

pub mod engine;
pub mod gate;
pub mod health;
pub mod latency;
pub mod token;

pub use engine::AnalyticsEngine;
pub use gate::GateReport;
pub use health::{HealthGrade, HealthReport};
pub use latency::LatencyReport;
pub use token::TokenReport;

use serde::{Deserialize, Serialize};

// ── SummaryReport ─────────────────────────────────────────────────────────────

/// Complete analytics summary combining all four sub-reports.
///
/// Produced by [`AnalyticsEngine::summary_report`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryReport {
    /// Agent identifier (caller-supplied; not extracted from entries).
    pub agent_id: String,
    /// Number of `AuditEntry` values processed.
    pub entries_analyzed: usize,
    /// Token usage report.
    pub token: TokenReport,
    /// Cortex latency and reliability report.
    pub latency: LatencyReport,
    /// Gate and routing analytics report.
    pub gate: GateReport,
    /// Overall agent health assessment.
    pub health: HealthReport,
}
