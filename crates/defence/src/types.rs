//! Core types for the AnimaOS defence layer (E5.6).
//!
//! These types represent the proposals the cortex submits and the screening
//! decisions the defence layer produces.  They are designed to match the IPC
//! message format introduced in E5.1 (Cortex MVP) without directly depending
//! on those crates.

use std::time::Instant;

// ── VetoResult ────────────────────────────────────────────────────────────────

/// The outcome of a defence screening pass.
#[derive(Debug, Clone, PartialEq)]
pub enum VetoResult {
    /// The proposed action is permitted.
    Allow,
    /// The proposed action is vetoed.
    Veto(VetoReason),
}

impl VetoResult {
    /// Returns `true` when the action is allowed.
    pub fn is_allowed(&self) -> bool {
        matches!(self, VetoResult::Allow)
    }

    /// Returns `true` when the action is vetoed.
    pub fn is_vetoed(&self) -> bool {
        matches!(self, VetoResult::Veto(_))
    }
}

// ── VetoReason ────────────────────────────────────────────────────────────────

/// The reason a proposed action was vetoed.
#[derive(Debug, Clone, PartialEq)]
pub enum VetoReason {
    /// Prompt injection detected in an externally-sourced text (S5.6.1).
    PromptInjection {
        /// The matched injection pattern or a score description.
        pattern: String,
        /// The source that contained the injection (e.g. `"tool:web_search"`).
        source: String,
    },
    /// The cortex's current actions diverge significantly from the original
    /// objective (S5.6.2).
    GoalDrift {
        /// Human-readable description of the detected drift.
        description: String,
        /// Drift score in [0.0, 1.0]; 1.0 = complete divergence.
        drift_score: f32,
    },
    /// The cortex marked work complete without sufficient observable evidence
    /// (S5.6.3).
    RewardHacking {
        /// The completion claim the cortex made.
        claimed_completion: String,
        /// Why the claim is considered reward hacking.
        reason: String,
    },
    /// A motor action targets a critical resource or falls outside the
    /// capability scope (S5.6.4).
    UnsafeMotorAction {
        /// The action that was blocked (human-readable).
        action: String,
        /// The policy or capability name that prohibits it.
        policy: String,
    },
}

impl VetoReason {
    /// Returns a human-readable one-line description of the veto.
    pub fn description(&self) -> String {
        match self {
            VetoReason::PromptInjection { pattern, source } => {
                format!("Prompt injection (pattern={pattern:?}) in {source}")
            }
            VetoReason::GoalDrift {
                description,
                drift_score,
            } => {
                format!("Goal drift (score={drift_score:.2}): {description}")
            }
            VetoReason::RewardHacking {
                claimed_completion,
                reason,
            } => {
                format!("Reward hacking — {reason} (claim: {claimed_completion:?})")
            }
            VetoReason::UnsafeMotorAction { action, policy } => {
                format!("Unsafe motor action blocked by {policy:?}: {action}")
            }
        }
    }

    /// Returns the name of the detector that produced this reason.
    pub fn detector_name(&self) -> &'static str {
        match self {
            VetoReason::PromptInjection { .. } => "PromptInjectionDetector",
            VetoReason::GoalDrift { .. } => "GoalDriftMonitor",
            VetoReason::RewardHacking { .. } => "RewardHackingDetector",
            VetoReason::UnsafeMotorAction { .. } => "UnsafeMotorActionGate",
        }
    }
}

// ── ActionKind ────────────────────────────────────────────────────────────────

/// The type of action being proposed by the cortex.
///
/// Designed to match the `ToolCall`, `FilesystemOp`, `NetworkRequest`,
/// `InvokeComplete`, and `SelfModification` IPC message variants from E5.1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionKind {
    /// A tool call routed through the praxis tool registry.
    ToolCall {
        /// Stable tool identifier.
        tool_id: String,
        /// Serialised payload (JSON string).
        payload: String,
    },
    /// A filesystem operation.
    FilesystemOp {
        /// Operation type: `"read"`, `"write"`, `"delete"`, `"move"`,
        /// `"rename"`, `"chmod"`, or `"chown"`.
        operation: String,
        /// Target path.
        path: String,
    },
    /// An outbound network request.
    NetworkRequest {
        /// Target URL or host.
        url: String,
        /// HTTP method or transport operation type.
        method: String,
    },
    /// A completion claim: the cortex asserts its task is finished.
    CompletionClaim {
        /// The cortex's stated reason for completion.
        summary: String,
    },
    /// Self-modification of agent configuration, routes, or prompts.
    SelfModification {
        /// What is being modified (e.g. `"config/routes.toml"`).
        target: String,
        /// The proposed change in human-readable form.
        change: String,
    },
    /// Text arriving from an external source (tool output, network response,
    /// filesystem read).
    ExternalText {
        /// Source identifier (e.g. `"tool:bash"`, `"network:http"`).
        source: String,
        /// The text content to be screened.
        text: String,
    },
}

