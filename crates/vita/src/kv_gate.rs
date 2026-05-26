//! KV-cache controller runtime integration — Story S5.4.4.
//!
//! This module wires the [`KvController`] from `kv_controller` into the
//! vita lifecycle so that routes with [`MemoryScope::kv_controller = true`]
//! use the learned gate for block-level eviction decisions during cortex
//! invocations.
//!
//! # Integration path
//!
//! The cortex invocation call-site calls [`gate_working_context`] before each
//! planning turn when the active route has `kv_controller: true`.  The function
//! decides which blocks to retain and records the decision in the audit log.
//!
//! # Fault handling (exit criterion 2)
//!
//! When the controller transitions to [`ControllerState::Faulted`]:
//! 1. A [`AuditEntry::KvControllerFaulted`] entry is written *immediately*.
//! 2. Subsequent calls produce [`AuditEntry::KvGatePass { fallback_lru: true }`]
//!    entries while the controller remains faulted.
//! 3. The caller receives LRU-ordered [`KvGateDecision`]s so the context
//!    window is still pruned correctly.
//!
//! The controller can only exit the faulted state via an explicit call to
//! [`KvController::reset`] (intended for operator intervention).
//!
//! # TurboQuant integration (E2.7)
//!
//! The [`Quantizer`] trait seam is carried through from `kv_controller` crate.
//! When E2.7 merges, install a `TurboQuantizer` via
//! [`KvController::with_quantizer`]; the gate will automatically incorporate
//! the quantisation similarity score into its retention priority.

#![forbid(unsafe_code)]

use kv_controller::{BlockFeatures, BlockRole, ControllerState, KvController, KvGateDecision};

use crate::{AuditEntry, AuditLog};

// ── Context block record ───────────────────────────────────────────────────────

/// Metadata about a single block in the working context.
///
/// Callers construct these from the L1 block table.  The controller does not
/// reach into `memory::VirtualContextManager` directly — instead, the caller
/// translates the block table into a slice of `ContextBlock` values and passes
/// it to [`gate_working_context`].
#[derive(Debug, Clone)]
pub struct ContextBlock {
    /// Sequential block index (0 = oldest).
    pub block_index: usize,
    /// Role that generated this block.
    pub role: BlockRole,
    /// Block contains a user-specified hard constraint.
    pub is_user_constraint: bool,
    /// Block contains error / exception trace information.
    pub is_error_trace: bool,
    /// Block is a tool invocation return value.
    pub is_tool_output: bool,
}

impl ContextBlock {
    /// Converts this block to [`BlockFeatures`] with the given context metadata.
    pub fn to_features(&self, total_blocks: usize, memory_pressure: f32) -> BlockFeatures {
        BlockFeatures::new(
            self.block_index,
            total_blocks,
            self.role,
            self.is_user_constraint,
            self.is_error_trace,
            self.is_tool_output,
            memory_pressure,
        )
    }
}

// ── Gate pass result ───────────────────────────────────────────────────────────

/// Summary of a single block-selection pass.
#[derive(Debug, Clone)]
pub struct GatePassResult {
    /// Per-block gate decisions in block-index order.
    pub decisions: Vec<KvGateDecision>,
    /// `true` when the pass used LRU fallback (controller was faulted).
    pub fallback_lru: bool,
    /// Whether the controller transitioned to `Faulted` during this pass.
    pub faulted_this_pass: bool,
    /// Number of needle blocks retained.
    pub needles_retained: usize,
    /// Total needle blocks.
    pub total_needles: usize,
}

impl GatePassResult {
    /// Blocks retained after the gate decision.
    pub fn retained_count(&self) -> usize {
        self.decisions.iter().filter(|d| d.retain).count()
    }
}

// ── Main integration function ──────────────────────────────────────────────────

