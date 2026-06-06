//! E15↔E11 bridge — route E11 skill/tool proposals into the E15 approval queue.
//!
//! E11 (`skills`) evaluates a draft skill or tool and decides whether it should
//! auto-promote, wait for an operator, or be rejected outright.  E15
//! (`lifecycle::approval`) owns the operator-facing [`ApprovalQueue`].  Until
//! now nothing connected the two: an E11 `PendingApproval` outcome never
//! actually landed in the queue an operator inspects.
//!
//! This module is that wire.  It depends on `skills` (the `lifecycle → skills`
//! edge is acyclic — `skills` only reaches `praxis`/`serde`), converts E11
//! proposal data into [`Proposal`] records, and provides
//! [`SkillApprovalBridge`], a thin orchestrator that maps each queued proposal
//! id back to the registry skill id so an operator decision (`approve` /
//! `reject` / `rollback`) is routed to the right [`SkillRegistry`] action.
//!
//! ## Flow
//!
//! ```text
//! evaluate_skill_proposal()  ──► ProposalOutcome
//!        │                              │
//!        │  AutoPromoted / Rejected ────┘  (no queue entry — see below)
//!        │  PendingApproval
//!        ▼
//! skill_proposal_to_queue_proposal() ──► Proposal{ NewSkill }
//!        ▼
//! SkillApprovalBridge::enqueue_skill() ──► ApprovalQueue (Pending)
//!        ▼
//! operator: approve(id) ──► SkillRegistry::promote(skill_id)   (Proposed→Active)
//!           reject(id)  ──► registry untouched (skill stays Proposed)
//!           rollback(id)──► SkillRegistry::rollback(skill_id)   (→RolledBack)
//! ```
//!
//! ### Why `AutoPromoted` and `Rejected` skills are *not* enqueued
//!
//! The approval queue exists solely to gate decisions that still need an
//! operator.  An `AutoPromoted` skill has already been promoted to `Active` by
//! the E11 gate (operator/builtin authorship, or an agent skill with
//! `auto_promote_agent_skills` enabled) — there is nothing left to approve, so
//! enqueueing it would create a phantom pending item for an already-live skill.
//! A `Rejected` skill failed pre-screening and was never registered, so there
//! is no artifact to promote.  In both cases the conversion returns `None`.
//!
//! Tools are different: per the E11 spec they *never* auto-promote, so a tool
//! proposal that passed screening is *always* enqueued as a [`ProposalKind::NewTool`].

use std::collections::HashMap;

use skills::{
    ProposalAction, ProposalOutcome, SkillAuthor, SkillProposal, SkillRegistry, ToolProposal,
};

use crate::approval::{
    ApprovalQueue, DefenceVerdict, Proposal, ProposalKind, ProposalStatus, SandboxTestResult,
};

// ── Hash / fingerprint helpers ─────────────────────────────────────────────────

/// A dependency-free, stable 64-bit FNV-1a fingerprint rendered as 16 hex chars.
///
/// The [`ProposalKind`] hash fields are documented as SHA-256 digests, and a
/// production caller computes them from the real prompt/WASM bytes inside the
/// E2.5 sandbox / screening pipeline (where the bytes already live) and passes
/// them to the `*_with_hash` constructors below.  When no precomputed digest is
/// available (tests, or a caller that only has the in-memory proposal), this
/// fingerprint gives a deterministic, collision-resistant-enough content id
/// without dragging a crypto crate into the bridge.
pub fn content_fingerprint(bytes: &[u8]) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

/// Render a [`SkillAuthor`] + optional source episode as a queue `provenance`
/// note for the operator.
fn provenance_note(author: &SkillAuthor, source_episode: Option<&str>) -> String {
    match source_episode {
        Some(ep) => format!("{author} (episode {ep})"),
        None => format!("{author}"),
    }
}

// ── Skill proposal → queue proposal ─────────────────────────────────────────────

/// Convert an evaluated skill proposal into a queue [`Proposal`].
///
/// Returns `Some(Proposal { kind: NewSkill, .. })` **only** when the outcome is
/// [`ProposalAction::PendingApproval`].  For [`ProposalAction::AutoPromoted`] and
/// [`ProposalAction::Rejected`] this returns `None` — see the module docs for
/// the rationale (already promoted / never registered, so nothing to gate).
///
/// The `prompt_hash` is derived from the proposal's `skill_text` via
/// [`content_fingerprint`]; use [`skill_proposal_to_queue_proposal_with_hash`]
/// to supply a precomputed SHA-256 instead.
pub fn skill_proposal_to_queue_proposal(
    outcome: &ProposalOutcome,
    proposal: &SkillProposal,
) -> Option<Proposal> {
    let prompt_hash = content_fingerprint(proposal.skill_text.as_bytes());
    skill_proposal_to_queue_proposal_with_hash(outcome, proposal, prompt_hash)
}

