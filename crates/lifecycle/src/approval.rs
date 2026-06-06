//! S15.2 — Approval-queue surface for E11 self-extension proposals.
//!
//! The approval queue is the operator-facing half of the E11 promotion gates.
//! When the agent proposes a new skill, tool, or weight update, the proposal
//! lands here and waits for an explicit operator sign-off before taking effect.
//!
//! ## Design
//!
//! Every proposal has a [`ProposalKind`] that carries the provenance data an
//! operator needs to make an informed decision:
//! - **NewSkill**: a prompt-only skill (zero executable surface, lower risk).
//! - **NewTool**: a WASM-sandboxed tool (requires operator approval always per
//!   the E11 spec).
//! - **WeightUpdate**: a fine-tuned adapter to be mounted on a local model.
//!
//! Proposals are stored in an [`ApprovalQueue`] (in-memory; callers persist via
//! the snapshot subsystem or by writing to the audit log).  The queue exposes:
//!
//! - [`ApprovalQueue::enqueue`]: add a pending proposal.
//! - [`ApprovalQueue::approve`] / [`ApprovalQueue::reject`]: operator decisions.
//! - [`ApprovalQueue::rollback`]: withdraw an approved proposal (before
//!   integration completes).
//! - [`ApprovalQueue::pending`]: list proposals awaiting a decision.
//!
//! All state transitions are recorded in the embedded [`ApprovalLog`] so the
//! full decision history is auditable even before the E15 audit-log bridge is
//! wired.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── ProposalKind ──────────────────────────────────────────────────────────────

/// The type and provenance of an agent self-extension proposal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ProposalKind {
    /// A prompt-only skill (no executable code; lower risk).
    NewSkill {
        /// Human-readable name for the proposed skill.
        name: String,
        /// Short description of what the skill does.
        description: String,
        /// SHA-256 hex digest of the prompt template bytes.
        prompt_hash: String,
    },
    /// A WASM-sandboxed tool (operator approval always required per E11).
    NewTool {
        /// Human-readable name for the proposed tool.
        name: String,
        /// Short description of the tool's function.
        description: String,
        /// SHA-256 hex digest of the compiled WASM module.
        wasm_hash: String,
        /// Capabilities the tool requests (e.g. `["allow_stdout"]`).
        requested_capabilities: Vec<String>,
    },
    /// A fine-tuned adapter to mount on a local model (E8).
    WeightUpdate {
        /// Identifier of the base model the adapter targets.
        model_id: String,
        /// SHA-256 hex digest of the adapter weights file.
        adapter_hash: String,
        /// Adapter rank / compression ratio, if known.
        rank: Option<u32>,
        /// Human-readable description of what the adapter was trained on.
        training_summary: String,
    },
}

impl ProposalKind {
    /// Short display label for the proposal kind.
    pub fn label(&self) -> &'static str {
        match self {
            ProposalKind::NewSkill { .. } => "new-skill",
            ProposalKind::NewTool { .. } => "new-tool",
            ProposalKind::WeightUpdate { .. } => "weight-update",
        }
    }
}

// ── ProposalStatus ────────────────────────────────────────────────────────────

/// Lifecycle state of an approval proposal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum ProposalStatus {
    /// Awaiting an operator decision.
    Pending,
    /// Approved by the operator; integration may proceed.
    Approved { approved_at_ns: u64, reason: String },
    /// Rejected by the operator; proposal is closed.
    Rejected { rejected_at_ns: u64, reason: String },
    /// Rolled back after approval (before integration completed).
    RolledBack {
        rolled_back_at_ns: u64,
        reason: String,
    },
}

// ── SandboxTestResult / DefenceVerdict ────────────────────────────────────────

/// Result of running the proposal through the Wasmtime sandbox (E2.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxTestResult {
    /// `true` when all sandbox tests passed.
    pub passed: bool,
    /// Total fuel consumed across all test invocations.
    pub fuel_consumed: u64,
    /// Maximum memory used in bytes.
    pub peak_memory_bytes: u64,
    /// Narrative description of the test run.
    pub summary: String,
}

/// Verdict from the defence layer screening (E5.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DefenceVerdict {
    /// `true` when the defence layer cleared the proposal.
    pub cleared: bool,
    /// Names of any detectors that flagged the proposal.
    pub flagged_by: Vec<String>,
    /// Human-readable description of any issues found.
    pub summary: String,
}

// ── Proposal ─────────────────────────────────────────────────────────────────

