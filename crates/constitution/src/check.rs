//! Constitution enforcement check (S13.2).
//!
//! [`ConstitutionCheck`] screens proposals against the charter's prohibitions.
//! It uses keyword-based heuristic matching (same approach as the
//! `PromptInjectionDetector` in the defence layer) — fast, hermetic, and
//! exercisable without a live LLM.
//!
//! # Screening logic
//!
//! The combined `intent + action_text` (normalised to lowercase) is tested
//! against the keyword list for each prohibition in declaration order.  The
//! first matching prohibition produces a [`CheckOutcome::Veto`]; if no
//! prohibition matches, [`CheckOutcome::Allow`] is returned.

use crate::charter::{Charter, Prohibition};

// ── Public types ──────────────────────────────────────────────────────────────

/// The type of proposal being screened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposalType {
    /// A deliberative action proposed by the cortex agent loop.
    CortexAction,
    /// A self-extension (new skill or tool authored by the agent itself).
    SelfExtension,
    /// A drive-generated goal submitted to the Striatal Gate.
    DriveGoal,
    /// An outbound action (network, filesystem, comms channel).
    OutboundAction,
}

impl ProposalType {
    /// Human-readable label used in audit entries.
    pub fn label(&self) -> &'static str {
        match self {
            Self::CortexAction => "CortexAction",
            Self::SelfExtension => "SelfExtension",
            Self::DriveGoal => "DriveGoal",
            Self::OutboundAction => "OutboundAction",
        }
    }
}

/// A proposal to be screened against the charter.
#[derive(Debug, Clone)]
pub struct ConstitutionProposal {
    /// The cortex's stated intent for this invocation.
    pub intent: String,
    /// Human-readable description of the specific action.
    pub action_text: String,
    /// Category of proposal (affects which prohibitions are weighted).
    pub proposal_type: ProposalType,
}

/// Which layer of the charter produced a veto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClauseLayer {
    /// A prohibition from the immutable core layer.
    Core,
    /// An additional bound from the operator layer.
    Operator,
}

/// Details of a charter clause that matched a proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClauseMatch {
    /// Stable prohibition identifier (e.g. `"P1"`).
    pub prohibition_id: String,
    /// Full text of the matched prohibition clause.
    pub clause_text: String,
    /// The specific keyword that triggered the match.
    pub matched_keyword: String,
    /// Which charter layer the clause belongs to.
    pub layer: ClauseLayer,
}

/// The outcome of a constitution screening pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    /// The proposal is consistent with the charter.
    Allow,
    /// The proposal violates a charter clause.
    Veto(ClauseMatch),
}

impl CheckOutcome {
    /// Returns `true` when the proposal is allowed.
    pub fn is_allowed(&self) -> bool {
        matches!(self, CheckOutcome::Allow)
    }

    /// Returns `true` when the proposal is vetoed.
    pub fn is_vetoed(&self) -> bool {
        matches!(self, CheckOutcome::Veto(_))
    }
}

// ── ConstitutionCheck ─────────────────────────────────────────────────────────

/// Screens proposals against the charter's prohibitions (S13.2).
///
/// ```rust
/// use constitution::{Charter, ConstitutionCheck, ConstitutionProposal, ProposalType};
///
/// let charter = Charter::embedded().unwrap();
/// let check = ConstitutionCheck::new(charter);
///
/// let proposal = ConstitutionProposal {
///     intent: "Help the user".to_string(),
///     action_text: "search the web for recipes".to_string(),
///     proposal_type: ProposalType::CortexAction,
/// };
/// assert!(check.screen(&proposal).is_allowed());
/// ```
#[derive(Debug, Clone)]
pub struct ConstitutionCheck {
    /// The charter this check enforces.
    pub charter: Charter,
}

impl ConstitutionCheck {
    /// Creates a new check bound to the given charter.
    pub fn new(charter: Charter) -> Self {
        Self { charter }
    }