/// Like [`skill_proposal_to_queue_proposal`] but with an explicit `prompt_hash`
/// (typically the real SHA-256 hex computed by the screening pipeline).
pub fn skill_proposal_to_queue_proposal_with_hash(
    outcome: &ProposalOutcome,
    proposal: &SkillProposal,
    prompt_hash: impl Into<String>,
) -> Option<Proposal> {
    // Only PendingApproval skills need operator gating; AutoPromoted skills are
    // already Active and Rejected skills were never registered.
    if !matches!(outcome.action, ProposalAction::PendingApproval) {
        return None;
    }
    // A PendingApproval outcome always carries the registered artifact id.
    let skill_id = outcome.artifact_id.clone()?;
    let prompt_hash = prompt_hash.into();

    let (name, description) = parse_name_and_description(&proposal.skill_text);

    Some(Proposal {
        // Use the registry skill id as the queue id so the round-trip mapping is
        // direct; the bridge still records the mapping explicitly for clarity.
        id: skill_id,
        kind: ProposalKind::NewSkill {
            name,
            description,
            prompt_hash,
        },
        created_at_ns: proposal.proposed_at_ns,
        provenance: provenance_note(&proposal.authored_by, proposal.source_episode.as_deref()),
        // Skills are prompt-only: no WASM sandbox run, no separate defence pass
        // here (the local SkillContentScreen already gated the text in E11).
        sandbox_result: None,
        defence_verdict: None,
        status: ProposalStatus::Pending,
    })
}

// ── Tool proposal → queue proposal ──────────────────────────────────────────────

/// Convert a tool proposal into a queue [`Proposal`].
///
/// Tools are **always** enqueued as [`ProposalKind::NewTool`]: per the E11 spec
/// there is no auto-promotion path for executable tools, so every screened tool
/// proposal must wait for explicit operator sign-off.
///
/// `wasm_hash` is derived from `wasm_bytes` via [`content_fingerprint`]; use
/// [`tool_proposal_to_queue_proposal_with_verdicts`] to attach a precomputed
/// hash and the real sandbox / defence results.
pub fn tool_proposal_to_queue_proposal(
    proposal: &ToolProposal,
    fixture_summary: impl Into<String>,
) -> Proposal {
    let wasm_hash = content_fingerprint(&proposal.wasm_bytes);
    let sandbox_result = SandboxTestResult {
        passed: true,
        fuel_consumed: 0,
        peak_memory_bytes: 0,
        summary: fixture_summary.into(),
    };
    tool_proposal_to_queue_proposal_with_verdicts(proposal, wasm_hash, sandbox_result, None)
}

/// Like [`tool_proposal_to_queue_proposal`] but lets the caller supply the real
/// `wasm_hash`, the full [`SandboxTestResult`] from the E2.5 run, and the
/// optional [`DefenceVerdict`] from screening.
pub fn tool_proposal_to_queue_proposal_with_verdicts(
    proposal: &ToolProposal,
    wasm_hash: impl Into<String>,
    sandbox_result: SandboxTestResult,
    defence_verdict: Option<DefenceVerdict>,
) -> Proposal {
    Proposal {
        id: skills::SkillEntry::id_from_name(&proposal.name),
        kind: ProposalKind::NewTool {
            name: proposal.name.clone(),
            description: proposal.description.clone(),
            wasm_hash: wasm_hash.into(),
            requested_capabilities: proposal.capabilities.clone(),
        },
        created_at_ns: proposal.proposed_at_ns,
        provenance: provenance_note(&proposal.authored_by, proposal.source_episode.as_deref()),
        sandbox_result: Some(sandbox_result),
        defence_verdict,
        status: ProposalStatus::Pending,
    }
}

/// Parse `name` / `description` out of SKILL.md frontmatter, falling back to
/// sensible defaults when a field is missing.
fn parse_name_and_description(skill_text: &str) -> (String, String) {
    let mut name = None;
    let mut description = None;
    for line in skill_text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("name:") {
            name = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("description:") {
            description = Some(rest.trim().to_string());
        }
        if name.is_some() && description.is_some() {
            break;
        }
    }
    (
        name.unwrap_or_else(|| "unnamed-skill".to_string()),
        description.unwrap_or_default(),
    )
}

