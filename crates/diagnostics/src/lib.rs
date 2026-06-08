//! E30 — Agent Self-Diagnostic System
//!
//! Provides a comprehensive health-check framework that aggregates observable
//! state from all AnimaOS subsystems into actionable [`DiagnosticReport`]s.
//!
//! ## Design
//!
//! All checks are stateless and operate on an [`AuditSnapshot`] derived from
//! the existing durable audit log — no new instrumentation is required.
//! This keeps the diagnostic system fully testable with synthetic data and
//! ensures it doesn't affect the hot execution path.
//!
//! ## Stories delivered
//!
//! | Story | Module | Description |
//! |-------|--------|-------------|
//! | S30.1 | [`check`]    | `DiagnosticCheck` trait, `CheckResult`, `HealthStatus` |
//! | S30.2 | [`snapshot`] | `AuditSnapshot` — point-in-time view derived from audit log |
//! | S30.3 | [`checks`]   | 11 built-in checks for all major subsystems |
//! | S30.4 | [`report`]   | `DiagnosticReport` aggregator with text rendering |
//! | S30.5 | (in `kernels/hosted`) | `anima diagnose` CLI + audit integration |
//!
//! ## Usage
//!
//! ```rust,no_run
//! use diagnostics::{AuditSnapshot, DiagnosticReport, checks::all_checks};
//! use vita::audit::AuditLog;
//!
//! let log = AuditLog::new();
//! let snapshot = AuditSnapshot::from_audit_log(log.entries());
//! let report = DiagnosticReport::run(&snapshot, &all_checks());
//! println!("{}", report.render_text());
//! ```

#![forbid(unsafe_code)]

pub mod check;
pub mod checks;
pub mod report;
pub mod snapshot;

// ── Public re-exports ─────────────────────────────────────────────────────────

pub use check::{CheckResult, DiagnosticCheck, HealthStatus};
pub use report::DiagnosticReport;
pub use snapshot::AuditSnapshot;
