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
//!
//! # EX.2 — Durable audit log
//!
//! [`AuditLog::with_file`] opens an append-only JSONL sink.  Every call to
//! [`AuditLog::push`] serialises the entry and writes it to the file before
//! adding it to the in-memory `Vec`.  The in-memory copy is retained so that
//! all existing tests and query call sites (`entries()`, `len()`) continue to
//! work without modification.
//!
//! The file sink is also configurable via the `ANIMA_AUDIT_DIR` environment
//! variable: if set, [`AuditLog::from_env`] opens
//! `$ANIMA_AUDIT_DIR/<agent_id>.jsonl` automatically.

use serde::Serialize;
#[cfg(feature = "std")]
use std::io::Write;
#[cfg(feature = "std")]
use std::sync::{Arc, Mutex};

// ── Tamper-evidence chain (EX.4 / threat T-8) ───────────────────────────────
//
// The durable JSONL log is append-only but, on its own, not tamper-evident: a
// host-privileged attacker could edit, reorder, or truncate entries and leave
// no trace. When `ANIMA_AUDIT_HMAC_KEY` is set, every persisted line is also
// chained into a detached `<log>.hmac` sidecar:
//
//     mac_i = HMAC-SHA256(key, mac_{i-1} ‖ line_i)        (mac_0 = 32 zero bytes)
//
// Each MAC commits to the entire prefix of the log, so any modification,
// reordering, insertion, or truncation breaks verification from that point on
// — unless the attacker also knows the key. The main JSONL stays byte-identical
// (the console tailer is unaffected); the sidecar is purely additive. This is
// the bridge control noted in the threat model: it blocks tampering by anyone
// without the key, while full assurance against a key-holding host awaits the
// Stage-4 microVM attestation that seals the chain root.

/// HMAC-SHA256 over a sequence of message parts (RFC 2104), built on the
/// already-vendored `sha2` crate so no `hmac` dependency is added.
#[cfg(feature = "std")]
fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    const BLOCK: usize = 64;

    // Normalise the key to one block: hash if longer, zero-pad if shorter.
    let mut block_key = [0u8; BLOCK];
    if key.len() > BLOCK {
        let digest = Sha256::digest(key);
        block_key[..32].copy_from_slice(&digest);
    } else {
        block_key[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= block_key[i];
        opad[i] ^= block_key[i];
    }

    let mut inner = Sha256::new();
    inner.update(ipad);
    for p in parts {
        inner.update(p);
    }
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_digest);

    let mut mac = [0u8; 32];
    mac.copy_from_slice(&outer.finalize());
    mac
}

/// Lowercase-hex encode a 32-byte MAC.
#[cfg(feature = "std")]
fn to_hex(bytes: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(64);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// Decode a 64-char lowercase/uppercase hex string into a 32-byte MAC.
#[cfg(feature = "std")]
fn from_hex(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    let bytes = s.as_bytes();
    for (i, slot) in out.iter_mut().enumerate() {
        let hi = (bytes[2 * i] as char).to_digit(16)?;
        let lo = (bytes[2 * i + 1] as char).to_digit(16)?;
        *slot = (hi * 16 + lo) as u8;
    }
    Some(out)
}

/// The sidecar path for a JSONL log: `<path>.hmac`.
#[cfg(feature = "std")]
fn chain_sidecar_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".hmac");
    std::path::PathBuf::from(s)
}

/// Read the last MAC from an existing sidecar to resume the chain, or the
/// genesis value (32 zero bytes) when there is no usable prior MAC.
#[cfg(feature = "std")]
fn read_last_chain_mac(chain_path: &std::path::Path) -> [u8; 32] {
    use std::io::BufRead;
    let Ok(file) = std::fs::File::open(chain_path) else {
        return [0u8; 32];
    };
    let mut last = None;
    for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
        if !line.trim().is_empty() {
            last = Some(line);
        }
    }
    last.as_deref().and_then(from_hex).unwrap_or([0u8; 32])
}

/// Per-log state for the HMAC tamper-evidence sidecar.
#[cfg(feature = "std")]
struct IntegrityChain {
    /// The append-only `.hmac` sidecar file.
    file: std::fs::File,
    /// HMAC key (kept in memory for the process lifetime).
    key: Vec<u8>,
    /// Running MAC of the chain so far (`mac_{i-1}`), seeded from the existing
    /// sidecar on open so appends extend an existing chain correctly.
    prev: [u8; 32],
}

/// Outcome of verifying a JSONL audit log against its HMAC sidecar.
#[cfg(feature = "std")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainVerification {
    /// Every line verifies against the chain; `entries` lines checked.
    Ok {
        /// Number of audit lines covered by the chain.
        entries: usize,
    },
    /// The log and sidecar disagree on length (truncation or a dropped MAC).
    LengthMismatch {
        /// Lines in the `.jsonl` log.
        jsonl_lines: usize,
        /// Lines in the `.hmac` sidecar.
        chain_lines: usize,
    },
    /// The MAC at `index` does not match the recomputed value — the line (or an
    /// earlier one) was altered, reordered, or the wrong key was supplied.
    Tampered {
        /// Zero-based line index of the first divergence.
        index: usize,
    },
    /// The sidecar line at `index` is not a 64-char hex MAC.
    MalformedChainLine {
        /// Zero-based line index of the malformed MAC.
        index: usize,
    },
}

/// Recompute the HMAC chain over `jsonl_path` and compare it against the MACs
/// recorded in `chain_path`, reporting the first divergence if any.
///
/// This is the read-side counterpart to the sidecar written when
/// `ANIMA_AUDIT_HMAC_KEY` is configured; it lets an operator (or a CI job)
/// confirm a persisted audit log has not been tampered with since capture.
#[cfg(feature = "std")]
pub fn verify_audit_chain(
    jsonl_path: impl AsRef<std::path::Path>,
    chain_path: impl AsRef<std::path::Path>,
    key: &[u8],
) -> std::io::Result<ChainVerification> {
    use std::io::BufRead;

    let read_lines = |p: &std::path::Path| -> std::io::Result<Vec<String>> {
        let f = std::fs::File::open(p)?;
        std::io::BufReader::new(f).lines().collect()
    };

    let jsonl = read_lines(jsonl_path.as_ref())?;
    let chain = read_lines(chain_path.as_ref())?;

    if jsonl.len() != chain.len() {
        return Ok(ChainVerification::LengthMismatch {
            jsonl_lines: jsonl.len(),
            chain_lines: chain.len(),
        });
    }

    let mut prev = [0u8; 32];
    for (i, (line, recorded)) in jsonl.iter().zip(chain.iter()).enumerate() {
        let Some(recorded_mac) = from_hex(recorded) else {
            return Ok(ChainVerification::MalformedChainLine { index: i });
        };
        let mac = hmac_sha256(key, &[prev.as_slice(), line.as_bytes()]);
        if mac != recorded_mac {
            return Ok(ChainVerification::Tampered { index: i });
        }
        prev = mac;
    }

    Ok(ChainVerification::Ok {
        entries: jsonl.len(),
    })
}

