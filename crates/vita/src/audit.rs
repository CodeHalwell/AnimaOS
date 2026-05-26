//! Per-agent lifecycle audit trail.
//!
//! The audit log is the end-of-pipeline observability surface called out in the
//! Phase 1 roadmap exit criteria: every task that traverses senses → vita →
//! scheduler → backend must leave a trace here.
//!
//! # E3.4 additions
//!
//! Sleep-maintenance phase entries ([`AuditEntry::SleepPhaseStarted`] and
//! [`AuditEntry::SleepPhaseCompleted`]) were added to support audited end-to-end
//! tracing of each sleep cycle (exit criterion 1 of E3.4).

/// A single observable lifecycle event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditEntry {
    /// A new task was pulled from the agenda and dispatched.
    TaskStarted {
        agent_id: String,
        task_id: u64,
        tier: u8,
        prompt: String,
    },
    /// The backend returned a complete streamed response.
    TaskCompleted {
        agent_id: String,
        task_id: u64,
        tokens_emitted: u32,
        response: String,
    },
    /// The backend returned an error or the stream was cancelled.
    TaskFailed {
        agent_id: String,
        task_id: u64,
        error: String,
    },
    /// Lifecycle transitioned into the sleep state.
    SleepEntered { agent_id: String },
    /// Lifecycle transitioned (back) into the waking state.
    WakeEntered { agent_id: String },
    /// A sleep-maintenance phase was started.
    ///
    /// Always followed by a matching [`AuditEntry::SleepPhaseCompleted`] for
    /// the same `(agent_id, phase)` pair.
    SleepPhaseStarted {
        /// Agent that owns the sleep cycle.
        agent_id: String,
        /// Human-readable phase name (e.g. `"MemoryPruning"`).
        phase: String,
    },
    /// A sleep-maintenance phase finished.
    ///
    /// Paired with a preceding [`AuditEntry::SleepPhaseStarted`].
    SleepPhaseCompleted {
        /// Agent that owns the sleep cycle.
        agent_id: String,
        /// Phase name matching the corresponding `SleepPhaseStarted` entry.
        phase: String,
        /// `true` when the phase completed without rollback or error.
        success: bool,
    },
    // ── E5.1 Cortex MVP audit entries ─────────────────────────────────────────
    /// The cortex was successfully invoked and made its first tool action.
    ///
    /// Satisfies E5.1 exit criterion 3: "end-to-end latency from sensory
    /// packet to first cortex tool action is logged."
    CortexInvoked {
        /// Per-invocation identifier for audit correlation.
        task_id: String,
        /// Duration from invocation start to the cortex's first tool action (ms).
        latency_to_first_action_ms: u64,
    },
    /// The cortex completed an invocation successfully.
    CortexCompleted {
        /// Per-invocation identifier.
        task_id: String,
        /// Number of tool calls the cortex made.
        tool_calls: usize,
        /// Length of the episode summary string (bytes).
        summary_len: usize,
    },
    /// The cortex process crashed or reported an unrecoverable error.
    ///
    /// Satisfies E5.1 exit criterion 2: "cortex crashes do not bring down
    /// vita; the audit log records the crash."
    CortexFault {
        /// Per-invocation identifier.
        task_id: String,
        /// Error message from the cortex (or from vita's process monitor).
        error: String,
    },
    // ── E5.6 — Defence Layer ──────────────────────────────────────────────────
    /// The defence layer vetoed a cortex proposal (S5.6.5).
    ///
    /// Logged at a higher severity than routine audit entries.  Callers
    /// integrating the `defence` crate emit this entry when
    /// [`defence::ScreeningOutcome::is_vetoed`] returns `true`.
    DefenceVeto {
        /// Agent identifier.
        agent_id: String,
        /// Cortex invocation that produced the vetoed proposal.
        invocation_id: String,
        /// Name of the detector that produced the veto (e.g.
        /// `"PromptInjectionDetector"`).
        detector: String,
        /// Human-readable description of the blocked action.
        action_blocked: String,
        /// Human-readable veto reason.
        reason: String,
    },
    /// Repeated vetoes within the configured window triggered an
    /// attention-demand escalation for the user (S5.6.5).
    AttentionDemandEscalated {
        /// Agent identifier.
        agent_id: String,
        /// Cortex invocation that pushed the veto count over the threshold.
        invocation_id: String,
        /// Number of vetoes counted in the window at the time of escalation.
        veto_count: usize,
        /// The configured window duration in seconds.
        window_secs: u64,
    },
}

/// Append-only audit log.
#[derive(Debug, Default, Clone)]
pub struct AuditLog {
    entries: Vec<AuditEntry>,
}

impl AuditLog {
    /// Creates an empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends an entry.
    pub fn push(&mut self, entry: AuditEntry) {
        self.entries.push(entry);
    }

    /// Borrows the full entry sequence.
    pub fn entries(&self) -> &[AuditEntry] {
        &self.entries
    }

    /// Returns the number of recorded entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true when no entries have been recorded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