/// A single self-extension proposal awaiting operator review.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    /// Unique proposal identifier (opaque string; typically a UUID or
    /// `<kind>-<hash_prefix>`).
    pub id: String,
    /// Content and provenance of the proposal.
    pub kind: ProposalKind,
    /// Wall-clock creation time (nanoseconds since Unix epoch).
    pub created_at_ns: u64,
    /// Free-form note about where the proposal originated (e.g. `"dreaming
    /// phase cycle 42"`, `"operator suggestion"`).
    pub provenance: String,
    /// Sandbox test results, if the proposal was run through E2.5.
    pub sandbox_result: Option<SandboxTestResult>,
    /// Defence layer verdict, if the proposal was screened.
    pub defence_verdict: Option<DefenceVerdict>,
    /// Current lifecycle state.
    pub status: ProposalStatus,
}

impl Proposal {
    /// `true` when the proposal is in the [`ProposalStatus::Pending`] state.
    pub fn is_pending(&self) -> bool {
        matches!(self.status, ProposalStatus::Pending)
    }

    /// `true` when the proposal was approved and has not been rolled back.
    pub fn is_approved(&self) -> bool {
        matches!(self.status, ProposalStatus::Approved { .. })
    }
}

// ── ApprovalLogEntry ──────────────────────────────────────────────────────────

/// A single entry in the embedded approval audit log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalLogEntry {
    pub proposal_id: String,
    pub action: String,
    pub timestamp_ns: u64,
    pub reason: Option<String>,
}

// ── ApprovalQueue ─────────────────────────────────────────────────────────────

/// In-memory queue of self-extension proposals.
///
/// Callers are responsible for persistence (e.g. through the snapshot subsystem
/// in S15.5 or by serialising the queue to the audit log).
#[derive(Debug, Clone, Default)]
pub struct ApprovalQueue {
    proposals: HashMap<String, Proposal>,
    insertion_order: Vec<String>,
    log: Vec<ApprovalLogEntry>,
}

impl ApprovalQueue {
    /// Create an empty approval queue.
    pub fn new() -> Self {
        ApprovalQueue::default()
    }

    /// Add a pending proposal to the queue.
    ///
    /// Returns `false` (and leaves the queue unchanged) when a proposal with
    /// the same `id` already exists.
    pub fn enqueue(&mut self, proposal: Proposal) -> bool {
        if self.proposals.contains_key(&proposal.id) {
            return false;
        }
        self.log.push(ApprovalLogEntry {
            proposal_id: proposal.id.clone(),
            action: "enqueued".to_string(),
            timestamp_ns: proposal.created_at_ns,
            reason: None,
        });
        self.insertion_order.push(proposal.id.clone());
        self.proposals.insert(proposal.id.clone(), proposal);
        true
    }

    /// Approve a pending proposal.
    ///
    /// Returns `Err` when the proposal does not exist or is not in the
    /// `Pending` state.
    pub fn approve(&mut self, id: &str, reason: impl Into<String>) -> Result<(), String> {
        let reason = reason.into();
        let proposal = self
            .proposals
            .get_mut(id)
            .ok_or_else(|| format!("proposal '{}' not found", id))?;
        if !proposal.is_pending() {
            return Err(format!(
                "proposal '{}' is not pending (status: {:?})",
                id, proposal.status
            ));
        }
        let ts = now_ns();
        proposal.status = ProposalStatus::Approved {
            approved_at_ns: ts,
            reason: reason.clone(),
        };
        self.log.push(ApprovalLogEntry {
            proposal_id: id.to_string(),
            action: "approved".to_string(),
            timestamp_ns: ts,
            reason: Some(reason),
        });
        Ok(())
    }

    /// Reject a pending proposal.
    ///
    /// Returns `Err` when the proposal does not exist or is not in the
    /// `Pending` state.
    pub fn reject(&mut self, id: &str, reason: impl Into<String>) -> Result<(), String> {
        let reason = reason.into();
        let proposal = self
            .proposals
            .get_mut(id)
            .ok_or_else(|| format!("proposal '{}' not found", id))?;
        if !proposal.is_pending() {
            return Err(format!(
                "proposal '{}' is not pending (status: {:?})",
                id, proposal.status
            ));
        }
        let ts = now_ns();
        proposal.status = ProposalStatus::Rejected {
            rejected_at_ns: ts,
            reason: reason.clone(),
        };
        self.log.push(ApprovalLogEntry {
            proposal_id: id.to_string(),
            action: "rejected".to_string(),
            timestamp_ns: ts,
            reason: Some(reason),
        });
        Ok(())
    }