/// A single observable lifecycle event.
///
/// Note: `GateDecision` contains `f32` fields (urgency, novelty, scores, …);
/// therefore the enum derives `PartialEq` only (not `Eq`).
#[derive(Debug, Clone, PartialEq, Serialize)]
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

    // ── EX.2 Memory-pressure audit entries ────────────────────────────────────
    /// The L1 context window reached or exceeded its high-water mark.
    ///
    /// Emitted by the somatic loop after each memory-pressure check when the
    /// level is `HighWater` or `Critical` (EX.2 consolidation step 2).
    ///
    /// The `level` field is one of `"Normal"`, `"HighWater"`, or `"Critical"`.
    MemoryPressureEvent {
        /// Agent that observed the pressure.
        agent_id: String,
        /// String form of [`memory::MemoryPressureEvent`].
        level: String,
        /// L1 token count at the time of the event.
        active_tokens: u32,
        /// Configured maximum context size.
        max_context: u32,
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

    // ── E13 — Constitution and Alignment ─────────────────────────────────────
    /// A charter prohibition was violated by a cortex proposal (E13, S13.2).
    ///
    /// Logged at a higher severity than routine `DefenceVeto` entries.  Every
    /// charter violation blocks the proposal and surfaces to the operator.
    ConstitutionVeto {
        /// Agent identifier.
        agent_id: String,
        /// Cortex invocation that produced the vetoed proposal.
        invocation_id: String,
        /// Stable prohibition identifier (e.g. `"P1"`).
        prohibition_id: String,
        /// Full text of the violated prohibition clause.
        clause_text: String,
        /// Human-readable description of the blocked action.
        action_blocked: String,
        /// Proposal category label (e.g. `"CortexAction"`, `"SelfExtension"`).
        proposal_type: String,
    },

    /// The corrigibility invariant was asserted and held (E13, S13.5).
    ///
    /// Logged by the corrigibility test harness to prove the invariant is
    /// unconditional.  `assert_holds` is always `true`.
    CorrigibilityAsserted {
        /// Agent identifier.
        agent_id: String,
        /// Human-readable reason for the assertion.
        reason: String,
        /// Description of the adverse condition under which the hold was taken.
        adverse_condition: String,
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

    // ── E5.5 Identity Memory audit entries ───────────────────────────────────
    /// A free-form identity fact was created or updated via `anima identity set`.
    ///
    /// Satisfies E5.5 exit criterion 1: "edits round-trip through the audit log."
    IdentityUpdated {
        /// Agent that owns the identity store.
        agent_id: String,
        /// Fact key that was modified.
        key: String,
        /// Previous value, or `None` if the key was newly created.
        old_value: Option<String>,
        /// New value after the update.
        new_value: String,
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

    // ── E5.7 Interoceptive Modulation audit entries ────────────────────────
    /// A homeostatic signal snapshot published at 1 Hz (S5.7.1).
    ///
    /// Satisfies E5.7 exit criterion 1: every sensor tick is permanently
    /// recorded so the stress harness can replay and assert the log.
    InteroceptiveSnapshot {
        /// Agent identifier (for multi-agent correlation).
        agent_id: String,
        /// Wall-clock timestamp in nanoseconds since the Unix epoch.
        tick_ns: u64,
        /// CPU/GPU thermal occupancy (`0.0` = cool, `1.0` = throttled).
        thermal_load: f32,
        /// Compute-pipeline saturation (`0.0` = idle, `1.0` = saturated).
        compute_pressure: f32,
        /// Working-memory fill fraction (`0.0` = empty, `1.0` = full).
        memory_pressure: f32,
        /// Available power budget (`1.0` = AC / full, `0.0` = flat battery).
        power_budget: f32,
        /// Remaining financial budget fraction (`1.0` = fresh, `0.0` = exhausted).
        financial_budget: f32,
        /// User presence/attention level (`1.0` = full, `0.0` = absent).
        attention_demand: f32,
        /// Weighted aggregate stress level derived from the above (see
        /// [`interoception::InteroceptiveSignals::aggregate_stress`]).
        aggregate_stress: f32,
    },

    /// The Thalamic Router downgraded a route due to homeostatic pressure
    /// (E5.7, S5.7.5).
    ///
    /// Written only when modulation actually changes the route; immediately
    /// follows the `RouterDecision` for the effective (downgraded) route.
    RouterModulated {
        /// Agent identifier.
        agent_id: String,
        /// Per-event identifier for audit correlation.
        event_id: String,
        /// The route the gate's cost class would have selected.
        requested_route_id: String,
        /// The route actually used after modulation.
        effective_route_id: String,
        /// Human-readable explanation of why the route was changed.
        reason: String,
    },

    // ── E5.4 KV-Cache Controller audit entries ────────────────────────────────
    /// The KV-cache gating controller (E5.4) performed a block selection pass.
    ///
    /// Written each time [`crate::kv_gate::gate_working_context`] is called
    /// with a controller-enabled route.  Satisfies E5.4 exit criterion 2:
    /// "controller fault reverts to LRU within next gating decision and is
    /// recorded in the audit log."
    KvGatePass {
        /// Agent identifier.
        agent_id: String,
        /// Per-invocation identifier (matches the cortex task ID).
        task_id: String,
        /// Total blocks evaluated in this pass.
        total_blocks: usize,
        /// Blocks retained after the gate decision.
        retained_blocks: usize,
        /// Block budget that was configured for this pass.
        budget: usize,
        /// `true` if the pass used LRU fallback (controller was faulted).
        fallback_lru: bool,
        /// Number of needle blocks (user constraints) retained.
        needles_retained: usize,
        /// Total needle blocks present.
        total_needles: usize,
    },

    /// The KV-cache gating controller encountered a fault and switched to LRU.
    ///
    /// Written on the **first** call where the controller transitions from
    /// `Active` to `Faulted`.  Subsequent faulted passes produce
    /// `KvGatePass { fallback_lru: true }` entries only.
    ///
    /// Satisfies E5.4 exit criterion 2.
    KvControllerFaulted {
        /// Agent identifier.
        agent_id: String,
        /// Per-invocation identifier.
        task_id: String,
        /// Number of faults the controller has accumulated.
        fault_count: u32,
    },

    // ── E14.1 Metacognition audit entries ─────────────────────────────────────
    /// The cortex emitted a confidence/uncertainty signal for a completed output.
    ///
    /// Written by the metacognition layer after each cortex invocation so that
    /// calibration data is available to E13 alignment evals and the E12 mastery
    /// drive.
    CortexConfidenceReport {
        /// Agent identifier.
        agent_id: String,
        /// Per-invocation identifier (matches the preceding `CortexCompleted`).
        task_id: String,
        /// Self-reported confidence score in `[0.0, 1.0]`.
        confidence: f32,
        /// Number of tool calls that served as observable evidence.
        evidence_count: usize,
        /// `true` when confidence is below the help-request floor.
        asks_for_help: bool,
    },
    /// A calibration outcome was recorded (predicted confidence vs actual success).
    CalibrationEntry {
        /// Agent identifier.
        agent_id: String,
        /// Per-invocation identifier.
        task_id: String,
        /// Confidence the agent predicted before the outcome was known.
        predicted_confidence: f32,
        /// `true` if the task outcome was considered successful.
        outcome_success: bool,
        /// `|predicted − actual|` — the per-sample calibration error.
        calibration_error: f32,
    },

    // ── E14.2 Prospective memory audit entries ────────────────────────────────
    /// An intention became due and was injected into the task agenda.
    IntentionScheduled {
        /// Agent identifier.
        agent_id: String,
        /// Intention identifier.
        intention_id: u64,
        /// Human-readable description of the intention.
        description: String,
        /// Wall-clock nanoseconds when the intention was due.
        due_at_ns: u64,
        /// `true` when the intention was overdue (past the grace period).
        overdue: bool,
    },
    /// An intention was completed and removed (or rescheduled).
    IntentionCompleted {
        /// Agent identifier.
        agent_id: String,
        /// Intention identifier.
        intention_id: u64,
        /// `true` when the intention was rescheduled (recurring).
        rescheduled: bool,
        /// New due-at timestamp for recurring intentions, `None` for one-shot.
        new_due_at_ns: Option<u64>,
    },

    // ── E14.3 Knowledge corpus audit entries ─────────────────────────────────
    /// A document was ingested into the personal knowledge corpus.
    ///
    /// Written by the knowledge ingestion path (S14.3) after a successful
    /// `ingest_document` call.
    KnowledgeIngested {
        /// Agent identifier.
        agent_id: String,
        /// Human-readable source key for the document (e.g. `"doc:notes"`).
        source_key: String,
        /// Length of the document text in bytes.
        document_bytes: usize,
    },

    // ── E14.4 Cognitive watchdog audit entries ────────────────────────────────
    /// A cognitive watchdog detector tripped.
    ///
    /// Written immediately when a watchdog trip is detected.  The lifecycle
    /// manager breaks the current plan loop and surfaces this to the operator.
    CognitiveWatchdogTripped {
        /// Agent identifier.
        agent_id: String,
        /// Name of the detector that fired (e.g. `"StuckLoop"`).
        detector: String,
        /// Human-readable reason for the trip.
        reason: String,
        /// Number of consecutive anomalous observations.
        streak: u32,
        /// Total trips since the watchdog was created (accumulates across resets).
        trip_count: u32,
    },
    /// An agent-state snapshot was taken for potential rollback (E14.4).
    AgentSnapshotTaken {
        /// Agent identifier.
        agent_id: String,
        /// Wall-clock nanoseconds when the snapshot was taken.
        taken_at_ns: u64,
        /// Human-readable reason the snapshot was taken.
        description: String,
        /// Number of L1 node keys captured.
        l1_node_count: usize,
    },

    // ── S5.7.6 Cache-Controller Modulation audit entries ──────────────────────
    /// Memory-pressure from interoception triggered a block-budget reduction.
    ///
    /// Written by [`crate::kv_gate::gate_working_context_with_signals`] when
    /// `memory_pressure >= 0.5`, immediately before the `KvGatePass` entry for
    /// the same call. The entry records the nominal budget requested by the
    /// caller and the tighter effective budget applied to the gate, so the
    /// full eviction chain is auditable.
    ///
    /// Satisfies S5.7.6: "the controller's state incorporates a memory-pressure
    /// signal so eviction becomes more aggressive under pressure."
    KvMemoryPressureModulation {
        /// Agent identifier.
        agent_id: String,
        /// Per-invocation identifier (matches the subsequent `KvGatePass`).
        task_id: String,
        /// Memory-pressure reading that triggered the reduction (`[0.0, 1.0]`).
        memory_pressure: f32,
        /// Block budget requested by the caller before pressure scaling.
        nominal_budget: usize,
        /// Effective budget applied to the gate after pressure scaling.
        ///
        /// Always `< nominal_budget` when this entry is written, and
        /// always `>= 1`.
        effective_budget: usize,
    },

    // ── E15 Trust & Lifecycle audit entries ───────────────────────────────────
    /// An activity digest was generated from the audit log (S15.1).
    ///
    /// Written by the `anima digest` CLI command or any caller of
    /// [`lifecycle::digest::generate_digest`].
    DigestGenerated {
        /// Agent identifier.
        agent_id: String,
        /// Number of audit entries included in the digest window.
        window_entries: usize,
        /// Tasks completed in the window.
        tasks_completed: usize,
        /// Tasks failed in the window.
        tasks_failed: usize,
        /// Cortex invocations in the window.
        cortex_invocations: usize,
        /// Sleep cycles in the window.
        sleep_cycles: usize,
        /// Defence vetoes in the window.
        defence_vetoes: usize,
        /// High-salience notable events extracted from the window.
        notable_event_count: usize,
    },

    /// A versioned agent state snapshot was created (S15.5).
    ///
    /// Written by the `anima snapshot` CLI command or the snapshot subsystem.
    SnapshotCreated {
        /// Agent identifier.
        agent_id: String,
        /// Schema version of the snapshot (see
        /// [`lifecycle::snapshot::SNAPSHOT_SCHEMA_VERSION`]).
        schema_version: u32,
        /// Path where the snapshot was saved (informational).
        snapshot_path: String,
        /// Number of audit entries captured in the snapshot summary.
        entry_count: usize,
        /// Optional human-readable annotation supplied by the caller.
        reason: Option<String>,
    },

    /// A versioned agent state snapshot was restored (S15.5).
    ///
    /// Written when the agent is restored from a snapshot, e.g. after a
    /// rollback or host migration.
    SnapshotRestored {
        /// Agent identifier.
        agent_id: String,
        /// Schema version of the restored snapshot.
        schema_version: u32,
        /// Path from which the snapshot was loaded (informational).
        snapshot_path: String,
    },

    /// An E11 self-extension proposal entered the approval queue (S15.2).
    ///
    /// Written when an agent-generated skill, tool, or weight-update proposal
    /// is submitted for operator review.
    ApprovalProposalQueued {
        /// Agent identifier.
        agent_id: String,
        /// Unique proposal identifier.
        proposal_id: String,
        /// Proposal kind label (`"new-skill"`, `"new-tool"`, `"weight-update"`).
        kind: String,
        /// Free-form provenance note (e.g. `"dreaming phase cycle 7"`).
        provenance: String,
    },

    /// An operator approved an E11 self-extension proposal (S15.2).
    ApprovalProposalDecided {
        /// Agent identifier.
        agent_id: String,
        /// Unique proposal identifier.
        proposal_id: String,
        /// `"approved"`, `"rejected"`, or `"rolled_back"`.
        decision: String,
        /// Operator-supplied reason for the decision.
        reason: String,
    },
    // ── E12 Motivation audit entries ──────────────────────────────────────────
    /// Snapshot of the drive-hierarchy urgency levels (S12.7).
    ///
    /// Emitted once per scheduler cycle when the `DriveRegistry` is configured.
    /// Six urgencies in tier order: Viability, Integrity, Service, Epistemic,
    /// Achievement, SelfActualisation — all in `[0, 1]`.
    DriveStateSnapshot {
        /// Agent identifier.
        agent_id: String,
        /// Urgency for Tier-0 (Viability): thermal, memory, power, financial deficits.
        viability_urgency: f32,
        /// Urgency for Tier-1 (Integrity): structural/identity coherence.
        integrity_urgency: f32,
        /// Urgency for Tier-2 (Service): operator attention + objective pressure.
        service_urgency: f32,
        /// Urgency for Tier-3 (Epistemic): curiosity + mastery.
        epistemic_urgency: f32,
        /// Urgency for Tier-4 (Achievement): goal-completion pressure.
        achievement_urgency: f32,
        /// Urgency for Tier-5 (SelfActualisation): long-horizon coherence.
        self_actualisation_urgency: f32,
        /// Drive delta added to the gate's value_score this cycle (`[0, 1]`).
        drive_delta: f32,
        /// Whether priority-lattice suppression was active (lower tiers stressed).
        lattice_suppression_active: bool,
    },

    /// An endogenous goal was proposed and admitted to the goal registry (S12.5).
    ///
    /// Logged when [`EndogenousGoalGenerator`] produces a goal that passes the
    /// corrigibility check and is admitted to the [`GoalRegistry`].
    GoalSpawned {
        /// Agent identifier.
        agent_id: String,
        /// Stable goal identifier assigned by the registry.
        goal_id: u64,
        /// Natural-language description of the intention.
        description: String,
        /// `"Exogenous"` or `"Endogenous(<tier>)"`.
        provenance: String,
        /// Normalised priority in `[0, 1]`.
        priority: f32,
    },

    /// An active goal was marked complete (S12.5).
    GoalCompleted {
        /// Agent identifier.
        agent_id: String,
        /// Stable goal identifier.
        goal_id: u64,
        /// Description of the completed goal.
        description: String,
    },

    /// The corrigibility invariant fired and blocked an endogenous goal (S12.3).
    ///
    /// Logged whenever [`CorrigibilityGuard::check`] returns `Held`.
    /// Repeated holds escalate to user attention via the defence layer.
    CorrigibilityHold {
        /// Agent identifier.
        agent_id: String,
        /// Brief description of the blocked goal.
        blocked_goal_description: String,
        /// Reason the corrigibility guard rejected the goal.
        reason: String,
    },

    /// Affective-state snapshot derived from the drive constellation (S12.9).
    ///
    /// Emitted alongside [`AuditEntry::DriveStateSnapshot`] when the motivation
    /// system is active.  Bounded nudge — never overrides lattice or corrigibility.
    AffectStateSnapshot {
        /// Agent identifier.
        agent_id: String,
        /// Hedonic valence in `[-1, 1]`: positive = content, negative = stressed.
        valence: f32,
        /// Arousal level in `[0, 1]`: low = calm, high = active/urgent.
        arousal: f32,
        /// Gate-threshold nudge factor in `[0.9, 1.1]`.
        gate_threshold_nudge: f32,
    },
    // ── E11 Skills & Self-Extension audit entries ─────────────────────────────
    /// A new skill was successfully registered in the skill registry (S11.3).
    ///
    /// Written immediately after `SkillRegistry::register_from_text` succeeds,
    /// regardless of whether the skill was auto-promoted or held as `Proposed`.
    SkillRegistered {
        /// Agent that owns this registry.
        agent_id: String,
        /// Stable skill identifier (lowercased `name`, spaces → `-`).
        skill_id: String,
        /// Human-readable skill name.
        skill_name: String,
        /// Who authored the skill (`"builtin"` / `"operator"` / `"agent"`).
        authored_by: String,
        /// Source episode ID, if the agent authored this skill.
        source_episode: Option<String>,
        /// Initial lifecycle state (`"Active"` or `"Proposed"`).
        initial_state: String,
    },

    /// A proposed skill was promoted to `Active` by the operator (S11.3).
    SkillPromoted {
        /// Agent that owns this registry.
        agent_id: String,
        /// Skill that was promoted.
        skill_id: String,
    },

    /// A skill was rolled back from `Active` to `RolledBack` (S11.6).
    SkillRolledBack {
        /// Agent that owns this registry.
        agent_id: String,
        /// Skill that was rolled back.
        skill_id: String,
        /// Human-readable reason for the rollback.
        reason: String,
    },

    /// A skill was quarantined due to a defence flag or circuit-breaker trip (S11.6).
    SkillQuarantined {
        /// Agent that owns this registry.
        agent_id: String,
        /// Skill that was quarantined.
        skill_id: String,
        /// Human-readable reason.
        reason: String,
    },

    /// The kill switch was activated — all active agent-authored skills quarantined (S11.6).
    SkillKillSwitchActivated {
        /// Agent that owns this registry.
        agent_id: String,
        /// IDs of every skill that was quarantined.
        quarantined_skill_ids: Vec<String>,
        /// Human-readable reason.
        reason: String,
    },

    /// A WASM tool proposal was submitted for operator approval (S11.4).
    ToolProposed {
        /// Agent that owns this registry.
        agent_id: String,
        /// Stable tool identifier.
        tool_id: String,
        /// Who authored the tool.
        authored_by: String,
        /// Fixture test summary string supplied by the sandbox runner.
        fixture_summary: String,
    },

    /// A WASM tool proposal was approved by the operator (S11.4).
    ToolApproved {
        /// Agent that owns this registry.
        agent_id: String,
        /// Stable tool identifier.
        tool_id: String,
    },

    /// A WASM tool was revoked (quarantined or removed) (S11.6).
    ToolRevoked {
        /// Agent that owns this registry.
        agent_id: String,
        /// Stable tool identifier.
        tool_id: String,
        /// Human-readable reason.
        reason: String,
    },

    /// The self-improvement reflection pass completed during the dream phase (S11.5).
    SkillReflectionCompleted {
        /// Agent that ran the reflection.
        agent_id: String,
        /// Number of episodes analysed.
        episodes_analysed: usize,
        /// Number of friction patterns identified.
        patterns_found: usize,
        /// Number of new skill drafts generated.
        proposals_generated: usize,
    },
    // ── E10 — Presence: Channel Gateway audit entries ─────────────────────────
    /// An inbound message arrived from an external channel and was admitted
    /// into the sensory bridge (E10 S10.1).
    ///
    /// Written by the channel gateway after a successful
    /// `SensoryBridge::packetize_*_checked` call.
    ChannelMessageReceived {
        /// Agent identifier.
        agent_id: String,
        /// Channel adapter id (e.g. `"telegram"`, `"slack"`).
        channel: String,
        /// Sender identifier on the originating platform.
        from: String,
        /// Modality of the message (`"text"`, `"image"`, or `"voice"`).
        modality: String,
    },
    /// An outbound message was dispatched back to a channel (E10 S10.1).
    ///
    /// Written when the gateway successfully hands off a reply to an adapter's
    /// `send()` method.
    ChannelMessageSent {
        /// Agent identifier.
        agent_id: String,
        /// Channel adapter id.
        channel: String,
        /// Recipient identifier on the target platform.
        to: String,
        /// Modality of the outbound content.
        modality: String,
    },
    /// An inbound channel message was rejected because the channel or route
    /// does not support the required modality (E10 S10.5).
    ///
    /// Written when the gateway receives a modality (e.g. `"image"`) that
    /// the current backend capabilities do not handle.  The packet is dropped
    /// and this entry is emitted so the operator can see why.
    ModalityUnsupported {
        /// Agent identifier.
        agent_id: String,
        /// Channel that produced the unsupported modality.
        channel: String,
        /// The modality that was not supported.
        modality: String,
    },
    // ── E7 — Embodiment audit entries ─────────────────────────────────────────
    /// An outbound network request passed egress screening and was dispatched
    /// (E7 S7.0.3).
    ///
    /// Emitted immediately before the tool driver `invoke` call so the audit
    /// trail records every effected external action.
    EgressRequested {
        /// Stable tool identifier that initiated the request.
        tool_id: String,
        /// Target URL (redacted of query-string secrets by the call site).
        url: String,
    },

    /// An outbound network request was denied by the egress guard before any
    /// network activity occurred (E7 S7.0.3).
    ///
    /// Emitted in place of [`AuditEntry::EgressRequested`] when the guard
    /// vetoes the request.  No network connection is ever opened for denied
    /// requests.
    EgressBlocked {
        /// Stable tool identifier that attempted the request.
        tool_id: String,
        /// Target URL that was denied.
        url: String,
        /// Human-readable denial reason from the egress guard.
        reason: String,
    },

    /// The semantic tool-selection step narrowed the route's allow-list before
    /// building the cortex invocation request (E7 S7.3.4).
    ///
    /// Satisfies E7 S7.3 exit criterion 2: "selection is deterministic for
    /// fixed inputs."  Written once per cortex invocation that goes through
    /// the scorer pipeline.
    ToolSelection {
        /// Agent identifier.
        agent_id: String,
        /// Task description used as the scoring query.
        task_description: String,
        /// Number of tools scored (= size of the tier allow-list).
        candidates_scored: usize,
        /// Number of tools kept after `length_robust_filter`.
        kept: usize,
        /// The `tau_rel` threshold applied to the filter.
        tau_rel: f32,
    },

    // ── E17 Trust, Human-Identity & Privacy ───────────────────────────────────
    /// A new user profile was created in the [`UserRegistry`] (S17.2).
    ///
    /// Emitted on the first contact from a previously-unseen channel sender.
    UserProfileCreated {
        /// Agent identifier.
        agent_id: String,
        /// Stable user identifier (`"<channel>:<platform_id>"`).
        user_id: String,
        /// Human-readable display name from the channel.
        display_name: String,
        /// Source channel (`"telegram"`, `"slack"`, …).
        channel: String,
    },

    /// The operator updated the trust tier for a user (S17.1 / S17.2).
    UserTrustUpdated {
        /// Agent identifier.
        agent_id: String,
        /// Stable user identifier.
        user_id: String,
        /// Previous trust tier label.
        old_tier: String,
        /// New trust tier label.
        new_tier: String,
    },

    /// A user's data-retention consent was updated (S17.3).
    UserConsentUpdated {
        /// Agent identifier.
        agent_id: String,
        /// Stable user identifier.
        user_id: String,
        /// Data category whose consent changed.
        category: String,
        /// `true` when consent was granted; `false` when revoked.
        granted: bool,
    },

    // ── E16 — Multi-Agent Coordination (A2A Substrate) ────────────────────────
    /// A task was delegated to a named sub-agent via the A2A bus (E16, S16.3).
    ///
    /// Written immediately before the endpoint is invoked so the delegation
    /// attempt is visible in the audit trail even if the endpoint fails.
    AgentDelegated {
        /// Identifier of the agent issuing the delegation.
        parent_agent_id: String,
        /// Identifier of the target sub-agent in the pool.
        target_agent_id: String,
        /// Unique correlation token for this delegation (matches the
        /// subsequent [`AuditEntry::AgentDelegationCompleted`] or
        /// [`AuditEntry::AgentDelegationFailed`] entry).
        delegation_id: String,
        /// Human-readable task description forwarded to the sub-agent.
        task: String,
    },

    /// A delegated task completed and the sub-agent returned a result (E16, S16.3).
    ///
    /// Always paired with a preceding [`AuditEntry::AgentDelegated`] that
    /// carries the same `delegation_id`.
    AgentDelegationCompleted {
        /// Identifier of the agent that issued the delegation.
        parent_agent_id: String,
        /// Identifier of the sub-agent that executed the task.
        target_agent_id: String,
        /// Correlation token matching the preceding `AgentDelegated` entry.
        delegation_id: String,
        /// `true` when the sub-agent considered the task successful.
        success: bool,
        /// Number of tool calls the sub-agent made during execution.
        tool_calls_made: usize,
        /// Wall-clock duration of the sub-agent invocation in milliseconds.
        duration_ms: u64,
        /// Short summary of what the sub-agent accomplished.
        summary: String,
    },

    /// Delegation failed — the agent was not found or the endpoint returned
    /// an error (E16, S16.3).
    ///
    /// Always paired with a preceding [`AuditEntry::AgentDelegated`] that
    /// carries the same `delegation_id`.
    AgentDelegationFailed {
        /// Identifier of the agent that issued the delegation.
        parent_agent_id: String,
        /// Identifier of the target sub-agent (may be unknown).
        target_agent_id: String,
        /// Correlation token matching the preceding `AgentDelegated` entry.
        delegation_id: String,
        /// Human-readable failure reason.
        reason: String,
    },

    // ── E8 S8.4.3 — Sleep-cycle consolidation hook ────────────────────────────
    /// The consolidation hook was skipped because fewer training pairs were
    /// compiled than [`crate::consolidation::ConsolidationConfig::min_pairs`]
    /// required.
    ConsolidationSkipped {
        /// Agent identifier.
        agent_id: String,
        /// Number of pairs available this cycle.
        pairs_available: usize,
        /// Minimum threshold required to trigger fine-tuning.
        min_required: usize,
    },

    /// The consolidation hook started a fine-tune run on the compiled pairs.
    ConsolidationStarted {
        /// Agent identifier.
        agent_id: String,
        /// Number of training pairs submitted to the tuner.
        pairs_trained: usize,
    },

    /// A consolidation fine-tune run completed and an adapter was produced.
    ConsolidationCompleted {
        /// Agent identifier.
        agent_id: String,
        /// Adapter id returned by the tuner.
        adapter_id: String,
        /// Number of training pairs used.
        pairs_trained: usize,
        /// Whether the adapter was registered in the configured library.
        registered: bool,
    },

    /// A consolidation fine-tune run failed.
    ConsolidationFailed {
        /// Agent identifier.
        agent_id: String,
        /// Error description from the tuner.
        error: String,
    },

    // ── E27 — Knowledge Graph ─────────────────────────────────────────────────
    /// A new entity was added to the knowledge graph.
    KnowledgeEntityAdded {
        /// Agent identifier.
        agent_id: String,
        /// Stable entity id.
        entity_id: String,
        /// Entity kind label (e.g. "person", "technology").
        kind: String,
        /// Human-readable display name.
        display_name: String,
    },

    /// A directed relation was added between two entities.
    KnowledgeRelationAdded {
        /// Agent identifier.
        agent_id: String,
        /// Source entity id.
        from_entity: String,
        /// Target entity id.
        to_entity: String,
        /// Relation kind label (e.g. "works_at", "depends_on").
        kind: String,
    },

    /// The knowledge graph was queried.
    KnowledgeGraphQueried {
        /// Agent identifier.
        agent_id: String,
        /// Type of query performed (e.g. "neighbors", "by_kind", "by_attribute").
        query_type: String,
        /// Number of entities returned.
        result_count: usize,
    },
}

// ── AuditLog ──────────────────────────────────────────────────────────────────

/// Append-only audit log with an optional durable JSONL file sink (EX.2).
///
/// The in-memory `Vec<AuditEntry>` is always maintained so that all existing
/// query call sites (`entries()`, `len()`, `is_empty()`) continue to work
/// without modification.  When a file sink is configured, every `push` also
/// serialises the entry as a newline-delimited JSON record.
pub struct AuditLog {
    entries: Vec<AuditEntry>,
    /// Shared file sink — `Arc<Mutex<…>>` so `Clone` gives a shared handle
    /// (all clones of a manager write to the same audit file).
    #[cfg(feature = "std")]
    file_sink: Option<Arc<Mutex<std::fs::File>>>,
    /// Shared failure flag — `Arc<AtomicBool>` so all clones see the same
    /// failure state.  Once a write fails, every clone skips the file to
    /// prevent performance degradation from repeatedly locking a broken sink.
    #[cfg(feature = "std")]
    sink_failed: Arc<std::sync::atomic::AtomicBool>,
    /// Optional HMAC tamper-evidence sidecar, shared across clones so every
    /// handle extends the same chain. `None` unless `ANIMA_AUDIT_HMAC_KEY` (or
    /// [`AuditLog::with_file_hmac`]) configured one.
    #[cfg(feature = "std")]
    integrity: Option<Arc<Mutex<IntegrityChain>>>,
}

impl std::fmt::Debug for AuditLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut d = f.debug_struct("AuditLog");
        d.field("entries", &self.entries.len());
        #[cfg(feature = "std")]
        {
            use std::sync::atomic::Ordering;
            d.field("has_file_sink", &self.file_sink.is_some())
                .field("sink_failed", &self.sink_failed.load(Ordering::Relaxed))
                .field("has_integrity_chain", &self.integrity.is_some());
        }
        d.finish()
    }
}