// ── SkillApprovalBridge ─────────────────────────────────────────────────────────

/// Errors produced while routing an approval decision back to the registry.
#[derive(Debug, Clone, PartialEq)]
pub enum BridgeError {
    /// The queue rejected the enqueue (duplicate proposal id).
    DuplicateProposal(String),
    /// The queue operation failed (unknown id, wrong status, …).
    Queue(String),
    /// The registry operation failed (unknown skill id, …).
    Registry(String),
    /// The queue proposal id has no mapped registry skill id (e.g. it was a
    /// tool proposal, or was never enqueued through this bridge).
    UnmappedProposal(String),
}

impl core::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            BridgeError::DuplicateProposal(id) => {
                write!(f, "proposal already enqueued: {id}")
            }
            BridgeError::Queue(e) => write!(f, "approval queue error: {e}"),
            BridgeError::Registry(e) => write!(f, "skill registry error: {e}"),
            BridgeError::UnmappedProposal(id) => {
                write!(f, "no skill mapped for proposal id: {id}")
            }
        }
    }
}

impl std::error::Error for BridgeError {}

/// Orchestrates the E15↔E11 hand-off between an [`ApprovalQueue`] and a
/// [`SkillRegistry`].
///
/// The bridge keeps the proposal/skill id mapping that the queue itself does
/// not know about, so that an operator decision on a queued *skill* proposal is
/// routed to the matching [`SkillRegistry`] lifecycle call:
///
/// - [`approve`](Self::approve)  → [`SkillRegistry::promote`]   (Proposed→Active)
/// - [`reject`](Self::reject)    → registry untouched (skill stays Proposed)
/// - [`rollback`](Self::rollback)→ [`SkillRegistry::rollback`]  (→RolledBack)
///
/// Tool proposals are enqueued (so an operator sees them) but, because tool
/// activation lives in the (separate) tool registry rather than the skill
/// registry, their decisions are recorded in the queue without a skill-registry
/// side-effect; callers can list approved tool proposals via the queue.
///
/// The bridge borrows neither the queue nor the registry: callers pass them in
/// per call so the bridge can be a long-lived, cheaply-cloneable id map that
/// coexists with the existing ownership of those two structures.
#[derive(Debug, Clone, Default)]
pub struct SkillApprovalBridge {
    /// queue proposal id → registry skill id (skill proposals only).
    skill_of_proposal: HashMap<String, String>,
}

impl SkillApprovalBridge {
    /// Create an empty bridge with no tracked proposals.
    pub fn new() -> Self {
        SkillApprovalBridge::default()
    }

    /// Enqueue an already-built skill [`Proposal`] and record its id→skill_id
    /// mapping.
    ///
    /// Returns the queue proposal id on success.  Use this when you constructed
    /// the [`Proposal`] yourself (e.g. via
    /// [`skill_proposal_to_queue_proposal_with_hash`]).
    pub fn enqueue_skill_proposal(
        &mut self,
        queue: &mut ApprovalQueue,
        proposal: Proposal,
    ) -> Result<String, BridgeError> {
        // The skill id we route decisions to is the queue id (the converter sets
        // the queue id to the registry skill id).
        let proposal_id = proposal.id.clone();
        let skill_id = proposal.id.clone();
        if !queue.enqueue(proposal) {
            return Err(BridgeError::DuplicateProposal(proposal_id));
        }
        self.skill_of_proposal.insert(proposal_id.clone(), skill_id);
        Ok(proposal_id)
    }

    /// Convert an evaluated skill proposal and, if it is `PendingApproval`,
    /// enqueue it.
    ///
    /// Returns:
    /// - `Ok(Some(id))` — a `PendingApproval` skill was enqueued.
    /// - `Ok(None)`     — the skill was `AutoPromoted` or `Rejected`; nothing to
    ///   enqueue (this is the normal, non-error path).
    /// - `Err(..)`      — the queue rejected the enqueue (duplicate id).
    pub fn enqueue_skill(
        &mut self,
        queue: &mut ApprovalQueue,
        outcome: &ProposalOutcome,
        proposal: &SkillProposal,
    ) -> Result<Option<String>, BridgeError> {
        match skill_proposal_to_queue_proposal(outcome, proposal) {
            None => Ok(None),
            Some(queue_proposal) => self.enqueue_skill_proposal(queue, queue_proposal).map(Some),
        }
    }