    /// Roll back an approved proposal (before integration completes).
    ///
    /// Returns `Err` when the proposal does not exist or is not in the
    /// `Approved` state.
    pub fn rollback(&mut self, id: &str, reason: impl Into<String>) -> Result<(), String> {
        let reason = reason.into();
        let proposal = self
            .proposals
            .get_mut(id)
            .ok_or_else(|| format!("proposal '{}' not found", id))?;
        if !proposal.is_approved() {
            return Err(format!(
                "proposal '{}' is not approved (status: {:?})",
                id, proposal.status
            ));
        }
        let ts = now_ns();
        proposal.status = ProposalStatus::RolledBack {
            rolled_back_at_ns: ts,
            reason: reason.clone(),
        };
        self.log.push(ApprovalLogEntry {
            proposal_id: id.to_string(),
            action: "rolled_back".to_string(),
            timestamp_ns: ts,
            reason: Some(reason),
        });
        Ok(())
    }

    /// Return all proposals in insertion order.
    pub fn all(&self) -> Vec<&Proposal> {
        self.insertion_order
            .iter()
            .filter_map(|id| self.proposals.get(id))
            .collect()
    }

    /// Return all pending proposals in insertion order.
    pub fn pending(&self) -> Vec<&Proposal> {
        self.all().into_iter().filter(|p| p.is_pending()).collect()
    }

    /// Return all approved proposals in insertion order.
    pub fn approved(&self) -> Vec<&Proposal> {
        self.all().into_iter().filter(|p| p.is_approved()).collect()
    }

    /// Look up a proposal by id.
    pub fn get(&self, id: &str) -> Option<&Proposal> {
        self.proposals.get(id)
    }

    /// The embedded approval audit log, in chronological order.
    pub fn log(&self) -> &[ApprovalLogEntry] {
        &self.log
    }

    /// Total number of proposals in the queue (all statuses).
    pub fn len(&self) -> usize {
        self.proposals.len()
    }

    /// `true` when the queue contains no proposals.
    pub fn is_empty(&self) -> bool {
        self.proposals.is_empty()
    }
}