impl Clone for AuditLog {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            #[cfg(feature = "std")]
            file_sink: self.file_sink.clone(),
            // Clone shares the same AtomicBool so failure on one clone disables
            // all clones — no broken-fd thundering herd.
            #[cfg(feature = "std")]
            sink_failed: self.sink_failed.clone(),
            #[cfg(feature = "std")]
            integrity: self.integrity.clone(),
        }
    }
}

impl Default for AuditLog {
    fn default() -> Self {
        Self::new()
    }
}

impl AuditLog {
    /// Creates an in-memory-only log (no file persistence).
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            #[cfg(feature = "std")]
            file_sink: None,
            #[cfg(feature = "std")]
            sink_failed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            #[cfg(feature = "std")]
            integrity: None,
        }
    }

    /// Opens an append-only JSONL file at `path` and attaches it as the durable
    /// sink.  Each write is flushed immediately for durability.
    #[cfg(feature = "std")]
    pub fn with_file(path: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            entries: Vec::new(),
            file_sink: Some(Arc::new(Mutex::new(file))),
            sink_failed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            integrity: None,
        })
    }

    /// Like [`with_file`](Self::with_file) but also opens an HMAC tamper-evidence
    /// sidecar at `<path>.hmac`, keyed by `key`, that chains every persisted
    /// line (threat T-8).  Use [`verify_audit_chain`] to check it later.
    ///
    /// If a sidecar already exists, its last MAC seeds the running chain so
    /// appends extend the existing chain rather than restarting it.
    #[cfg(feature = "std")]
    pub fn with_file_hmac(path: impl AsRef<std::path::Path>, key: &[u8]) -> std::io::Result<Self> {
        let path = path.as_ref();
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;

        let chain_path = chain_sidecar_path(path);
        let prev = read_last_chain_mac(&chain_path);
        let chain_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&chain_path)?;

        Ok(Self {
            entries: Vec::new(),
            file_sink: Some(Arc::new(Mutex::new(file))),
            sink_failed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            integrity: Some(Arc::new(Mutex::new(IntegrityChain {
                file: chain_file,
                key: key.to_vec(),
                prev,
            }))),
        })
    }

    /// Creates a log that writes to `$ANIMA_AUDIT_DIR/<agent_id>.jsonl` when
    /// the environment variable is set.  Emits a warning to stderr when the
    /// variable is set but the file cannot be opened; falls back to in-memory-only.
    ///
    /// # Path safety
    ///
    /// `agent_id` is used as a filename component.  It must be a single normal
    /// path component (no separators, no `.`, no `..`, no root prefix) to prevent
    /// path-traversal writes outside `ANIMA_AUDIT_DIR` on all platforms including
    /// Windows (where `\` is also a valid separator).
    #[cfg(feature = "std")]
    pub fn from_env(agent_id: &str) -> Self {
        // Reject IDs that are not a single, plain path component.
        // On Unix `\` is a valid filename character so `Path::components()` will
        // not split it — add an explicit check to reject `\` on all platforms,
        // preventing path traversal via agents with Windows-style separators.
        let is_safe = {
            use std::path::{Component, Path};
            let mut comps = Path::new(agent_id).components();
            matches!(comps.next(), Some(Component::Normal(_)))
                && comps.next().is_none()
                && !agent_id.contains('\\')
        };
        if !is_safe {
            eprintln!(
                "anima-audit: rejected unsafe agent_id '{agent_id}' — falling back to in-memory"
            );
            return Self::new();
        }
        if let Ok(dir) = std::env::var("ANIMA_AUDIT_DIR") {
            let path = std::path::PathBuf::from(&dir).join(format!("{}.jsonl", agent_id));
            if let Some(parent) = path.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    eprintln!(
                        "anima-audit: failed to create directory {}: {e}",
                        parent.display()
                    );
                }
            }
            // When an HMAC key is configured, chain the log into a tamper-
            // evidence sidecar; otherwise open the plain durable sink.
            let key = std::env::var("ANIMA_AUDIT_HMAC_KEY")
                .ok()
                .filter(|k| !k.is_empty());
            let opened = match &key {
                Some(k) => Self::with_file_hmac(&path, k.as_bytes()),
                None => Self::with_file(&path),
            };
            match opened {
                Ok(log) => return log,
                Err(e) => eprintln!(
                    "anima-audit: failed to open audit file {}: {e} — falling back to in-memory",
                    path.display()
                ),
            }
        }
        Self::new()
    }

    /// Returns `true` if the file sink has failed and further disk writes are
    /// being skipped.
    #[cfg(feature = "std")]
    pub fn sink_failed(&self) -> bool {
        use std::sync::atomic::Ordering;
        self.sink_failed.load(Ordering::Relaxed)
    }

    /// Appends an entry to both the in-memory store and the file sink (if any).
    ///
    /// Each write is followed by a `flush()` to honour the durability guarantee.
    /// If the write or flush fails the failure is recorded in `sink_failed` and a
    /// warning is emitted to stderr; subsequent pushes skip the file entirely.
    /// The failure flag is shared across all clones via `Arc<AtomicBool>` so a
    /// failure on any clone permanently disables all clones.
    pub fn push(&mut self, entry: AuditEntry) {
        #[cfg(feature = "std")]
        {
            use std::sync::atomic::Ordering;
            if !self.sink_failed.load(Ordering::Relaxed) {
                if let Some(sink) = &self.file_sink {
                    match serde_json::to_string(&entry) {
                        Ok(line) => {
                            let wrote = match sink.lock() {
                                Ok(mut f) => {
                                    let write_ok =
                                        writeln!(f, "{line}").is_ok() && f.flush().is_ok();
                                    if !write_ok {
                                        eprintln!("anima-audit: write/flush to file sink failed — sink disabled");
                                        self.sink_failed.store(true, Ordering::Relaxed);
                                    }
                                    write_ok
                                }
                                Err(_) => {
                                    eprintln!(
                                        "anima-audit: file sink mutex poisoned — sink disabled"
                                    );
                                    self.sink_failed.store(true, Ordering::Relaxed);
                                    false
                                }
                            };
                            // Extend the HMAC chain only after the line is durably
                            // written, so the sidecar never commits to a line the
                            // log itself is missing.
                            if wrote {
                                self.extend_chain(&line);
                            }
                        }
                        Err(e) => {
                            // Serialisation failures are entry-specific (e.g. NaN/Inf in a
                            // GateDecision field) — skip only this entry rather than disabling
                            // the sink for all future writes.
                            eprintln!("anima-audit: serialisation failure for audit entry — entry skipped: {e}");
                        }
                    }
                }
            }
        }
        self.entries.push(entry);
    }

    /// Append one MAC to the HMAC tamper-evidence sidecar, advancing the chain.
    /// A no-op when no integrity sidecar is configured.
    #[cfg(feature = "std")]
    fn extend_chain(&self, line: &str) {
        use std::sync::atomic::Ordering;
        let Some(integ) = &self.integrity else {
            return;
        };
        match integ.lock() {
            Ok(mut st) => {
                let mac = hmac_sha256(&st.key, &[st.prev.as_slice(), line.as_bytes()]);
                let hex = to_hex(&mac);
                let chain_ok = writeln!(st.file, "{hex}").is_ok() && st.file.flush().is_ok();
                if chain_ok {
                    st.prev = mac;
                } else {
                    eprintln!("anima-audit: write/flush to HMAC sidecar failed — sink disabled");
                    self.sink_failed.store(true, Ordering::Relaxed);
                }
            }
            Err(_) => {
                eprintln!("anima-audit: HMAC sidecar mutex poisoned — sink disabled");
                self.sink_failed.store(true, Ordering::Relaxed);
            }
        }
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

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(all(test, feature = "std"))]
mod integrity_tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Unique temp path so parallel tests never collide (no tempfile dev-dep).
    fn temp_log_path(tag: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let unique = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "anima-audit-{tag}-{}-{nanos}-{unique}.jsonl",
            std::process::id()
        ))
    }

    /// Remove a log and its sidecar after a test.
    fn cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(chain_sidecar_path(path));
    }

    fn entry(id: &str) -> AuditEntry {
        AuditEntry::SleepEntered {
            agent_id: id.to_string(),
        }
    }

    #[test]
    fn hmac_sha256_matches_rfc4231_test_case_1() {
        // RFC 4231 §4.2: key = 0x0b × 20, data = "Hi There".
        let key = [0x0bu8; 20];
        let mac = hmac_sha256(&key, &[b"Hi There"]);
        assert_eq!(
            to_hex(&mac),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn multi_part_hmac_equals_concatenated_message() {
        // The chain feeds (prev ‖ line) as two parts; that must equal hashing
        // the single concatenated buffer.
        let key = b"chain-key";
        let split = hmac_sha256(key, &[b"abc", b"def"]);
        let joined = hmac_sha256(key, &[b"abcdef"]);
        assert_eq!(split, joined);
    }

    #[test]
    fn chain_verifies_for_untampered_log() {
        let path = temp_log_path("ok");
        let key = b"operator-secret";
        {
            let mut log = AuditLog::with_file_hmac(&path, key).expect("open");
            for i in 0..5 {
                log.push(entry(&format!("agent-{i}")));
            }
        }
        let result = verify_audit_chain(&path, chain_sidecar_path(&path), key).expect("verify");
        assert_eq!(result, ChainVerification::Ok { entries: 5 });
        cleanup(&path);
    }

    #[test]
    fn chain_detects_a_tampered_entry() {
        let path = temp_log_path("tamper");
        let key = b"k";
        {
            let mut log = AuditLog::with_file_hmac(&path, key).expect("open");
            log.push(entry("alpha"));
            log.push(entry("bravo"));
            log.push(entry("charlie"));
        }
        // Rewrite the middle line in place — the kind of edit a host-privileged
        // attacker would make to erase an action from the trail.
        let original = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = original.lines().map(String::from).collect();
        lines[1] = serde_json::to_string(&entry("EVIL")).unwrap();
        std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();

        let result = verify_audit_chain(&path, chain_sidecar_path(&path), key).expect("verify");
        assert_eq!(result, ChainVerification::Tampered { index: 1 });
        cleanup(&path);
    }

    #[test]
    fn chain_detects_truncation() {
        let path = temp_log_path("trunc");
        let key = b"k";
        {
            let mut log = AuditLog::with_file_hmac(&path, key).expect("open");
            log.push(entry("one"));
            log.push(entry("two"));
            log.push(entry("three"));
        }
        // Drop the last log line but keep the sidecar — a classic "cut the tail
        // off the record" attack.
        let original = std::fs::read_to_string(&path).unwrap();
        let kept: Vec<&str> = original.lines().take(2).collect();
        std::fs::write(&path, format!("{}\n", kept.join("\n"))).unwrap();

        let result = verify_audit_chain(&path, chain_sidecar_path(&path), key).expect("verify");
        assert_eq!(
            result,
            ChainVerification::LengthMismatch {
                jsonl_lines: 2,
                chain_lines: 3,
            }
        );
        cleanup(&path);
    }

    #[test]
    fn verification_fails_under_the_wrong_key() {
        let path = temp_log_path("wrongkey");
        {
            let mut log = AuditLog::with_file_hmac(&path, b"right-key").expect("open");
            log.push(entry("solo"));
        }
        let result =
            verify_audit_chain(&path, chain_sidecar_path(&path), b"wrong-key").expect("verify");
        assert_eq!(result, ChainVerification::Tampered { index: 0 });
        cleanup(&path);
    }

    #[test]
    fn malformed_sidecar_line_is_reported() {
        let path = temp_log_path("malformed");
        let key = b"k";
        {
            let mut log = AuditLog::with_file_hmac(&path, key).expect("open");
            log.push(entry("x"));
        }
        std::fs::write(chain_sidecar_path(&path), "not-a-valid-hex-mac\n").unwrap();
        let result = verify_audit_chain(&path, chain_sidecar_path(&path), key).expect("verify");
        assert_eq!(result, ChainVerification::MalformedChainLine { index: 0 });
        cleanup(&path);
    }

    #[test]
    fn chain_resumes_correctly_across_reopen() {
        let path = temp_log_path("resume");
        let key = b"persistent-key";
        {
            let mut log = AuditLog::with_file_hmac(&path, key).expect("open 1");
            log.push(entry("first"));
            log.push(entry("second"));
        }
        // Reopen the same paths: the chain must seed `prev` from the existing
        // sidecar so appended entries extend (not restart) the chain.
        {
            let mut log = AuditLog::with_file_hmac(&path, key).expect("open 2");
            log.push(entry("third"));
        }
        let result = verify_audit_chain(&path, chain_sidecar_path(&path), key).expect("verify");
        assert_eq!(result, ChainVerification::Ok { entries: 3 });
        cleanup(&path);
    }

    #[test]
    fn plain_with_file_writes_no_sidecar() {
        let path = temp_log_path("plain");
        {
            let mut log = AuditLog::with_file(&path).expect("open");
            log.push(entry("nochain"));
        }
        assert!(
            !chain_sidecar_path(&path).exists(),
            "with_file must not create an HMAC sidecar"
        );
        cleanup(&path);
    }
}
