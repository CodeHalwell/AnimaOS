#![allow(clippy::doc_markdown)]

//! E32 ↔ E8 — corpus-growth fine-tune proposal trigger.
//!
//! Closes the autonomy loop the report calls for: when the agent's accumulated
//! sleep-phase training corpus crosses a size threshold, it should *propose* a
//! fine-tune run rather than wait for a human to remember to launch one. This
//! module is that policy. It is deliberately a **pure decision** over plain
//! numbers — corpus pair count, a threshold, a cooldown — so it unit-tests
//! hermetically and carries no dependency on `memory`/`finetune`.
//!
//! On firing it emits a one-shot [`ScheduledJob`] ([`JobSchedule::Immediate`])
//! whose opaque `payload` is the JSON [`FineTuneProposalPayload`] below; the
//! hosted runner dispatches that payload (training is operator-gated downstream
//! via the E8 adoption gate + E15 approval queue, so the job *proposes*, it does
//! not silently adopt). A **cooldown** stops the agent re-proposing on every
//! sleep cycle while the corpus keeps growing past the threshold.

use serde::{Deserialize, Serialize};

use crate::job::ScheduledJob;
use crate::schedule::JobSchedule;

/// The structured `payload` carried by a fine-tune proposal job.
///
/// Serialised into [`ScheduledJob::payload`]; the hosted runner deserialises it
/// to launch the `anima-finetune` pipeline against the named base model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FineTuneProposalPayload {
    /// Discriminator so the runner can route the payload. Always
    /// `"finetune_proposal"`.
    pub kind: String,
    /// Base model the proposed adapter targets.
    pub base_model: String,
    /// Accumulated training-pair count that triggered the proposal.
    pub corpus_pairs: usize,
    /// Why the proposal fired (always `"corpus_threshold"` for this trigger).
    pub reason: String,
}

impl FineTuneProposalPayload {
    /// The discriminator value the runner matches on.
    pub const KIND: &'static str = "finetune_proposal";

    fn new(base_model: impl Into<String>, corpus_pairs: usize) -> Self {
        Self {
            kind: Self::KIND.to_string(),
            base_model: base_model.into(),
            corpus_pairs,
            reason: "corpus_threshold".to_string(),
        }
    }
}

/// Policy controlling when a corpus-growth fine-tune is proposed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FineTuneTrigger {
    /// Minimum accumulated training-pair count before a fine-tune is proposed.
    pub corpus_threshold: usize,
    /// Minimum nanoseconds between successive proposals. Prevents re-proposing
    /// every sleep cycle while the corpus stays above the threshold.
    pub cooldown_ns: u64,
}

impl FineTuneTrigger {
    /// Construct a trigger. A `corpus_threshold` of 0 is clamped to 1 so a fresh
    /// agent with an empty corpus never proposes.
    pub fn new(corpus_threshold: usize, cooldown_ns: u64) -> Self {
        Self {
            corpus_threshold: corpus_threshold.max(1),
            cooldown_ns,
        }
    }

    /// Whether a proposal should fire now.
    ///
    /// Fires when `corpus_pairs >= corpus_threshold` **and** either nothing has
    /// been proposed yet or at least `cooldown_ns` has elapsed since the last
    /// proposal. `now_ns < last_proposed_at_ns` (clock skew) is treated as "no
    /// time elapsed", so the cooldown still applies.
    pub fn should_propose(
        &self,
        corpus_pairs: usize,
        now_ns: u64,
        last_proposed_at_ns: Option<u64>,
    ) -> bool {
        if corpus_pairs < self.corpus_threshold {
            return false;
        }
        match last_proposed_at_ns {
            None => true,
            Some(prev) => now_ns.saturating_sub(prev) >= self.cooldown_ns,
        }
    }

