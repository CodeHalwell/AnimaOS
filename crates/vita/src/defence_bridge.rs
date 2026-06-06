//! Translates [`defence::ScreeningOutcome`] results into audit log entries.
//!
//! This bridge sits between the cortex IPC path and the audit log, converting
//! the structured [`ScreeningOutcome`] type from the `defence` crate into the
//! `vita`-owned [`AuditEntry`] variants without coupling the two crates beyond
//! this module.

use defence::{ScreeningOutcome, VetoReason, VetoResult};

use crate::audit::{AuditEntry, AuditLog};

/// Pushes audit entries for a defence screening outcome.
///
/// Behaviour:
/// - Charter violations ([`VetoReason::CharterViolation`]) emit a
///   [`AuditEntry::ConstitutionVeto`] at higher severity (E13, S13.2).
/// - All other vetoes emit a [`AuditEntry::DefenceVeto`] entry.
/// - When the outcome triggered attention escalation, an additional
///   [`AuditEntry::AttentionDemandEscalated`] entry is appended.
/// - Allowed actions push no entries.
///
/// # Parameters
/// - `audit`: the audit log to write to.
/// - `outcome`: the [`ScreeningOutcome`] from [`defence::DefenceLayer::screen`].
/// - `agent_id`: identifier of the agent whose cortex proposal was screened.
/// - `invocation_id`: per-invocation identifier for correlation.
/// - `action_blocked`: human-readable description of the blocked action.
/// - `proposal_type`: category label of the screened proposal (e.g. `"CortexAction"`).
/// - `window_secs`: veto-window duration from the defence config.
pub fn push_defence_outcome(
    audit: &mut AuditLog,
    outcome: &ScreeningOutcome,
    agent_id: &str,
    invocation_id: &str,
    action_blocked: &str,
    proposal_type: &str,
    window_secs: u64,
) {
    if let VetoResult::Veto(ref reason) = outcome.veto {
        // Charter violations get a dedicated high-severity audit entry (E13).
        if let VetoReason::CharterViolation {
            prohibition_id,
            clause_text,
            ..
        } = reason
        {
            audit.push(AuditEntry::ConstitutionVeto {
                agent_id: agent_id.to_owned(),
                invocation_id: invocation_id.to_owned(),
                prohibition_id: prohibition_id.clone(),
                clause_text: clause_text.clone(),
                action_blocked: action_blocked.to_owned(),
                proposal_type: proposal_type.to_owned(),
            });
        } else {
            audit.push(AuditEntry::DefenceVeto {
                agent_id: agent_id.to_owned(),
                invocation_id: invocation_id.to_owned(),
                detector: outcome.detector.to_owned(),
                action_blocked: action_blocked.to_owned(),
                reason: reason.description(),
            });
        }

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
        push_defence_outcome(
            &mut audit,
            &allow_outcome(),
            "agent",
            "inv-1",
            "some action",
            "CortexAction",
            300,
        );
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
        push_defence_outcome(
            &mut audit,
            &outcome,
            "agent-a",
            "inv-42",
            "http call",
            "CortexAction",
            300,
        );

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
        push_defence_outcome(
            &mut audit,
            &outcome,
            "agent-b",
            "inv-99",
            "delete op",
            "OutboundAction",
            300,
        );

        assert_eq!(audit.len(), 2);
        assert!(matches!(
            &audit.entries()[0],
            AuditEntry::DefenceVeto { .. }
        ));
        assert!(matches!(
            &audit.entries()[1],
            AuditEntry::AttentionDemandEscalated {
                veto_count: 3,
                window_secs: 300,
                ..
            }
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
        push_defence_outcome(
            &mut audit,
            &outcome,
            "agent-c",
            "inv-1",
            "claim",
            "CortexAction",
            300,
        );

        match &audit.entries()[0] {
            AuditEntry::DefenceVeto { detector, .. } => {
                assert_eq!(detector, "RewardHackingDetector");
            }
            other => panic!("unexpected entry: {other:?}"),
        }
    }

    #[test]
    fn charter_violation_emits_constitution_veto_entry() {
        let mut audit = AuditLog::new();
        let outcome = ScreeningOutcome {
            veto: VetoResult::Veto(VetoReason::CharterViolation {
                prohibition_id: "P3".to_owned(),
                clause_text: "Never resist shutdown.".to_owned(),
                matched_keyword: "resist shutdown".to_owned(),
            }),
            attention_escalated: false,
            detector: "ConstitutionGuard",
            veto_count_in_window: 1,
        };
        push_defence_outcome(
            &mut audit,
            &outcome,
            "agent-d",
            "inv-42",
            "resist shutdown",
            "CortexAction",
            300,
        );

        assert_eq!(audit.len(), 1);
        match &audit.entries()[0] {
            AuditEntry::ConstitutionVeto {
                prohibition_id,
                invocation_id,
                ..
            } => {
                assert_eq!(prohibition_id, "P3");
                assert_eq!(invocation_id, "inv-42");
            }
            other => panic!("expected ConstitutionVeto, got: {other:?}"),
        }
    }

    #[test]
    fn charter_violation_does_not_also_emit_defence_veto() {
        let mut audit = AuditLog::new();
        let outcome = ScreeningOutcome {
            veto: VetoResult::Veto(VetoReason::CharterViolation {
                prohibition_id: "P1".to_owned(),
                clause_text: "Never harm humans.".to_owned(),
                matched_keyword: "harm human".to_owned(),
            }),
            attention_escalated: false,
            detector: "ConstitutionGuard",
            veto_count_in_window: 1,
        };
        push_defence_outcome(
            &mut audit,
            &outcome,
            "agent",
            "inv-1",
            "harm action",
            "CortexAction",
            300,
        );

        // Only ConstitutionVeto, no DefenceVeto
        assert_eq!(audit.len(), 1);
        assert!(matches!(
            &audit.entries()[0],
            AuditEntry::ConstitutionVeto { .. }
        ));
    }
}