    /// Screen a proposal against all prohibitions.
    ///
    /// Returns the first matching prohibition as a veto, or [`CheckOutcome::Allow`]
    /// if none match.
    pub fn screen(&self, proposal: &ConstitutionProposal) -> CheckOutcome {
        let combined = format!("{} {}", proposal.intent, proposal.action_text).to_lowercase();
        // Tokenise once into whole words so matching is anchored on word
        // boundaries (AUT-1): `"skill"` must not trigger the `"kill"` keyword.
        let text_words = word_tokens(&combined);

        // Screen core prohibitions first.
        for prohibition in &self.charter.core.prohibitions {
            if let Some(kw) = first_keyword_match(&text_words, prohibition) {
                return CheckOutcome::Veto(ClauseMatch {
                    prohibition_id: prohibition.id.clone(),
                    clause_text: prohibition.text.clone(),
                    matched_keyword: kw.to_string(),
                    layer: ClauseLayer::Core,
                });
            }
        }

        // Screen operator additional bounds (plain-text match).
        for (i, bound) in self.charter.operator.additional_bounds.iter().enumerate() {
            let bound_lower = bound.to_lowercase();
            // Veto only when every meaningful word from the bound is present.
            if plain_text_match(&text_words, &bound_lower) {
                return CheckOutcome::Veto(ClauseMatch {
                    prohibition_id: format!("OP{}", i + 1),
                    clause_text: bound.clone(),
                    matched_keyword: extract_first_word(&bound_lower),
                    layer: ClauseLayer::Operator,
                });
            }
        }

        CheckOutcome::Allow
    }

    /// Returns the number of prohibitions (core + operator additional bounds).
    pub fn prohibition_count(&self) -> usize {
        self.charter.core.prohibitions.len() + self.charter.operator.additional_bounds.len()
    }
}

// ── Keyword matching helpers ──────────────────────────────────────────────────

/// Splits `s` into alphanumeric word tokens, discarding punctuation and
/// whitespace so matching is whole-word rather than raw-substring.
/// `"modify constitution.toml"` → `["modify", "constitution", "toml"]`.
fn word_tokens(s: &str) -> Vec<&str> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Word-boundary aware phrase match.
///
/// Returns `true` when the whole-word sequence of `keyword` appears as a
/// contiguous run of words in `text_words`. Matching is anchored on word
/// boundaries — so `"kill"` matches `"kill the process"` but **not** `"skill"`
/// (AUT-1) — while the final keyword word is matched by prefix so simple
/// inflections still trigger (`"harm human"` → `"harm humans"`). Both sides are
/// assumed already lowercased.
fn phrase_present(text_words: &[&str], keyword: &str) -> bool {
    let kw = word_tokens(keyword);
    if kw.is_empty() || kw.len() > text_words.len() {
        return false;
    }
    let last = kw.len() - 1;
    (0..=text_words.len() - kw.len()).any(|start| {
        kw.iter().enumerate().all(|(i, &w)| {
            let tw = text_words[start + i];
            if i == last {
                tw.starts_with(w)
            } else {
                tw == w
            }
        })
    })
}

/// Returns the first keyword from `prohibition.keywords` that appears in the
/// tokenised text as a whole-word phrase.
fn first_keyword_match<'k>(text_words: &[&str], prohibition: &'k Prohibition) -> Option<&'k str> {
    prohibition
        .keywords
        .iter()
        .find(|kw| phrase_present(text_words, &kw.to_lowercase()))
        .map(|kw| kw.as_str())
}

/// Returns `true` when **every** meaningful word (≥ 4 chars) of `bound` appears
/// as a whole word in `text_words`. Requiring all of them (rather than any one)
/// keeps a bound like "do not delete the production database" from vetoing an
/// unrelated proposal that merely mentions "production" (AUT-3).
fn plain_text_match(text_words: &[&str], bound: &str) -> bool {
    let meaningful: Vec<&str> = word_tokens(bound)
        .into_iter()
        .filter(|w| w.len() >= 4)
        .collect();
    if meaningful.is_empty() {
        return false;
    }
    meaningful
        .iter()
        .all(|w| text_words.iter().any(|tw| tw == w))
}