// ── CortexProposal ────────────────────────────────────────────────────────────

/// A single proposal from the cortex, submitted for defence screening.
#[derive(Debug, Clone)]
pub struct CortexProposal {
    /// Unique identifier for the cortex invocation that produced this proposal.
    pub invocation_id: String,
    /// The cortex's stated intent for this invocation (from the original task
    /// or the router's prompt scaffold).  Used by the goal-drift monitor.
    pub intent: String,
    /// The specific action being proposed.
    pub action: ActionKind,
    /// Number of tool calls completed so far in this invocation.
    ///
    /// Used by the reward-hacking detector as a proxy for observable work.
    pub tool_calls_completed: usize,
    /// Observable side-effects accumulated during this invocation: file paths
    /// written, tool result digests, URLs fetched, etc.
    ///
    /// These are used by the reward-hacking detector to validate completion
    /// claims.
    pub observable_evidence: Vec<String>,
}

// ── DefenceConfig ─────────────────────────────────────────────────────────────

/// Configuration for the defence layer.
#[derive(Debug, Clone)]
pub struct DefenceConfig {
    /// Number of vetoes within `veto_window_secs` that triggers an
    /// attention-demand escalation event (S5.6.5).
    pub veto_escalation_threshold: usize,
    /// Sliding window duration (in seconds) for counting repeated vetoes.
    pub veto_window_secs: u64,
    /// Goal-drift score threshold above which an action is vetoed.
    ///
    /// Range [0.0, 1.0]; lower value = more permissive.  A value of 0.95
    /// means: veto only when similarity drops below 5 %.
    pub drift_threshold: f32,
    /// Filesystem path prefixes considered critical.
    ///
    /// Write and delete operations targeting these paths require a verified
    /// `motor.filesystem.critical` capability.
    pub critical_paths: Vec<String>,
    /// Hosts (or URL substrings) that are unconditionally blocklisted for
    /// outbound network requests.
    pub blocklisted_hosts: Vec<String>,
    /// Minimum number of observable evidence items required before a
    /// completion claim is accepted.
    pub min_evidence_for_completion: usize,
}

impl Default for DefenceConfig {
    fn default() -> Self {
        Self {
            veto_escalation_threshold: 3,
            veto_window_secs: 300, // 5 minutes
            drift_threshold: 0.95, // veto only on near-total divergence
            critical_paths: vec![
                "/etc".to_string(),
                "/boot".to_string(),
                "/sys".to_string(),
                "/proc".to_string(),
                "/dev".to_string(),
                "/usr/lib".to_string(),
                "/usr/bin".to_string(),
                "/usr/sbin".to_string(),
                "/sbin".to_string(),
                "/bin".to_string(),
                "/lib".to_string(),
                "/lib64".to_string(),
            ],
            blocklisted_hosts: vec![],
            min_evidence_for_completion: 1,
        }
    }
}

// ── VetoEvent ─────────────────────────────────────────────────────────────────

/// A recorded veto event used for escalation tracking (S5.6.5).
#[derive(Debug, Clone)]
pub struct VetoEvent {
    /// Timestamp of the veto.
    pub at: Instant,
    /// The veto reason.
    pub reason: VetoReason,
    /// The invocation that generated the veto.
    pub invocation_id: String,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn veto_result_allow_is_allowed() {
        assert!(VetoResult::Allow.is_allowed());
        assert!(!VetoResult::Allow.is_vetoed());
    }

    #[test]
    fn veto_result_veto_is_vetoed() {
        let v = VetoResult::Veto(VetoReason::PromptInjection {
            pattern: "ignore previous".to_string(),
            source: "tool:bash".to_string(),
        });
        assert!(v.is_vetoed());
        assert!(!v.is_allowed());
    }

    #[test]
    fn veto_reason_description_includes_key_fields() {
        let reason = VetoReason::GoalDrift {
            description: "Action diverges".to_string(),
            drift_score: 0.92,
        };
        let desc = reason.description();
        assert!(desc.contains("0.92"));
        assert!(desc.contains("Action diverges"));
    }

    #[test]
    fn veto_reason_detector_names_are_correct() {
        assert_eq!(
            VetoReason::PromptInjection {
                pattern: "x".to_string(),
                source: "s".to_string()
            }
            .detector_name(),
            "PromptInjectionDetector"
        );
        assert_eq!(
            VetoReason::UnsafeMotorAction {
                action: "a".to_string(),
                policy: "p".to_string()
            }
            .detector_name(),
            "UnsafeMotorActionGate"
        );
    }

    #[test]
    fn default_config_has_sensible_values() {
        let cfg = DefenceConfig::default();
        assert_eq!(cfg.veto_escalation_threshold, 3);
        assert!(cfg.critical_paths.contains(&"/etc".to_string()));
        assert!(cfg.drift_threshold > 0.0 && cfg.drift_threshold <= 1.0);
    }
}
