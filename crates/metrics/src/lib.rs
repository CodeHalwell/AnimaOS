//! E18 — Metrics & Observability
//!
//! Provides a structured metrics aggregation pipeline that folds a
//! [`vita::audit::AuditEntry`] slice into an [`AgentMetrics`] snapshot.  The
//! snapshot can be rendered as a human-readable text report, a JSON document,
//! or a Prometheus-compatible exposition string — all without any network I/O
//! or external dependencies.
//!
//! ## Design principles
//!
//! - **Pure fold**: [`aggregate`] is a deterministic function over `&[AuditEntry]`;
//!   it has no side-effects and is safe to call repeatedly or concurrently.
//! - **Zero new sensing**: all data is sourced from the existing audit log.
//! - **Multiple output formats**: callers choose JSON, Prometheus text, or a
//!   human-readable summary at render time, not at aggregation time.
//! - **Windowed**: callers may pass any sub-slice of the audit log to scope
//!   metrics to a time range or a session boundary.

#![forbid(unsafe_code)]

pub mod aggregator;
pub mod prometheus;
pub mod reporter;

pub use aggregator::{aggregate, AgentMetrics};
pub use prometheus::render_prometheus;
pub use reporter::render_text_report;