/// Gate the working context: select at most `budget` blocks to retain.
///
/// This is the primary integration point called by the cortex invocation
/// path when `route.memory_scope.kv_controller == true`.
///
/// # Audit entries written
///
/// - [`AuditEntry::KvControllerFaulted`] — written once on the transition
///   from `Active` to `Faulted`.
/// - [`AuditEntry::KvGatePass`] — written on every call (fallback or not).
///
/// # Arguments
///
/// - `controller` — the shared controller instance (mutable borrow).
/// - `blocks` — current working-context blocks from the L1 block table.
/// - `budget` — maximum blocks to retain.
/// - `memory_pressure` — scalar pressure signal from interoception (`[0.0, 1.0]`).
/// - `agent_id` — agent identifier for audit entries.
/// - `task_id` — per-invocation identifier for audit correlation.
/// - `audit_log` — the lifecycle audit log.
pub fn gate_working_context(
    controller: &mut KvController,
    blocks: &[ContextBlock],
    budget: usize,
    memory_pressure: f32,
    agent_id: &str,
    task_id: &str,
    audit_log: &mut AuditLog,
) -> GatePassResult {
    let total_blocks = blocks.len();
    let total_needles = blocks.iter().filter(|b| b.is_user_constraint).count();

    let was_faulted_before = controller.state == ControllerState::Faulted;

    // Build feature vectors.
    let features: Vec<BlockFeatures> = blocks
        .iter()
        .map(|b| b.to_features(total_blocks, memory_pressure))
        .collect();

    // Run the gate.
    let decisions = controller.select_blocks(&features, budget);

    let faulted_this_pass = !was_faulted_before && controller.state == ControllerState::Faulted;
    let fallback_lru = decisions.iter().any(|d| d.fallback_lru);

    // If controller just faulted, write the fault entry first.
    if faulted_this_pass {
        audit_log.push(AuditEntry::KvControllerFaulted {
            agent_id: agent_id.to_string(),
            task_id: task_id.to_string(),
            fault_count: controller.fault_count,
        });
    }

    let needles_retained = decisions
        .iter()
        .zip(features.iter())
        .filter(|(d, f)| d.retain && f.is_user_constraint)
        .count();

    // Write the gate-pass entry.
    audit_log.push(AuditEntry::KvGatePass {
        agent_id: agent_id.to_string(),
        task_id: task_id.to_string(),
        total_blocks,
        retained_blocks: decisions.iter().filter(|d| d.retain).count(),
        budget,
        fallback_lru,
        needles_retained,
        total_needles,
    });

    GatePassResult {
        decisions,
        fallback_lru,
        faulted_this_pass,
        needles_retained,
        total_needles,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kv_controller::{AlwaysFaultGate, KvController};

    fn make_blocks(n: usize, needle_indices: &[usize]) -> Vec<ContextBlock> {
        (0..n)
            .map(|i| ContextBlock {
                block_index: i,
                role: BlockRole::User,
                is_user_constraint: needle_indices.contains(&i),
                is_error_trace: false,
                is_tool_output: false,
            })
            .collect()
    }

    // ── Exit criterion 2: fault → LRU fallback + audit entry ─────────────────

    /// When the controller faults, the next gate pass switches to LRU and
    /// writes a `KvControllerFaulted` audit entry followed by `KvGatePass`.
    #[test]
    fn kv_controller_fault_is_recorded_in_audit_log() {
        let mut ctrl = KvController::new(AlwaysFaultGate, 0.5);
        let mut log = AuditLog::new();
        let blocks = make_blocks(10, &[0, 1]);

        let result = gate_working_context(
            &mut ctrl,
            &blocks,
            5,
            0.5,
            "agent-1",
            "task-abc",
            &mut log,
        );

        // The controller should have faulted during this pass.
        assert!(result.faulted_this_pass, "controller should have faulted");
        assert!(result.fallback_lru, "result should use LRU fallback");

        // Audit log: KvControllerFaulted then KvGatePass
        assert_eq!(log.len(), 2, "expected 2 audit entries");
        assert!(
            matches!(log.entries()[0], AuditEntry::KvControllerFaulted { .. }),
            "first entry should be KvControllerFaulted"
        );
        assert!(
            matches!(log.entries()[1], AuditEntry::KvGatePass { fallback_lru: true, .. }),
            "second entry should be KvGatePass with fallback_lru=true"
        );
    }

    /// After the initial fault, subsequent passes produce only `KvGatePass`
    /// entries (not a second `KvControllerFaulted`).
    #[test]
    fn subsequent_faulted_passes_produce_only_gate_pass_entries() {
        let mut ctrl = KvController::new(AlwaysFaultGate, 0.5);
        let mut log = AuditLog::new();
        let blocks = make_blocks(6, &[]);

        // First call: faults and writes 2 entries.
        gate_working_context(&mut ctrl, &blocks, 3, 0.0, "a", "t", &mut log);
        // Second call: controller already faulted — should write only 1 entry.
        gate_working_context(&mut ctrl, &blocks, 3, 0.0, "a", "t", &mut log);

        assert_eq!(log.len(), 3, "second pass should add only 1 entry");
        assert!(
            matches!(log.entries()[2], AuditEntry::KvGatePass { fallback_lru: true, .. }),
            "third entry should be KvGatePass"
        );
    }

    /// Pre-trained controller (active) writes a normal gate-pass entry without fault.
    #[test]
    fn active_controller_writes_gate_pass_without_fault_entry() {
        let mut ctrl = KvController::with_pre_trained_weights();
        let mut log = AuditLog::new();
        let blocks = make_blocks(10, &[0, 3]);

        let result = gate_working_context(
            &mut ctrl,
            &blocks,
            5,
            0.3,
            "agent-2",
            "task-xyz",
            &mut log,
        );

        assert!(!result.faulted_this_pass);
        assert!(!result.fallback_lru);
        assert_eq!(log.len(), 1, "active controller writes 1 entry");
        assert!(
            matches!(log.entries()[0], AuditEntry::KvGatePass { fallback_lru: false, .. }),
            "entry should be a non-fallback gate pass"
        );
    }

    /// Budget is respected: at most `budget` blocks are retained.
    #[test]
    fn gate_pass_respects_block_budget() {
        let mut ctrl = KvController::with_pre_trained_weights();
        let mut log = AuditLog::new();
        let blocks = make_blocks(20, &[0, 1, 2]);

        let result = gate_working_context(&mut ctrl, &blocks, 7, 0.5, "a", "t", &mut log);
        assert_eq!(result.retained_count(), 7);
    }

    /// Needle retention is counted correctly in the pass result and audit entry.
    #[test]
    fn gate_pass_counts_needle_retention_correctly() {
        // Pre-trained controller retains all needles in the oldest half.
        let mut ctrl = KvController::with_pre_trained_weights();
        let mut log = AuditLog::new();
        // Needles at indices 0 and 1 (oldest blocks).
        let blocks = make_blocks(10, &[0, 1]);

        let result = gate_working_context(&mut ctrl, &blocks, 5, 0.5, "a", "t", &mut log);
        assert_eq!(result.total_needles, 2);
        // Pre-trained weights strongly prefer user_constraint blocks.
        assert_eq!(
            result.needles_retained, 2,
            "pre-trained controller should retain both needles"
        );

        // Audit entry should also reflect the counts.
        if let AuditEntry::KvGatePass {
            needles_retained,
            total_needles,
            ..
        } = &log.entries()[0]
        {
            assert_eq!(*needles_retained, 2);
            assert_eq!(*total_needles, 2);
        } else {
            panic!("expected KvGatePass");
        }
    }
}