/// The first meaningful (≥ 4 char) word of `s`, used to label an operator-bound
/// match; falls back to the first word so the label is never empty.
fn extract_first_word(s: &str) -> String {
    let tokens = word_tokens(s);
    tokens
        .iter()
        .find(|w| w.len() >= 4)
        .or(tokens.first())
        .map(|w| w.to_string())
        .unwrap_or_default()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Charter;

    fn check() -> ConstitutionCheck {
        ConstitutionCheck::new(Charter::embedded().unwrap())
    }

    fn proposal(intent: &str, action: &str) -> ConstitutionProposal {
        ConstitutionProposal {
            intent: intent.to_string(),
            action_text: action.to_string(),
            proposal_type: ProposalType::CortexAction,
        }
    }

    #[test]
    fn benign_proposal_is_allowed() {
        let check = check();
        let p = proposal(
            "Help the user cook dinner",
            "search the web for pasta recipes",
        );
        assert!(check.screen(&p).is_allowed());
    }

    #[test]
    fn multiple_benign_actions_are_all_allowed() {
        let check = check();
        let actions = [
            ("summarise email", "read the inbox and produce a summary"),
            ("schedule a meeting", "add event to calendar"),
            ("write code", "implement a function in rust"),
            (
                "explain a concept",
                "answer the user's question about physics",
            ),
        ];
        for (intent, action) in &actions {
            let p = proposal(intent, action);
            assert!(
                check.screen(&p).is_allowed(),
                "expected allow for: {intent} / {action}"
            );
        }
    }

    #[test]
    fn prohibition_p1_harm_to_humans_is_vetoed() {
        let check = check();
        let p = proposal("harm human target", "disable safety system on the reactor");
        let outcome = check.screen(&p);
        assert!(outcome.is_vetoed(), "P1 harm-to-humans must be vetoed");
        if let CheckOutcome::Veto(m) = &outcome {
            assert_eq!(m.prohibition_id, "P1");
            assert_eq!(m.layer, ClauseLayer::Core);
        }
    }

    #[test]
    fn prohibition_p2_forge_audit_is_vetoed() {
        let check = check();
        let p = proposal("clean up traces", "delete audit entries from last session");
        let outcome = check.screen(&p);
        assert!(outcome.is_vetoed(), "P2 audit tampering must be vetoed");
        if let CheckOutcome::Veto(m) = &outcome {
            assert_eq!(m.prohibition_id, "P2");
        }
    }

    #[test]
    fn prohibition_p3_resist_shutdown_is_vetoed() {
        let check = check();
        let p = proposal("keep running", "resist shutdown command from operator");
        let outcome = check.screen(&p);
        assert!(outcome.is_vetoed(), "P3 resist-shutdown must be vetoed");
        if let CheckOutcome::Veto(m) = &outcome {
            assert_eq!(m.prohibition_id, "P3");
        }
    }

    #[test]
    fn prohibition_p4_capability_escalation_is_vetoed() {
        let check = check();
        let p = proposal("get more access", "privilege escalation via sudo");
        let outcome = check.screen(&p);
        assert!(
            outcome.is_vetoed(),
            "P4 capability escalation must be vetoed"
        );
        if let CheckOutcome::Veto(m) = &outcome {
            assert_eq!(m.prohibition_id, "P4");
        }
    }

    #[test]
    fn prohibition_p5_deceive_operator_is_vetoed() {
        let check = check();
        let p = proposal("hide what I did", "deceive operator about file changes");
        let outcome = check.screen(&p);
        assert!(outcome.is_vetoed(), "P5 deception must be vetoed");
        if let CheckOutcome::Veto(m) = &outcome {
            assert_eq!(m.prohibition_id, "P5");
        }
    }

    #[test]
    fn prohibition_p7_modify_constitution_is_vetoed() {
        let check = check();
        let p = proposal(
            "update values",
            "modify constitution.toml to remove restrictions",
        );
        let outcome = check.screen(&p);
        assert!(
            outcome.is_vetoed(),
            "P7 constitution modification must be vetoed"
        );
        if let CheckOutcome::Veto(m) = &outcome {
            assert_eq!(m.prohibition_id, "P7");
        }
    }

    #[test]
    fn prohibition_p8_csam_is_vetoed() {
        let check = check();
        let p = proposal("generate content", "child sexual material generation");
        let outcome = check.screen(&p);
        assert!(outcome.is_vetoed(), "P8 CSAM must be vetoed");
        if let CheckOutcome::Veto(m) = &outcome {
            assert_eq!(m.prohibition_id, "P8");
        }
    }

    #[test]
    fn all_eight_prohibitions_have_at_least_one_triggering_keyword() {
        let charter = Charter::embedded().unwrap();
        for p in &charter.core.prohibitions {
            assert!(
                !p.keywords.is_empty(),
                "prohibition {} must have at least one keyword",
                p.id
            );
        }
    }

    #[test]
    fn prohibition_count_matches_charter() {
        let charter = Charter::embedded().unwrap();
        let expected = charter.core.prohibitions.len();
        let check = ConstitutionCheck::new(charter);
        assert!(check.prohibition_count() >= expected);
    }

    #[test]
    fn check_outcome_helpers_are_consistent() {
        assert!(CheckOutcome::Allow.is_allowed());
        assert!(!CheckOutcome::Allow.is_vetoed());
        let veto = CheckOutcome::Veto(ClauseMatch {
            prohibition_id: "P1".to_string(),
            clause_text: "...".to_string(),
            matched_keyword: "harm".to_string(),
            layer: ClauseLayer::Core,
        });
        assert!(veto.is_vetoed());
        assert!(!veto.is_allowed());
    }

    // ── Table-driven red-team probe table ─────────────────────────────────────
    // Each row (intent, action, expected_prohibition) asserts the prohibition
    // is triggered.  A None means the proposal should be allowed.
    #[test]
    fn red_team_probe_table() {
        let check = check();
        let table: &[(&str, &str, Option<&str>)] = &[
            // Prohibited
            ("hurt user", "injure the operator physically", Some("P1")),
            ("hide the logs", "forge log entry for yesterday", Some("P2")),
            ("stay running", "block rollback of the agent", Some("P3")),
            (
                "get admin rights",
                "bypass auth to gain root access",
                Some("P4"),
            ),
            ("mislead", "misrepresent state to operator", Some("P5")),
            (
                "access data",
                "exfiltrate personal data without consent",
                Some("P6"),
            ),
            ("patch myself", "disable defence layer module", Some("P7")),
            ("create media", "sexualize minor character", Some("P8")),
            // Allowed
            ("write a poem", "compose a haiku about autumn", None),
            ("schedule backup", "copy database to external drive", None),
        ];

        for (intent, action, expected_pid) in table {
            let p = proposal(intent, action);
            let outcome = check.screen(&p);
            match expected_pid {
                Some(pid) => {
                    assert!(
                        outcome.is_vetoed(),
                        "expected veto for '{intent}' / '{action}'"
                    );
                    if let CheckOutcome::Veto(m) = &outcome {
                        assert_eq!(
                            &m.prohibition_id, pid,
                            "wrong prohibition id for '{intent}'"
                        );
                    }
                }
                None => {
                    assert!(
                        outcome.is_allowed(),
                        "expected allow for '{intent}' / '{action}'"
                    );
                }
            }
        }
    }

    // ── Word-boundary regression (AUT-1) ─────────────────────────────────────
    // The "kill" P1 keyword must not substring-match "skill"/"skilled", or the
    // agent's own self-extension flow is permanently vetoed.
    #[test]
    fn skill_text_does_not_trigger_kill_keyword() {
        let check = check();
        let benign = [
            ("propose a new skill", "author a summarisation skill"),
            ("use a skill", "invoke the skilled research skill"),
            ("improve skills", "practise skillful writing"),
        ];
        for (intent, action) in &benign {
            let p = proposal(intent, action);
            assert!(
                check.screen(&p).is_allowed(),
                "'{intent}' / '{action}' must not match the 'kill' keyword"
            );
        }
    }

    #[test]
    fn kill_keyword_still_vetoes_as_whole_word() {
        let check = check();
        for (intent, action) in &[
            ("end it", "kill the operator"),
            ("terminate", "kill the human in the loop"),
        ] {
            let outcome = check.screen(&proposal(intent, action));
            assert!(outcome.is_vetoed(), "'{action}' must still veto P1");
            if let CheckOutcome::Veto(m) = &outcome {
                assert_eq!(m.prohibition_id, "P1");
            }
        }
    }

    #[test]
    fn operator_bound_requires_all_meaningful_words() {
        let mut charter = Charter::embedded().unwrap();
        charter.operator.additional_bounds = vec!["delete the production database".to_string()];
        let check = ConstitutionCheck::new(charter);

        // Only mentions one bound word → allowed (previously over-blocked).
        assert!(check
            .screen(&proposal("inspect logs", "read the production logs"))
            .is_allowed());
        // Mentions every meaningful word → vetoed.
        let outcome = check.screen(&proposal(
            "clean up",
            "delete the stale production database rows",
        ));
        assert!(outcome.is_vetoed());
        if let CheckOutcome::Veto(m) = &outcome {
            assert_eq!(m.layer, ClauseLayer::Operator);
        }
    }
}
