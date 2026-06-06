//! E11 S11.5 integration: the Dreaming-phase self-improvement loop end-to-end.
//!
//! Exercises the hosted routing surface using the public crate APIs:
//!   1. `vita::LifecycleManager` runs the Dreaming-phase reflection during a
//!      sleep cycle, registering an agent-authored `Proposed` skill into its
//!      shared `SkillRegistry` (the `vita → skills` edge; no `lifecycle` dep).
//!   2. The hosted side (which may depend on both `vita` and `lifecycle`) drains
//!      the registry's pending agent-authored proposals into the E15
//!      `ApprovalQueue` via `lifecycle::SkillApprovalBridge` — the same hand-off
//!      the `anima-hosted skills reflect` command performs.
//!
//! This proves the dependency cycle is avoided: vita registers, hosted routes.

use std::sync::Arc;

use lifecycle::approval::{ApprovalQueue, ProposalKind, ProposalStatus};
use lifecycle::skill_bridge::SkillApprovalBridge;
use memory::VirtualContextManager;
use scheduler::MockLlmBackend;
use senses::{HumanGuidance, SensoryBridge};
use skills::{
    EpisodeSummary, ProposalAction, ProposalOutcome, SkillAuthor, SkillProposal, SkillRegistry,
    SkillState,
};
use vita::{AuditEntry, LifecycleConfig, LifecycleManager};

fn manager_with_registry(agent_id: &str) -> LifecycleManager {
    LifecycleManager::new(
        agent_id,
        SensoryBridge::new(HumanGuidance::new("test")),
        VirtualContextManager::with_capacity(0, 4096),
        LifecycleConfig { max_context: 4096 },
        HumanGuidance::new("boot"),
        Arc::new(MockLlmBackend::new()),
        Some(0),
    )
    .with_skill_registry(SkillRegistry::default())
}

fn co_occurrence_episodes() -> Vec<EpisodeSummary> {
    (0..3)
        .map(|i| EpisodeSummary {
            episode_id: format!("ep-{i}"),
            summary: format!("episode {i}: searched then archived"),
            tools_used: vec!["web-search".to_string(), "archive".to_string()],
            success: true,
        })
        .collect()
}

/// Drain the registry's pending agent-authored skills into the approval queue —
/// the routing the hosted kernel performs after a dream cycle.
fn route_pending_into_queue(
    registry: &SkillRegistry,
    proposed_ids: &[String],
    queue: &mut ApprovalQueue,
    bridge: &mut SkillApprovalBridge,
) {
    for skill_id in proposed_ids {
        let entry = match registry.list_all().into_iter().find(|e| &e.id == skill_id) {
            Some(e) => e,
            None => continue,
        };
        if !matches!(entry.state, SkillState::Proposed) {
            continue;
        }
        let skill_text = format!(
            "---\nname: {name}\ndescription: {desc}\n---\n",
            name = entry.manifest.name,
            desc = entry.manifest.description,
        );
        let proposal = SkillProposal {
            skill_text,
            authored_by: SkillAuthor::Agent,
            proposed_at_ns: entry.provenance.proposed_at_ns,
            source_episode: entry.provenance.source_episode.clone(),
        };
        let outcome = ProposalOutcome {
            artifact_id: Some(skill_id.clone()),
            action: ProposalAction::PendingApproval,
        };
        bridge
            .enqueue_skill(queue, &outcome, &proposal)
            .expect("enqueue of pending proposal must succeed");
    }
}

#[test]
fn dreaming_reflection_proposals_enqueue_into_approval_queue() {
    let mut m = manager_with_registry("dream-agent");
    for ep in co_occurrence_episodes() {
        m.record_episode_summary(ep);
    }

    // Step 1: vita's sleep cycle runs the Dreaming-phase reflection.
    m.run_sleep_cycle();

    // The reflection registered >=1 Proposed agent-authored skill + audit.
    let registry_handle = m.skill_registry_handle().expect("registry installed");
    let proposed_ids: Vec<String> = {
        let guard = registry_handle.lock().unwrap();
        guard
            .list_all()
            .into_iter()
            .filter(|e| e.state.is_proposed())
            .map(|e| e.id.clone())
            .collect()
    };
    assert!(
        !proposed_ids.is_empty(),
        "Dreaming reflection must register at least one Proposed skill"
    );
    assert!(
        m.audit
            .entries()
            .iter()
            .any(|e| matches!(e, AuditEntry::SkillReflectionCompleted { .. })),
        "a SkillReflectionCompleted audit entry must be emitted"
    );

    // Step 2: hosted routing — drain the pending proposals into the E15 queue.
    let mut queue = ApprovalQueue::new();
    let mut bridge = SkillApprovalBridge::new();
    {
        let guard = registry_handle.lock().unwrap();
        route_pending_into_queue(&guard, &proposed_ids, &mut queue, &mut bridge);
    }

    // Every Proposed reflection skill is now a pending NewSkill in the queue.
    assert_eq!(
        queue.pending().len(),
        proposed_ids.len(),
        "all pending reflection proposals must be enqueued"
    );
    for p in queue.pending() {
        assert!(matches!(p.kind, ProposalKind::NewSkill { .. }));
        assert!(matches!(p.status, ProposalStatus::Pending));
    }
    assert_eq!(bridge.tracked_len(), proposed_ids.len());

    // Step 3: operator approval promotes the skill back in the registry.
    let target = &proposed_ids[0];
    {
        let mut guard = registry_handle.lock().unwrap();
        bridge
            .approve(&mut queue, &mut guard, target, "operator ok")
            .expect("approve must route to registry promote");
        let entry = guard
            .list_all()
            .into_iter()
            .find(|e| &e.id == target)
            .unwrap();
        assert!(
            entry.state.is_active(),
            "approved skill must be promoted to Active"
        );
    }
    assert!(queue.get(target).unwrap().is_approved());
}

#[test]
fn no_pattern_means_no_queue_entries() {
    let mut m = manager_with_registry("dream-agent-empty");
    // Distinct tools per episode → no co-occurrence pattern above threshold.
    for (i, tools) in [["a", "b"], ["c", "d"], ["e", "f"]].into_iter().enumerate() {
        m.record_episode_summary(EpisodeSummary {
            episode_id: format!("e{i}"),
            summary: format!("episode {i}"),
            tools_used: tools.iter().map(|t| t.to_string()).collect(),
            success: true,
        });
    }

    m.run_sleep_cycle();

    let registry_handle = m.skill_registry_handle().unwrap();
    let guard = registry_handle.lock().unwrap();
    assert!(
        guard.is_empty(),
        "nothing should be registered without a qualifying pattern"
    );
}