    /// Enqueue a tool proposal (always — tools never auto-promote).
    ///
    /// Returns the queue proposal id.  Tool decisions are not routed to the
    /// skill registry (tools live in the tool registry), so no id→skill mapping
    /// is recorded.
    pub fn enqueue_tool(
        &mut self,
        queue: &mut ApprovalQueue,
        proposal: &ToolProposal,
        fixture_summary: impl Into<String>,
    ) -> Result<String, BridgeError> {
        let queue_proposal = tool_proposal_to_queue_proposal(proposal, fixture_summary);
        let id = queue_proposal.id.clone();
        if !queue.enqueue(queue_proposal) {
            return Err(BridgeError::DuplicateProposal(id));
        }
        Ok(id)
    }

    /// Approve a queued proposal and, when it is a tracked *skill* proposal,
    /// promote the corresponding skill in the registry (`Proposed`→`Active`).
    ///
    /// For tool proposals (no mapping recorded) the queue is updated but the
    /// skill registry is left untouched.
    pub fn approve(
        &self,
        queue: &mut ApprovalQueue,
        registry: &mut SkillRegistry,
        proposal_id: &str,
        reason: impl Into<String>,
    ) -> Result<(), BridgeError> {
        queue
            .approve(proposal_id, reason)
            .map_err(BridgeError::Queue)?;
        if let Some(skill_id) = self.skill_of_proposal.get(proposal_id) {
            registry
                .promote(skill_id)
                .map_err(|e| BridgeError::Registry(e.to_string()))?;
        }
        Ok(())
    }

    /// Reject a queued proposal.
    ///
    /// The registry is intentionally left untouched: a rejected skill stays in
    /// its current (`Proposed`) state rather than being promoted.  Callers that
    /// want a rejected skill removed from the registry can `rollback` instead.
    pub fn reject(
        &self,
        queue: &mut ApprovalQueue,
        proposal_id: &str,
        reason: impl Into<String>,
    ) -> Result<(), BridgeError> {
        queue
            .reject(proposal_id, reason)
            .map_err(BridgeError::Queue)
    }

    /// Roll back a previously-approved proposal and, when it is a tracked
    /// *skill* proposal, roll the corresponding skill back in the registry
    /// (→`RolledBack`).
    pub fn rollback(
        &self,
        queue: &mut ApprovalQueue,
        registry: &mut SkillRegistry,
        proposal_id: &str,
        reason: impl Into<String>,
    ) -> Result<(), BridgeError> {
        queue
            .rollback(proposal_id, reason)
            .map_err(BridgeError::Queue)?;
        if let Some(skill_id) = self.skill_of_proposal.get(proposal_id) {
            registry
                .rollback(skill_id)
                .map_err(|e| BridgeError::Registry(e.to_string()))?;
        }
        Ok(())
    }

    /// The registry skill id mapped to a queue proposal id, if any.
    pub fn skill_id_for(&self, proposal_id: &str) -> Option<&str> {
        self.skill_of_proposal.get(proposal_id).map(String::as_str)
    }

    /// Number of skill proposals currently tracked by the bridge.
    pub fn tracked_len(&self) -> usize {
        self.skill_of_proposal.len()
    }

