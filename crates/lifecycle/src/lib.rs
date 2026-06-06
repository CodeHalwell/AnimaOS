//! E15 — Operator Trust & Agent Lifecycle
//!
//! Provides the operational layer for *living with* a long-running autonomous
//! agent.  Operators can:
//!
//! - **Trust** it: see what it did via [`digest`] and [`replay`].
//! - **Debug** it: step through past decisions with [`replay::DecisionReplayer`].
//! - **Gate** it: inspect pending self-extension proposals via [`approval`].
//! - **Sandbox** it: run a shadow agent against recorded scenarios via [`twin`].
//! - **Keep** it: snapshot and migrate the agent state with [`snapshot`].
//!
//! ## Stories delivered
//!
//! | Story | Module | Description |
//! |-------|--------|-------------|
//! | S15.1 | [`digest`]   | "While you were away" activity digest from the audit log |
//! | S15.2 | [`approval`] | Approval-queue surface for E11 self-extension proposals |
//! | S15.3 | [`replay`]   | Decision replay / time-travel debugging |
//! | S15.4 | [`twin`]     | Digital-twin sandbox for safe staging of changes |
//! | S15.5 | [`snapshot`] | State versioning & migration across AnimaOS upgrades |
//!
//! ## Architecture
//!
//! All modules consume [`vita::audit::AuditEntry`] slices — the existing durable
//! audit log is the spine.  No new instrumentation is required for S15.1 or
//! S15.3; S15.5 adds a thin snapshot schema on top of existing persistent stores.

#![forbid(unsafe_code)]

pub mod approval;
pub mod digest;
pub mod replay;
pub mod snapshot;
pub mod twin;
