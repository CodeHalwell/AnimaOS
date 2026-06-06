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
//! The [`skill_bridge`] module wires E11 (`skills`) skill/tool proposals into
//! the [`approval`] queue so operator-gated promotions actually flow through it.
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
pub mod skill_bridge;
pub mod snapshot;
pub mod twin;

// ── Public re-exports ─────────────────────────────────────────────────────────

pub use approval::{
    ApprovalQueue, DefenceVerdict, Proposal, ProposalKind, ProposalStatus, SandboxTestResult,
};
pub use skill_bridge::{
    content_fingerprint, skill_proposal_to_queue_proposal,
    skill_proposal_to_queue_proposal_with_hash, tool_proposal_to_queue_proposal,
    tool_proposal_to_queue_proposal_with_verdicts, BridgeError, SkillApprovalBridge,
};
