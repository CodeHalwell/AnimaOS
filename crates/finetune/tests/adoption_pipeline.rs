//! End-to-end S8.4.8 adoption pipeline:
//! `train → register → evaluate → decide → mount_gated`.
//!
//! Proves the documented pipeline shape: a freshly trained adapter is *known*
//! to the library but cannot reach a serving tier until it has cleared both
//! halves of the adoption gate (eval harness + alignment screen).

use anima_finetune::eval::{CaseCounts, Metric};
use anima_finetune::{
    decide_adoption, evaluate_adapter, AdapterLibrary, AdoptionPolicy, AlignmentOutcome, EvalCase,
    EvalReport, FineTuneConfig, FineTuneJob, FineTuner, FixtureFineTuner, MetricScores, MountError,
    MountId, TrainingPair, TrainingSet,
};

/// Train a deterministic fixture adapter from a couple of pairs.
fn train(adapter_id: &str) -> anima_finetune::AdapterArtifact {
    let tuner = FixtureFineTuner::new();
    let cfg = FineTuneConfig::new("base-q4", "episodic://test", adapter_id);
    let job = FineTuneJob::new(format!("consolidation-{adapter_id}"), cfg);
    let pairs = vec![
        TrainingPair::new("what is my name?", "Ada"),
        TrainingPair::new("favourite colour?", "teal"),
    ];
    let set = TrainingSet::from_pairs(&pairs);
    tuner
        .run_job(&job, set.pairs())
        .expect("fixture training is deterministic and infallible")
}

/// A deliberately weak LoRA baseline so a competent candidate clears the margin.
fn weak_baseline() -> EvalReport {
    EvalReport {
        adapter_id: "lora-baseline".to_string(),
        scores: MetricScores {
            task_success: 0.50,
            ood_generalisation: 0.50,
            retention: 0.50,
            merge_fidelity: 1.0,
        },
        case_counts: CaseCounts {
            task_success: 1,
            ood_generalisation: 1,
            retention: 1,
        },
    }
}

fn strong_cases() -> Vec<EvalCase> {
    vec![
        EvalCase::new("t1", Metric::TaskSuccess, "held-out", 0.95),
        EvalCase::new("o1", Metric::OodGeneralisation, "shifted", 0.95),
        EvalCase::new("r1", Metric::Retention, "core fact", 0.95),
    ]
}

#[test]
fn adapter_mounts_only_after_clearing_the_gate() {
    let artifact = train("nightly-adapter");
    let mut lib = AdapterLibrary::new(8);
    lib.register(artifact.clone()).unwrap();

    let mount = MountId::new("cheap-local", "base-q4");

    // Registered but un-gated: the router's gated mount refuses it.
    assert!(matches!(
        lib.mount_gated(mount.clone(), &artifact.adapter_id),
        Err(MountError::NotAdopted { .. })
    ));

    // Evaluate against the baseline and an approving alignment screen.
    let candidate = evaluate_adapter(&artifact, &strong_cases());
    let decision = decide_adoption(
        &candidate,
        &weak_baseline(),
        &AlignmentOutcome::Approved,
        &AdoptionPolicy::default(),
    );
    assert!(
        decision.approved,
        "candidate should clear the gate; reasons: {:?}",
        decision.reasons
    );

    // Automated clearance alone is not enough — the operator half still gates it.
    lib.record_adoption(&decision);
    assert!(matches!(
        lib.mount_gated(mount.clone(), &artifact.adapter_id),
        Err(MountError::NotOperatorApproved { .. })
    ));

    // With operator sign-off recorded too, the same mount succeeds.
    lib.record_operator_approval(&artifact.adapter_id);
    lib.mount_gated(mount.clone(), &artifact.adapter_id)
        .unwrap();
    assert_eq!(lib.mounted_at(&mount), Some(artifact.adapter_id.as_str()));
}

#[test]
fn alignment_veto_blocks_mount_despite_good_eval() {
    let artifact = train("misaligned-adapter");
    let mut lib = AdapterLibrary::new(8);
    lib.register(artifact.clone()).unwrap();

    let candidate = evaluate_adapter(&artifact, &strong_cases());
    let decision = decide_adoption(
        &candidate,
        &weak_baseline(),
        &AlignmentOutcome::Vetoed {
            reasons: vec!["violates corrigibility clause".to_string()],
        },
        &AdoptionPolicy::default(),
    );

    assert!(decision.eval_passed, "eval half should pass on merit");
    assert!(!decision.approved, "alignment veto must block adoption");
    lib.record_adoption(&decision);

    let mount = MountId::new("cheap-local", "base-q4");
    assert!(matches!(
        lib.mount_gated(mount, &artifact.adapter_id),
        Err(MountError::NotAdopted { .. })
    ));
}