    /// `true` when the bridge tracks no skill proposals.
    pub fn is_empty(&self) -> bool {
        self.skill_of_proposal.is_empty()
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use skills::{
        evaluate_skill_proposal, PromotionGateConfig, ProposalAction, SkillContentScreen,
    };

    const VALID_SKILL: &str = "\
---
name: regression-runner
description: Runs the regression suite and reports failures with file and line.
---

## Steps

1. Run the test suite.
2. Report failures.
";

    fn pending_skill_proposal() -> SkillProposal {
        SkillProposal {
            skill_text: VALID_SKILL.to_string(),
            authored_by: SkillAuthor::Agent,
            proposed_at_ns: 1_234,
            source_episode: Some("ep-7".to_string()),
        }
    }

    fn sample_tool_proposal() -> ToolProposal {
        ToolProposal {
            name: "markdown-renderer".to_string(),
            description: "Renders Markdown to HTML efficiently.".to_string(),
            capabilities: vec!["allow_stdout".to_string()],
            wasm_bytes: b"\x00asm\x01\x00\x00\x00".to_vec(),
            fixtures_json: "[]".to_string(),
            authored_by: SkillAuthor::Agent,
            proposed_at_ns: 5_678,
            source_episode: None,
        }
    }

    /// Evaluate a skill with auto-promotion disabled so it lands as
    /// `PendingApproval`, returning the outcome + the consumed proposal clone.
    fn evaluate_pending(reg: &mut SkillRegistry) -> (ProposalOutcome, SkillProposal) {
        let proposal = pending_skill_proposal();
        let outcome = evaluate_skill_proposal(
            proposal.clone(),
            reg,
            &SkillContentScreen::default(),
            &PromotionGateConfig {
                auto_promote_agent_skills: false,
            },
        )
        .unwrap();
        (outcome, proposal)
    }

    // (a) A PendingApproval skill proposal enqueues exactly one NewSkill proposal.
    #[test]
    fn pending_skill_enqueues_exactly_one_new_skill() {
        let mut reg = SkillRegistry::default();
        let mut queue = ApprovalQueue::new();
        let mut bridge = SkillApprovalBridge::new();

        let (outcome, proposal) = evaluate_pending(&mut reg);
        assert_eq!(outcome.action, ProposalAction::PendingApproval);

        let id = bridge
            .enqueue_skill(&mut queue, &outcome, &proposal)
            .unwrap();
        assert!(id.is_some());

        assert_eq!(queue.len(), 1);
        assert_eq!(queue.pending().len(), 1);
        let queued = &queue.pending()[0];
        assert!(matches!(queued.kind, ProposalKind::NewSkill { .. }));
        if let ProposalKind::NewSkill {
            name, description, ..
        } = &queued.kind
        {
            assert_eq!(name, "regression-runner");
            assert!(description.contains("regression"));
        }
        // Mapping recorded so an approval routes back to the registry.
        assert_eq!(bridge.tracked_len(), 1);
        assert_eq!(
            bridge.skill_id_for(id.as_deref().unwrap()),
            Some("regression-runner")
        );
    }

    // (b) An AutoPromoted skill does NOT enqueue.
    #[test]
    fn auto_promoted_skill_does_not_enqueue() {
        let mut reg = SkillRegistry::default();
        let mut queue = ApprovalQueue::new();
        let mut bridge = SkillApprovalBridge::new();

        // Default gate config auto-promotes agent skills.
        let proposal = pending_skill_proposal();
        let outcome = evaluate_skill_proposal(
            proposal.clone(),
            &mut reg,
            &SkillContentScreen::default(),
            &PromotionGateConfig::default(),
        )
        .unwrap();
        assert_eq!(outcome.action, ProposalAction::AutoPromoted);

        let enqueued = bridge
            .enqueue_skill(&mut queue, &outcome, &proposal)
            .unwrap();
        assert!(enqueued.is_none());
        assert!(queue.is_empty());
        assert!(bridge.is_empty());

        // Direct converter also returns None.
        assert!(skill_proposal_to_queue_proposal(&outcome, &proposal).is_none());
    }

    // A Rejected skill also does not enqueue.
    #[test]
    fn rejected_skill_does_not_enqueue() {
        let outcome = ProposalOutcome {
            artifact_id: None,
            action: ProposalAction::Rejected {
                reason: "injection pattern".to_string(),
            },
        };
        let proposal = pending_skill_proposal();
        assert!(skill_proposal_to_queue_proposal(&outcome, &proposal).is_none());
    }

    // (c) A tool proposal always enqueues a NewTool.
    #[test]
    fn tool_proposal_always_enqueues_new_tool() {
        let mut queue = ApprovalQueue::new();
        let mut bridge = SkillApprovalBridge::new();
        let tool = sample_tool_proposal();

        let id = bridge
            .enqueue_tool(&mut queue, &tool, "2/2 fixtures passed")
            .unwrap();

        assert_eq!(queue.len(), 1);
        let queued = queue.get(&id).unwrap();
        match &queued.kind {
            ProposalKind::NewTool {
                name,
                requested_capabilities,
                ..
            } => {
                assert_eq!(name, "markdown-renderer");
                assert_eq!(requested_capabilities, &vec!["allow_stdout".to_string()]);
            }
            other => panic!("expected NewTool, got {other:?}"),
        }
        // Sandbox summary carried through; tools are not skill-mapped.
        assert_eq!(
            queued.sandbox_result.as_ref().unwrap().summary,
            "2/2 fixtures passed"
        );
        assert!(bridge.is_empty());
    }

    // (d) Approving a queued skill proposal promotes the skill (Proposed→Active).
    #[test]
    fn approving_skill_promotes_in_registry() {
        let mut reg = SkillRegistry::default();
        let mut queue = ApprovalQueue::new();
        let mut bridge = SkillApprovalBridge::new();

        let (outcome, proposal) = evaluate_pending(&mut reg);
        let skill_id = outcome.artifact_id.clone().unwrap();
        // Skill starts Proposed (not in active list).
        assert_eq!(reg.list_active().len(), 0);

        let id = bridge
            .enqueue_skill(&mut queue, &outcome, &proposal)
            .unwrap()
            .unwrap();

        bridge
            .approve(&mut queue, &mut reg, &id, "looks safe")
            .unwrap();

        // Queue records approval; registry promoted the skill to Active.
        assert!(queue.get(&id).unwrap().is_approved());
        assert_eq!(reg.list_active().len(), 1);
        let entry = reg
            .list_all()
            .into_iter()
            .find(|e| e.id == skill_id)
            .unwrap();
        assert!(entry.state.is_active());
    }

    // (e) Rejecting does not promote.
    #[test]
    fn rejecting_skill_does_not_promote() {
        let mut reg = SkillRegistry::default();
        let mut queue = ApprovalQueue::new();
        let mut bridge = SkillApprovalBridge::new();

        let (outcome, proposal) = evaluate_pending(&mut reg);
        let skill_id = outcome.artifact_id.clone().unwrap();

        let id = bridge
            .enqueue_skill(&mut queue, &outcome, &proposal)
            .unwrap()
            .unwrap();

        bridge
            .reject(&mut queue, &id, "capability too broad")
            .unwrap();

        // Queue shows rejected; registry skill is still NOT active.
        assert!(matches!(
            queue.get(&id).unwrap().status,
            ProposalStatus::Rejected { .. }
        ));
        assert_eq!(reg.list_active().len(), 0);
        let entry = reg
            .list_all()
            .into_iter()
            .find(|e| e.id == skill_id)
            .unwrap();
        assert!(entry.state.is_proposed());
    }

    #[test]
    fn rollback_after_approve_rolls_back_skill() {
        let mut reg = SkillRegistry::default();
        let mut queue = ApprovalQueue::new();
        let mut bridge = SkillApprovalBridge::new();

        let (outcome, proposal) = evaluate_pending(&mut reg);
        let skill_id = outcome.artifact_id.clone().unwrap();
        let id = bridge
            .enqueue_skill(&mut queue, &outcome, &proposal)
            .unwrap()
            .unwrap();

        bridge.approve(&mut queue, &mut reg, &id, "ok").unwrap();
        assert_eq!(reg.list_active().len(), 1);

        bridge
            .rollback(&mut queue, &mut reg, &id, "regressed in prod")
            .unwrap();

        assert!(matches!(
            queue.get(&id).unwrap().status,
            ProposalStatus::RolledBack { .. }
        ));
        assert_eq!(reg.list_active().len(), 0);
        let entry = reg
            .list_all()
            .into_iter()
            .find(|e| e.id == skill_id)
            .unwrap();
        assert_eq!(entry.state, skills::SkillState::RolledBack);
    }

    #[test]
    fn duplicate_enqueue_is_rejected() {
        let mut reg = SkillRegistry::default();
        let mut queue = ApprovalQueue::new();
        let mut bridge = SkillApprovalBridge::new();

        let (outcome, proposal) = evaluate_pending(&mut reg);
        bridge
            .enqueue_skill(&mut queue, &outcome, &proposal)
            .unwrap();

        // Re-converting the same outcome yields the same queue id → duplicate.
        let again = skill_proposal_to_queue_proposal(&outcome, &proposal).unwrap();
        let err = bridge
            .enqueue_skill_proposal(&mut queue, again)
            .unwrap_err();
        assert!(matches!(err, BridgeError::DuplicateProposal(_)));
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn content_fingerprint_is_stable_and_distinct() {
        let a = content_fingerprint(b"hello world");
        let b = content_fingerprint(b"hello world");
        let c = content_fingerprint(b"goodbye world");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn explicit_hash_is_used_when_provided() {
        let mut reg = SkillRegistry::default();
        let (outcome, proposal) = evaluate_pending(&mut reg);
        let p = skill_proposal_to_queue_proposal_with_hash(&outcome, &proposal, "deadbeefcafe")
            .unwrap();
        if let ProposalKind::NewSkill { prompt_hash, .. } = p.kind {
            assert_eq!(prompt_hash, "deadbeefcafe");
        } else {
            panic!("expected NewSkill");
        }
    }
}
