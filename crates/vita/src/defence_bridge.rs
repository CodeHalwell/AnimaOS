//! Translates [`defence::ScreeningOutcome`] results into audit log entries.
//!
//! This bridge sits between the cortex IPC path and the audit log, converting
//! the structured [`ScreeningOutcome`] type from the `defence` crate into the
//! `vita`-owned [`AuditEntry`] variants without coupling the two crates beyond
//! this module.

use defence::{ScreeningOutcome, VetoResult};

use crate::audit::{AuditEntry, AuditLog};

/// Pushes audit entries for a defence screening outcome.
///
/// Behaviour:
/// - If the outcome is a veto, pushes a [`AuditEntry::DefenceVeto`] entry.
/// - If the outcome triggered attention escalation, additionally pushes an
///   [`AuditEntry::AttentionDemandEscalated`] entry.
/// - If the action was allowed, no entry is pushed (allow paths are not audited
///   here — the cortex-bridge already records its own entries).
///
/// # Parameters
/// - `audit`: the audit log to write to.
/// - `outcome`: the [`ScreeningOutcome`] returned by [`defence::DefenceLayer::screen`].
/// - `agent_id`: identifier of the agent whose cortex proposal was screened.
/// - `invocation_id`: per-invocation identifier for correlation.
/// - `action_blocked`: human-readable description of the blocked action.
/// - `window_secs`: the veto-window duration from the defence config (used in
///   the escalation entry).
pub fn push_defence_outcome(
    audit: &mut AuditLog,
    outcome: &ScreeningOutcome,
    agent_id: &str,
    invocation_id: &str,
    action_blocked: &str,
    window_secs: u64,
) {
    if let VetoResult::Veto(ref reason) = outcome.veto {
        audit.push(AuditEntry::DefenceVeto {
            agent_id: agent_id.to_owned(),
            invocation_id: invocation_id.to_owned(),
            detector: outcome.detector.to_owned(),
            action_blocked: action_blocked.to_owned(),
            reason: reason.description(),
        });

        if outcome.attention_escalated {
            audit.push(AuditEntry::AttentionDemandEscalated {
                agent_id: agent_id.to_owned(),
                invocation_id: invocation_id.to_owned(),
                veto_count: outcome.veto_count_in_window,
                window_secs,
            });
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use defence::{ScreeningOutcome, VetoReason, VetoResult};

    fn veto_outcome(reason: VetoReason, escalated: bool, count: usize) -> ScreeningOutcome {
        ScreeningOutcome {
            veto: VetoResult::Veto(reason),
            attention_escalated: escalated,
            detector: "TestDetector",
            veto_count_in_window: count,
        }
    }

    fn allow_outcome() -> ScreeningOutcome {
        ScreeningOutcome {
            veto: VetoResult::Allow,
            attention_escalated: false,
            detector: "none",
            veto_count_in_window: 0,
        }
    }

    #[test]
    fn allow_outcome_pushes_no_entries() {
        let mut audit = AuditLog::new();
        push_defence_outcome(&mut audit, &allow_outcome(), "agent", "inv-1", "some action", 300);
        assert!(audit.is_empty());
    }

    #[test]
    fn veto_without_escalation_pushes_one_entry() {
        let mut audit = AuditLog::new();
        let outcome = veto_outcome(
            VetoReason::PromptInjection {
                pattern: "ignore previous".to_owned(),
                source: "tool:http".to_owned(),
            },
            false,
            1,
        );
        push_defence_outcome(&mut audit, &outcome, "agent-a", "inv-42", "http call", 300);

        assert_eq!(audit.len(), 1);
        assert!(matches!(
            &audit.entries()[0],
            AuditEntry::DefenceVeto { invocation_id, .. } if invocation_id == "inv-42"
        ));
    }

    #[test]
    fn veto_with_escalation_pushes_two_entries() {
        let mut audit = AuditLog::new();
        let outcome = veto_outcome(
            VetoReason::UnsafeMotorAction {
                action: "delete /etc/passwd".to_owned(),
                policy: "critical_paths".to_owned(),
            },
            true,
            3,
        );
        push_defence_outcome(&mut audit, &outcome, "agent-b", "inv-99", "delete op", 300);

        assert_eq!(audit.len(), 2);
        assert!(matches!(&audit.entries()[0], AuditEntry::DefenceVeto { .. }));
        assert!(matches!(
            &audit.entries()[1],
            AuditEntry::AttentionDemandEscalated { veto_count: 3, window_secs: 300, .. }
        ));
    }

    #[test]
    fn veto_entry_carries_correct_detector_name() {
        let mut audit = AuditLog::new();
        let outcome = ScreeningOutcome {
            veto: VetoResult::Veto(VetoReason::RewardHacking {
                claimed_completion: "done".to_owned(),
                reason: "no evidence".to_owned(),
            }),
            attention_escalated: false,
            detector: "RewardHackingDetector",
            veto_count_in_window: 1,
        };
        push_defence_outcome(&mut audit, &outcome, "agent-c", "inv-1", "claim", 300);

        match &audit.entries()[0] {
            AuditEntry::DefenceVeto { detector, .. } => {
                assert_eq!(detector, "RewardHackingDetector");
            }
            other => panic!("unexpected entry: {other:?}"),
        }
    }
}