fn now_ns() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn skill_proposal(id: &str) -> Proposal {
        Proposal {
            id: id.to_string(),
            kind: ProposalKind::NewSkill {
                name: "summarise-email".to_string(),
                description: "Summarises an email thread".to_string(),
                prompt_hash: "abc123".to_string(),
            },
            created_at_ns: 1_000_000_000,
            provenance: "dreaming phase".to_string(),
            sandbox_result: None,
            defence_verdict: None,
            status: ProposalStatus::Pending,
        }
    }

    fn tool_proposal(id: &str) -> Proposal {
        Proposal {
            id: id.to_string(),
            kind: ProposalKind::NewTool {
                name: "markdown-renderer".to_string(),
                description: "Renders Markdown to HTML".to_string(),
                wasm_hash: "deadbeef".to_string(),
                requested_capabilities: vec!["allow_stdout".to_string()],
            },
            created_at_ns: 2_000_000_000,
            provenance: "mastery drive".to_string(),
            sandbox_result: Some(SandboxTestResult {
                passed: true,
                fuel_consumed: 5000,
                peak_memory_bytes: 65536,
                summary: "all tests passed".to_string(),
            }),
            defence_verdict: Some(DefenceVerdict {
                cleared: true,
                flagged_by: vec![],
                summary: "no issues found".to_string(),
            }),
            status: ProposalStatus::Pending,
        }
    }

    #[test]
    fn new_queue_is_empty() {
        let q = ApprovalQueue::new();
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
        assert!(q.pending().is_empty());
    }

    #[test]
    fn enqueue_adds_pending_proposal() {
        let mut q = ApprovalQueue::new();
        let ok = q.enqueue(skill_proposal("p1"));
        assert!(ok);
        assert_eq!(q.len(), 1);
        assert_eq!(q.pending().len(), 1);
    }

    #[test]
    fn enqueue_duplicate_id_returns_false_and_does_not_change_queue() {
        let mut q = ApprovalQueue::new();
        q.enqueue(skill_proposal("p1"));
        let ok = q.enqueue(skill_proposal("p1"));
        assert!(!ok);
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn pending_returns_only_pending_proposals() {
        let mut q = ApprovalQueue::new();
        q.enqueue(skill_proposal("p1"));
        q.enqueue(skill_proposal("p2"));
        q.approve("p1", "looks good").unwrap();
        assert_eq!(q.pending().len(), 1);
        assert_eq!(q.pending()[0].id, "p2");
    }

    #[test]
    fn approve_transitions_pending_to_approved() {
        let mut q = ApprovalQueue::new();
        q.enqueue(skill_proposal("p1"));
        q.approve("p1", "trusted source").unwrap();
        assert!(q.get("p1").unwrap().is_approved());
        assert_eq!(q.approved().len(), 1);
    }

    #[test]
    fn approve_unknown_id_returns_err() {
        let mut q = ApprovalQueue::new();
        let result = q.approve("does-not-exist", "reason");
        assert!(result.is_err());
    }

    #[test]
    fn approve_non_pending_proposal_returns_err() {
        let mut q = ApprovalQueue::new();
        q.enqueue(skill_proposal("p1"));
        q.approve("p1", "first").unwrap();
        let result = q.approve("p1", "duplicate");
        assert!(result.is_err());
    }

    #[test]
    fn reject_transitions_pending_to_rejected() {
        let mut q = ApprovalQueue::new();
        q.enqueue(tool_proposal("t1"));
        q.reject("t1", "capability too broad").unwrap();
        let p = q.get("t1").unwrap();
        assert!(matches!(p.status, ProposalStatus::Rejected { .. }));
        assert_eq!(q.pending().len(), 0);
    }

    #[test]
    fn reject_unknown_id_returns_err() {
        let mut q = ApprovalQueue::new();
        assert!(q.reject("nope", "reason").is_err());
    }

    #[test]
    fn rollback_transitions_approved_to_rolled_back() {
        let mut q = ApprovalQueue::new();
        q.enqueue(skill_proposal("p1"));
        q.approve("p1", "ok").unwrap();
        q.rollback("p1", "integration failed").unwrap();
        let p = q.get("p1").unwrap();
        assert!(matches!(p.status, ProposalStatus::RolledBack { .. }));
    }

    #[test]
    fn rollback_non_approved_proposal_returns_err() {
        let mut q = ApprovalQueue::new();
        q.enqueue(skill_proposal("p1"));
        assert!(q.rollback("p1", "too early").is_err());
    }

    #[test]
    fn log_records_all_actions_in_order() {
        let mut q = ApprovalQueue::new();
        q.enqueue(skill_proposal("p1"));
        q.enqueue(skill_proposal("p2"));
        q.approve("p1", "ok").unwrap();
        q.reject("p2", "no").unwrap();

        let log = q.log();
        assert_eq!(log.len(), 4);
        assert_eq!(log[0].action, "enqueued");
        assert_eq!(log[1].action, "enqueued");
        assert_eq!(log[2].action, "approved");
        assert_eq!(log[3].action, "rejected");
    }

    #[test]
    fn all_preserves_insertion_order() {
        let mut q = ApprovalQueue::new();
        q.enqueue(skill_proposal("p1"));
        q.enqueue(tool_proposal("p2"));
        q.enqueue(skill_proposal("p3"));
        let all = q.all();
        assert_eq!(all[0].id, "p1");
        assert_eq!(all[1].id, "p2");
        assert_eq!(all[2].id, "p3");
    }

    #[test]
    fn proposal_kind_labels_are_correct() {
        assert_eq!(
            ProposalKind::NewSkill {
                name: "".into(),
                description: "".into(),
                prompt_hash: "".into()
            }
            .label(),
            "new-skill"
        );
        assert_eq!(
            ProposalKind::NewTool {
                name: "".into(),
                description: "".into(),
                wasm_hash: "".into(),
                requested_capabilities: vec![]
            }
            .label(),
            "new-tool"
        );
        assert_eq!(
            ProposalKind::WeightUpdate {
                model_id: "".into(),
                adapter_hash: "".into(),
                rank: None,
                training_summary: "".into()
            }
            .label(),
            "weight-update"
        );
    }

    #[test]
    fn proposal_with_sandbox_and_defence_results_is_stored() {
        let mut q = ApprovalQueue::new();
        q.enqueue(tool_proposal("t1"));
        let p = q.get("t1").unwrap();
        assert!(p.sandbox_result.as_ref().unwrap().passed);
        assert!(p.defence_verdict.as_ref().unwrap().cleared);
    }

    #[test]
    fn approved_queue_length_is_correct_after_mixed_decisions() {
        let mut q = ApprovalQueue::new();
        for i in 0..5 {
            q.enqueue(skill_proposal(&format!("p{}", i)));
        }
        q.approve("p0", "ok").unwrap();
        q.approve("p1", "ok").unwrap();
        q.reject("p2", "no").unwrap();

        assert_eq!(q.approved().len(), 2);
        assert_eq!(q.pending().len(), 2); // p3, p4
        assert_eq!(q.len(), 5); // all five still in the map
    }
}
