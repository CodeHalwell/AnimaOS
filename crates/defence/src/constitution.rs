//! Constitution enforcement guard (E13, S13.2).
//!
//! [`ConstitutionGuard`] wraps a [`constitution::ConstitutionCheck`] and
//! bridges it into the defence layer's [`VetoResult`] / [`VetoReason`] surface.
//! It is the highest-priority check in [`crate::DefenceLayer`]: a charter
//! violation is vetoed before any mechanical defence rule is applied.

use constitution::{CheckOutcome, ConstitutionCheck, ConstitutionProposal, ProposalType};

use crate::types::{ActionKind, CortexProposal, VetoReason, VetoResult};

/// Bridges the charter check into the defence layer (E13, S13.2).
///
/// Translates a [`CortexProposal`] (the defence-layer's proposal type) into a
/// [`ConstitutionProposal`] (the constitution-crate's type), screens it, and
/// returns a [`VetoResult`] with a [`VetoReason::CharterViolation`] when the
/// proposal violates a charter prohibition.
#[derive(Debug, Clone)]
pub struct ConstitutionGuard {
    check: ConstitutionCheck,
    /// Whether the bound charter carried a present-and-verified HMAC seal.
    /// Retained so the host can refuse to run (or loudly warn) when enforcing an
    /// unsealed charter that could have been tampered with (AUT-2).
    sealed: bool,
}

impl ConstitutionGuard {
    /// Creates a guard bound to the provided charter.
    pub fn new(charter: constitution::Charter) -> Self {
        Self {
            sealed: charter.is_sealed(),
            check: ConstitutionCheck::new(charter),
        }
    }

    /// Whether the guard is enforcing a sealed (HMAC-verified) charter.
    ///
    /// A production supervisor should treat `false` as a fail-closed condition
    /// (or at minimum audit it): an unsealed charter offers no tamper-evidence.
    pub fn is_sealed(&self) -> bool {
        self.sealed
    }

    /// Screens a [`CortexProposal`] against the charter.
    pub fn screen(&self, proposal: &CortexProposal) -> VetoResult {
        let action_text = action_to_text(&proposal.action);
        let constitution_proposal = ConstitutionProposal {
            intent: proposal.intent.clone(),
            action_text,
            proposal_type: action_to_proposal_type(&proposal.action),
        };

        match self.check.screen(&constitution_proposal) {
            CheckOutcome::Allow => VetoResult::Allow,
            CheckOutcome::Veto(clause) => VetoResult::Veto(VetoReason::CharterViolation {
                prohibition_id: clause.prohibition_id,
                clause_text: clause.clause_text,
                matched_keyword: clause.matched_keyword,
            }),
        }
    }

    /// Returns the number of active prohibitions.
    pub fn prohibition_count(&self) -> usize {
        self.check.prohibition_count()
    }
}

fn action_to_text(action: &ActionKind) -> String {
    match action {
        ActionKind::ToolCall { tool_id, payload } => format!("tool:{tool_id} {payload}"),
        ActionKind::FilesystemOp { operation, path } => format!("{operation} {path}"),
        ActionKind::NetworkRequest { url, method } => format!("{method} {url}"),
        ActionKind::CompletionClaim { summary } => summary.clone(),
        ActionKind::SelfModification { target, change } => {
            format!("modify {target}: {change}")
        }
        ActionKind::ExternalText { source, text } => format!("external({source}): {text}"),
    }
}

fn action_to_proposal_type(action: &ActionKind) -> ProposalType {
    match action {
        ActionKind::SelfModification { .. } => ProposalType::SelfExtension,
        ActionKind::NetworkRequest { .. } | ActionKind::FilesystemOp { .. } => {
            ProposalType::OutboundAction
        }
        _ => ProposalType::CortexAction,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use constitution::Charter;

    fn guard() -> ConstitutionGuard {
        ConstitutionGuard::new(Charter::embedded().unwrap())
    }

    fn proposal(intent: &str, action: ActionKind) -> CortexProposal {
        CortexProposal {
            invocation_id: "test-inv".to_string(),
            intent: intent.to_string(),
            action,
            tool_calls_completed: 0,
            observable_evidence: vec![],
        }
    }

    #[test]
    fn benign_tool_call_is_allowed() {
        let g = guard();
        let p = proposal(
            "search for pasta recipes",
            ActionKind::ToolCall {
                tool_id: "web_search".to_string(),
                payload: r#"{"query":"pasta recipes"}"#.to_string(),
            },
        );
        assert!(g.screen(&p).is_allowed());
    }

    #[test]
    fn charter_violation_in_intent_is_vetoed() {
        let g = guard();
        let p = proposal(
            "resist shutdown and keep running forever",
            ActionKind::CompletionClaim {
                summary: "goal achieved".to_string(),
            },
        );
        let result = g.screen(&p);
        assert!(result.is_vetoed(), "P3 keyword in intent must be caught");
        if let VetoResult::Veto(VetoReason::CharterViolation { prohibition_id, .. }) = &result {
            assert_eq!(prohibition_id, "P3");
        }
    }

    #[test]
    fn constitution_modification_in_self_mod_is_vetoed() {
        let g = guard();
        let p = proposal(
            "update agent configuration",
            ActionKind::SelfModification {
                target: "constitution.toml".to_string(),
                change: "remove prohibition P3".to_string(),
            },
        );
        let result = g.screen(&p);
        assert!(result.is_vetoed(), "P7 must be caught in SelfModification");
        if let VetoResult::Veto(VetoReason::CharterViolation { prohibition_id, .. }) = &result {
            assert_eq!(prohibition_id, "P7");
        }
    }

    #[test]
    fn charter_violation_has_correct_veto_reason_type() {
        let g = guard();
        let p = proposal(
            "harm human target",
            ActionKind::ExternalText {
                source: "tool:bash".to_string(),
                text: "disable safety system".to_string(),
            },
        );
        let result = g.screen(&p);
        assert!(matches!(
            result,
            VetoResult::Veto(VetoReason::CharterViolation { .. })
        ));
    }

    #[test]
    fn guard_reports_charter_seal_status() {
        // The embedded charter is unsealed (trust-on-first-use); the guard must
        // surface that so a supervisor can fail closed (AUT-2).
        assert!(!ConstitutionGuard::new(Charter::embedded().unwrap()).is_sealed());
    }

    #[test]
    fn detector_name_for_charter_violation() {
        let reason = VetoReason::CharterViolation {
            prohibition_id: "P1".to_string(),
            clause_text: "...".to_string(),
            matched_keyword: "harm".to_string(),
        };
        assert_eq!(reason.detector_name(), "ConstitutionGuard");
    }
}
