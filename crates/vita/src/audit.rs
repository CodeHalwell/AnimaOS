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
///
/// Note: `GateDecision` contains `f32` fields (urgency, novelty, scores, …);
/// therefore the enum derives `PartialEq` only (not `Eq`).
#[derive(Debug, Clone, PartialEq)]
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

    // ── E5.2 Striatal Gate audit entries ──────────────────────────────────────
    /// A Striatal Gate evaluation was performed for a candidate event.
    ///
    /// Written immediately before every cortex invocation (or rejection).
    /// Satisfies E5.2 exit criterion 1: "every cortex invocation is preceded
    /// by a gate decision entry in the audit log; no invocation bypasses the
    /// gate without an explicit override entry."
    GateDecision {
        /// Agent that owns this gate evaluation.
        agent_id: String,
        /// Per-event identifier used for audit correlation.
        event_id: String,
        /// `true` → cortex invoked; `false` → event blocked.
        invoke: bool,
        /// Routing tier selected (`"CheapLocal"` / `"MidTier"` / `"Frontier"`),
        /// or `None` when the event was blocked.
        cost_class: Option<String>,
        // ── Event features (S5.2.1) ──────────────────────────────────────────
        /// Event urgency score (`[0.0, 1.0]`).
        urgency: f32,
        /// Event novelty score (`[0.0, 1.0]`).
        novelty: f32,
        /// `true` when the event is user-facing.
        user_facing: bool,
        /// String representation of the semantic class.
        semantic_class: String,
        // ── Computed values ───────────────────────────────────────────────────
        /// Value score computed from the event features.
        value_score: f32,
        /// Adaptive threshold the score was tested against.
        threshold_applied: f32,
        // ── Homeostatic signals (S5.2.1) ──────────────────────────────────────
        /// CPU/GPU thermal occupancy at the time of evaluation.
        thermal_load: f32,
        /// Compute-pipeline saturation at the time of evaluation.
        compute_pressure: f32,
        /// Working-memory fill fraction at the time of evaluation.
        memory_pressure: f32,
        /// Available power budget fraction at the time of evaluation.
        power_budget: f32,
        /// Remaining financial API budget fraction at the time of evaluation.
        financial_budget: f32,
        /// User attention level at the time of evaluation.
        attention_demand: f32,
        // ── Decision metadata ─────────────────────────────────────────────────
        /// Human-readable reasoning string surfaced by `anima why`.
        reasoning: String,
        /// `true` when a `GateOverride` changed the normal gate outcome.
        override_active: bool,
    },

    // ── E5.3 Thalamic Router audit entries ────────────────────────────────────
    /// A Thalamic Router decision was made for a gated event.
    ///
    /// Written immediately after a `GateDecision` with `invoke=true`, recording
    /// which route configuration was selected and how tools were filtered.
    /// Satisfies E5.3 exit criterion 1: every invocation has a traceable
    /// route selection in the audit log.
    RouterDecision {
        /// Agent that owns this routing decision.
        agent_id: String,
        /// Per-event identifier for audit correlation (matches `GateDecision`).
        event_id: String,
        /// Identifier of the selected route (e.g. `"cheap-local"`).
        route_id: String,
        /// Model selector tier label (e.g. `"mid-tier"`).
        model_selector: String,
        /// Human-readable tool scope name.
        tool_scope_name: String,
        /// Number of tools offered to the router before scoping.
        tools_available: usize,
        /// Number of tools the cortex will see after route scoping.
        tools_permitted: usize,
        /// Whether identity memory is accessible on this route.
        memory_scope_identity: bool,
        /// Whether L1 working memory is accessible on this route.
        memory_scope_l1: bool,
        /// Whether L2 warm cache is accessible on this route.
        memory_scope_l2: bool,
        /// Whether L3 archive is accessible on this route.
        memory_scope_l3: bool,
        /// Maximum planning + acting turns for this invocation.
        max_turns: u32,
        /// Maximum total tool calls for this invocation.
        max_tool_calls: u32,
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
