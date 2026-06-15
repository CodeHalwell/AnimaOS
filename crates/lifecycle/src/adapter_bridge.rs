//! E15↔E8 bridge — route a self-trained adapter into the approval queue.
//!
//! A sleep-phase adapter that has cleared the **finetune adoption gate**
//! (`anima_finetune::decide_adoption`: eval harness + alignment) has passed the
//! *automated* checks, but the report still requires a **human** sign-off before
//! the router mounts it. This bridge turns such an adapter into a
//! [`ProposalKind::WeightUpdate`] [`Proposal`] so it appears in the operator
//! approval queue, symmetric to how [`crate::skill_bridge`] routes E11
//! skill/tool proposals.

use anima_finetune::{AdapterArtifact, AdoptionDecision};

use crate::approval::{Proposal, ProposalKind, ProposalStatus};

/// Convert an adapter that has cleared the adoption gate into an operator-gated
/// [`Proposal`].
///
/// Returns `None` when `decision` was **not** approved — an adapter that failed
/// the automated eval/alignment gate must never reach the operator queue (it is
/// not promotable), mirroring `skill_proposal_to_queue_proposal` returning
/// `None` for skills that were not `PendingApproval`.
///
/// On approval the proposal carries the base model, the adapter weights digest,
/// the adaptation rank (if any), and a caller-supplied `training_summary`; its
/// `provenance` records that the automated gate cleared it.
pub fn adapter_adoption_to_proposal(
    artifact: &AdapterArtifact,
    decision: &AdoptionDecision,
    training_summary: impl Into<String>,
    now_ns: u64,
) -> Option<Proposal> {
    if !decision.approved {
        return None;
    }
    Some(Proposal {
        // Use the adapter id as the queue id so the round-trip mapping is direct.
        id: artifact.adapter_id.clone(),
        kind: ProposalKind::WeightUpdate {
            model_id: artifact.provenance.base_model.clone(),
            adapter_hash: artifact.weights_digest.clone(),
            rank: artifact.provenance.method.rank(),
            training_summary: training_summary.into(),
        },
        created_at_ns: now_ns,
        provenance: format!(
            "self-trained adapter `{}` ({}); cleared finetune adoption gate \
             (eval + alignment), job `{}`",
            artifact.adapter_id,
            artifact.provenance.method.label(),
            artifact.provenance.source_job,
        ),
        // Weight updates aren't WASM-sandboxed; the eval harness + alignment
        // screen are the automated checks, already encoded by `decision`.
        sandbox_result: None,
        defence_verdict: None,
        status: ProposalStatus::Pending,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::ApprovalQueue;
    use anima_finetune::artifact::{AdapterFormat, Provenance};
    use anima_finetune::method::{AdaptationMethod, MergePath, ServingTier};
    use anima_finetune::AdapterArtifact;

    fn artifact() -> AdapterArtifact {
        AdapterArtifact {
            adapter_id: "nightly-adapter".to_string(),
            description: "trained on episodic experience".to_string(),
            format: AdapterFormat::LoraAdapter,
            merge_path: MergePath::Clean,
            serving_tier: ServingTier::MountableAdapter,
            weights_digest: "abc123".to_string(),
            adapter_path: None,
            merged_gguf_path: None,
            provenance: Provenance {
                base_model: "base-q4".to_string(),
                method: AdaptationMethod::QLora {
                    rank: 16,
                    alpha: 32,
                    base_bits: 4,
                },
                source_job: "consolidation-anima".to_string(),
                created_at_ns: 100,
            },
        }
    }

    fn approved() -> AdoptionDecision {
        AdoptionDecision {
            adapter_id: "nightly-adapter".to_string(),
            approved: true,
            eval_passed: true,
            alignment_passed: true,
            reasons: vec![],
        }
    }

    fn rejected() -> AdoptionDecision {
        AdoptionDecision {
            adapter_id: "nightly-adapter".to_string(),
            approved: false,
            eval_passed: false,
            alignment_passed: true,
            reasons: vec!["eval: did not beat baseline".to_string()],
        }
    }

    #[test]
    fn approved_adapter_becomes_weight_update_proposal() {
        let p = adapter_adoption_to_proposal(&artifact(), &approved(), "120 episodic pairs", 200)
            .expect("approved adapter yields a proposal");
        assert_eq!(p.id, "nightly-adapter");
        assert_eq!(p.created_at_ns, 200);
        assert!(p.is_pending());
        match p.kind {
            ProposalKind::WeightUpdate {
                model_id,
                adapter_hash,
                rank,
                training_summary,
            } => {
                assert_eq!(model_id, "base-q4");
                assert_eq!(adapter_hash, "abc123");
                assert_eq!(rank, Some(16));
                assert_eq!(training_summary, "120 episodic pairs");
            }
            other => panic!("expected WeightUpdate, got {other:?}"),
        }
        assert!(p.provenance.contains("adoption gate"));
    }

    #[test]
    fn rejected_adapter_yields_no_proposal() {
        assert!(adapter_adoption_to_proposal(&artifact(), &rejected(), "x", 1).is_none());
    }

    #[test]
    fn proposal_enqueues_and_surfaces_as_pending() {
        let p = adapter_adoption_to_proposal(&artifact(), &approved(), "summary", 10).unwrap();
        let mut queue = ApprovalQueue::new();
        assert!(queue.enqueue(p));
        let pending = queue.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].kind.label(), "weight-update");
    }
}
