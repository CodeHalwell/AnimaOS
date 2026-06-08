//! E28 — Alert Rules & Threshold Monitoring
//!
//! Defines threshold-based alert rules over the [`AgentMetrics`] produced by
//! the E18 metrics crate.  Rules are evaluated deterministically — no network
//! I/O, no side-effects — so the evaluator is safe to call in tests and in
//! production audit passes alike.
//!
//! ## Design
//!
//! - **`AlertRule`** — a named condition: `<metric_field> <op> <threshold>`.
//! - **`AlertRuleRegistry`** — atomic JSON-persisted store of active rules.
//! - **`AlertEvaluator`** — pure function: `evaluate(&AgentMetrics, &[AlertRule])
//!   → Vec<AlertEvent>`.
//! - **`AlertStateTracker`** — per-rule `Normal → Firing → Resolved` state
//!   machine that suppresses duplicate `Firing` events and generates `Resolved`
//!   events when a rule clears.
//!
//! ## Severity levels
//!
//! `Info < Warning < Critical`  — stored on each rule; propagated to events.

#![forbid(unsafe_code)]

pub mod evaluator;
pub mod registry;
pub mod rule;
pub mod state;

pub use evaluator::{evaluate, AlertEvent, AlertEventKind};
pub use registry::{AlertRuleRegistry, RegistryError};
pub use rule::{AlertCondition, AlertRule, AlertSeverity, ComparisonOp, MetricField};
pub use state::{AlertState, AlertStateTracker};