    /// Build the one-shot proposal job for the given corpus state.
    ///
    /// The caller is responsible for adding it to the [`JobRegistry`] and for
    /// remembering `now_ns` as the new `last_proposed_at_ns`.
    ///
    /// [`JobRegistry`]: crate::registry::JobRegistry
    pub fn build_job(
        &self,
        corpus_pairs: usize,
        base_model: &str,
        workspace_id: &str,
        now_ns: u64,
    ) -> ScheduledJob {
        let payload = FineTuneProposalPayload::new(base_model, corpus_pairs);
        // Serialising this small, owned struct cannot fail; assert the invariant
        // rather than silently enqueueing an empty payload the runner can't route.
        let payload_json = serde_json::to_string(&payload)
            .expect("FineTuneProposalPayload serialises to JSON");
        ScheduledJob::new(
            format!("fine-tune proposal: corpus reached {corpus_pairs} pairs"),
            workspace_id,
            payload_json,
            JobSchedule::Immediate,
            now_ns,
        )
    }

    /// Convenience: return a proposal job iff [`should_propose`] fires.
    ///
    /// [`should_propose`]: Self::should_propose
    pub fn evaluate(
        &self,
        corpus_pairs: usize,
        base_model: &str,
        workspace_id: &str,
        now_ns: u64,
        last_proposed_at_ns: Option<u64>,
    ) -> Option<ScheduledJob> {
        if self.should_propose(corpus_pairs, now_ns, last_proposed_at_ns) {
            Some(self.build_job(corpus_pairs, base_model, workspace_id, now_ns))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR_NS: u64 = 3_600 * 1_000_000_000;

    fn trigger() -> FineTuneTrigger {
        FineTuneTrigger::new(100, 24 * HOUR_NS)
    }

    #[test]
    fn does_not_fire_below_threshold() {
        assert!(!trigger().should_propose(99, HOUR_NS, None));
    }

    #[test]
    fn fires_at_threshold_with_no_prior_proposal() {
        assert!(trigger().should_propose(100, HOUR_NS, None));
        assert!(trigger().should_propose(250, HOUR_NS, None));
    }

    #[test]
    fn cooldown_blocks_reproposal_until_elapsed() {
        let t = trigger();
        let last = 10 * HOUR_NS;
        // 12h later: still within the 24h cooldown.
        assert!(!t.should_propose(500, last + 12 * HOUR_NS, Some(last)));
        // 24h later: cooldown satisfied.
        assert!(t.should_propose(500, last + 24 * HOUR_NS, Some(last)));
    }

    #[test]
    fn clock_skew_does_not_bypass_cooldown() {
        let t = trigger();
        let last = 100 * HOUR_NS;
        // now < last (skew) → treated as zero elapsed → still cooling down.
        assert!(!t.should_propose(500, last - 5 * HOUR_NS, Some(last)));
    }

    #[test]
    fn zero_threshold_is_clamped_so_empty_corpus_never_fires() {
        let t = FineTuneTrigger::new(0, 0);
        assert_eq!(t.corpus_threshold, 1);
        assert!(!t.should_propose(0, 1, None));
        assert!(t.should_propose(1, 1, None));
    }

    #[test]
    fn build_job_is_immediate_oneshot_with_structured_payload() {
        let job = trigger().build_job(150, "base-q4", "ws-1", 42);
        assert_eq!(job.schedule, JobSchedule::Immediate);
        assert_eq!(job.workspace_id, "ws-1");
        assert!(job.description.contains("150 pairs"));

        let payload: FineTuneProposalPayload = serde_json::from_str(&job.payload).unwrap();
        assert_eq!(payload.kind, FineTuneProposalPayload::KIND);
        assert_eq!(payload.base_model, "base-q4");
        assert_eq!(payload.corpus_pairs, 150);
        assert_eq!(payload.reason, "corpus_threshold");
    }

    #[test]
    fn evaluate_returns_none_when_policy_does_not_fire() {
        assert!(trigger().evaluate(50, "base-q4", "", 1, None).is_none());
    }

    #[test]
    fn evaluate_returns_job_when_policy_fires() {
        let job = trigger()
            .evaluate(200, "base-q4", "", HOUR_NS, None)
            .expect("policy fires");
        assert!(job.is_active());
        assert_eq!(job.schedule.type_label(), "immediate");
    }

    #[test]
    fn trigger_serde_round_trip() {
        let t = trigger();
        let json = serde_json::to_string(&t).unwrap();
        let back: FineTuneTrigger = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }
}
