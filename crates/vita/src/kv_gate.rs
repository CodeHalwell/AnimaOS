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

use interoception::InteroceptiveSignals;
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

// ── S5.7.6 Cache-Controller Modulation ────────────────────────────────────────

/// Compute the effective block budget after applying memory-pressure scaling.
///
/// # Behaviour
///
/// | `memory_pressure` | Effective budget |
/// |-------------------|-----------------|
/// | `< 0.5`           | `nominal_budget` (no reduction) |
/// | `0.5 → 1.0`       | scales linearly from `nominal` down to `0.70 × nominal` |
/// | `1.0`             | `max(ceil(0.70 × nominal_budget), 1)` |
///
/// The activation threshold (0.5) is chosen so that routine background pressure
/// does not trigger eviction budget cuts; only genuinely elevated memory load
/// (above half the scale) tightens the block window.
///
/// The maximum reduction is 30 % of the nominal budget, traded off against
/// the minimum guarantee of at least 1 block so the context is never fully
/// emptied by a pressure event alone.
///
/// # Monotone property
///
/// For a fixed `nominal_budget`, the result is non-increasing in
/// `memory_pressure`: `effective_budget(b, p₁) >= effective_budget(b, p₂)`
/// whenever `p₁ <= p₂`.
pub fn effective_budget_under_pressure(nominal_budget: usize, memory_pressure: f32) -> usize {
    let pressure = memory_pressure.clamp(0.0, 1.0);
    if pressure < 0.5 {
        return nominal_budget;
    }
    // Map (0.5, 1.0] → (0.0, 1.0] and apply up to 30 % reduction.
    let excess = (pressure - 0.5) * 2.0; // normalise to [0.0, 1.0]
    let max_reduction = 0.30_f32;
    let factor = 1.0 - max_reduction * excess;
    let effective = (nominal_budget as f32 * factor).ceil() as usize;
    effective.max(1)
}

/// Gate the working context using live [`InteroceptiveSignals`] — Story S5.7.6.
///
/// This is the formal bridge between the interoceptive sensor layer and the
/// KV-cache controller: `memory_pressure` from the sensor bundle is used to
/// (a) populate the `memory_pressure` feature for every block so the linear
/// model can account for it in its per-block score, and (b) reduce the
/// effective block budget when pressure is elevated, making eviction
/// **more aggressive under pressure** as S5.7.6 requires.
///
/// # Audit entries written
///
/// 1. [`AuditEntry::KvMemoryPressureModulation`] — written **before** the
///    gate pass only when `memory_pressure >= 0.5` (i.e., when a budget
///    reduction actually fires).
/// 2. Then the normal sequence from [`gate_working_context`]:
///    - Optionally [`AuditEntry::KvControllerFaulted`] (first fault only).
///    - [`AuditEntry::KvGatePass`] always.
///
/// # Arguments
///
/// - `controller` — shared controller instance (mutable borrow).
/// - `blocks` — current working-context blocks from the L1 block table.
/// - `nominal_budget` — maximum blocks to retain (pre-pressure).
/// - `signals` — live interoceptive snapshot; `signals.memory_pressure` drives
///   both the feature vector and the budget adjustment.
/// - `agent_id`, `task_id`, `audit_log` — audit trail fields.
pub fn gate_working_context_with_signals(
    controller: &mut KvController,
    blocks: &[ContextBlock],
    nominal_budget: usize,
    signals: &InteroceptiveSignals,
    agent_id: &str,
    task_id: &str,
    audit_log: &mut AuditLog,
) -> GatePassResult {
    let memory_pressure = signals.memory_pressure.clamp(0.0, 1.0);
    let effective_budget = effective_budget_under_pressure(nominal_budget, memory_pressure);

    // Log the modulation event when it actually fires (pressure threshold met).
    if effective_budget < nominal_budget {
        audit_log.push(AuditEntry::KvMemoryPressureModulation {
            agent_id: agent_id.to_string(),
            task_id: task_id.to_string(),
            memory_pressure,
            nominal_budget,
            effective_budget,
        });
    }

    gate_working_context(
        controller,
        blocks,
        effective_budget,
        memory_pressure,
        agent_id,
        task_id,
        audit_log,
    )
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

        let result =
            gate_working_context(&mut ctrl, &blocks, 5, 0.5, "agent-1", "task-abc", &mut log);

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
            matches!(
                log.entries()[1],
                AuditEntry::KvGatePass {
                    fallback_lru: true,
                    ..
                }
            ),
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
            matches!(
                log.entries()[2],
                AuditEntry::KvGatePass {
                    fallback_lru: true,
                    ..
                }
            ),
            "third entry should be KvGatePass"
        );
    }

    /// Pre-trained controller (active) writes a normal gate-pass entry without fault.
    #[test]
    fn active_controller_writes_gate_pass_without_fault_entry() {
        let mut ctrl = KvController::with_pre_trained_weights();
        let mut log = AuditLog::new();
        let blocks = make_blocks(10, &[0, 3]);

        let result =
            gate_working_context(&mut ctrl, &blocks, 5, 0.3, "agent-2", "task-xyz", &mut log);

        assert!(!result.faulted_this_pass);
        assert!(!result.fallback_lru);
        assert_eq!(log.len(), 1, "active controller writes 1 entry");
        assert!(
            matches!(
                log.entries()[0],
                AuditEntry::KvGatePass {
                    fallback_lru: false,
                    ..
                }
            ),
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

    // ── S5.7.6 — effective_budget_under_pressure tests ───────────────────────

    /// Below the 0.5 activation threshold the budget is returned unchanged.
    #[test]
    fn effective_budget_unchanged_below_pressure_threshold() {
        for &p in &[0.0_f32, 0.1, 0.25, 0.499] {
            assert_eq!(
                effective_budget_under_pressure(20, p),
                20,
                "pressure={p} should not reduce budget"
            );
        }
    }

    /// At exactly the threshold boundary the budget also goes unreduced.
    #[test]
    fn effective_budget_is_unchanged_at_threshold_boundary() {
        // 0.5 maps to excess=0 → factor=1.0 → budget unchanged.
        assert_eq!(effective_budget_under_pressure(10, 0.5), 10);
    }

    /// At maximum pressure (1.0) the budget is reduced by at most 30 %.
    ///
    /// For nominal=10 the theoretical minimum is `ceil(10 × 0.70) = 7`.
    #[test]
    fn effective_budget_is_reduced_by_up_to_thirty_percent_at_maximum_pressure() {
        let nominal = 10;
        let effective = effective_budget_under_pressure(nominal, 1.0);
        // ceil(10 × 0.70) = 7
        assert_eq!(effective, 7, "30% reduction of 10 → 7 (ceiling)");
        // Must be strictly less than nominal to confirm eviction is more aggressive.
        assert!(effective < nominal);
    }

    /// The effective budget is monotone non-increasing with pressure.
    #[test]
    fn effective_budget_is_monotone_non_increasing_with_pressure() {
        let nominal = 100;
        let pressures: Vec<f32> = (0..=20).map(|i| i as f32 * 0.05).collect();
        let budgets: Vec<usize> = pressures
            .iter()
            .map(|&p| effective_budget_under_pressure(nominal, p))
            .collect();
        for window in budgets.windows(2) {
            assert!(
                window[0] >= window[1],
                "budget should not increase as pressure rises: {} → {}",
                window[0],
                window[1]
            );
        }
    }

    /// The effective budget is always at least 1, even under maximum pressure.
    #[test]
    fn effective_budget_is_at_least_one_at_any_pressure() {
        for nominal in [0, 1, 2, 5] {
            let effective = effective_budget_under_pressure(nominal, 1.0);
            assert!(
                effective >= 1,
                "nominal={nominal}: effective budget must be ≥ 1, got {effective}"
            );
        }
    }

    // ── S5.7.6 — gate_working_context_with_signals tests ─────────────────────

    fn neutral_signals() -> InteroceptiveSignals {
        InteroceptiveSignals::neutral()
    }

    fn high_pressure_signals() -> InteroceptiveSignals {
        InteroceptiveSignals::neutral().with_memory_pressure(1.0)
    }

    /// At neutral signals (pressure=0) no modulation entry is written.
    #[test]
    fn no_modulation_entry_at_low_memory_pressure() {
        let mut ctrl = KvController::with_pre_trained_weights();
        let mut log = AuditLog::new();
        let blocks = make_blocks(10, &[]);

        gate_working_context_with_signals(
            &mut ctrl,
            &blocks,
            5,
            &neutral_signals(),
            "agent",
            "task",
            &mut log,
        );

        // Only the normal KvGatePass entry should be present.
        assert_eq!(log.len(), 1, "neutral pressure: expected only KvGatePass");
        assert!(
            matches!(log.entries()[0], AuditEntry::KvGatePass { .. }),
            "expected KvGatePass, got {:?}",
            log.entries()[0]
        );
    }

    /// At high pressure a `KvMemoryPressureModulation` entry precedes the gate pass.
    #[test]
    fn high_memory_pressure_logs_modulation_entry_before_gate_pass() {
        let mut ctrl = KvController::with_pre_trained_weights();
        let mut log = AuditLog::new();
        let blocks = make_blocks(10, &[]);

        gate_working_context_with_signals(
            &mut ctrl,
            &blocks,
            10,
            &high_pressure_signals(),
            "a",
            "t",
            &mut log,
        );

        assert_eq!(
            log.len(),
            2,
            "expected KvMemoryPressureModulation + KvGatePass"
        );

        // Modulation entry comes first.
        match &log.entries()[0] {
            AuditEntry::KvMemoryPressureModulation {
                memory_pressure,
                nominal_budget,
                effective_budget,
                ..
            } => {
                assert!((*memory_pressure - 1.0).abs() < 1e-6);
                assert_eq!(*nominal_budget, 10);
                assert!(*effective_budget < 10, "budget should be reduced");
                assert!(*effective_budget >= 1);
            }
            other => panic!("expected KvMemoryPressureModulation, got {other:?}"),
        }

        // Gate pass entry follows.
        assert!(
            matches!(log.entries()[1], AuditEntry::KvGatePass { .. }),
            "second entry should be KvGatePass"
        );
    }

    /// High memory pressure results in fewer retained blocks than low pressure.
    ///
    /// This is the primary behavioural assertion for S5.7.6: "eviction becomes
    /// more aggressive under pressure."
    #[test]
    fn high_memory_pressure_retains_fewer_blocks_than_low_pressure() {
        let blocks = make_blocks(20, &[]);
        let nominal_budget = 14;

        let mut ctrl_low = KvController::with_pre_trained_weights();
        let mut log_low = AuditLog::new();
        let result_low = gate_working_context_with_signals(
            &mut ctrl_low,
            &blocks,
            nominal_budget,
            &neutral_signals(),
            "a",
            "t",
            &mut log_low,
        );

        let mut ctrl_high = KvController::with_pre_trained_weights();
        let mut log_high = AuditLog::new();
        let result_high = gate_working_context_with_signals(
            &mut ctrl_high,
            &blocks,
            nominal_budget,
            &high_pressure_signals(),
            "a",
            "t",
            &mut log_high,
        );

        assert!(
            result_high.retained_count() < result_low.retained_count(),
            "high pressure ({}) should retain fewer blocks than low pressure ({})",
            result_high.retained_count(),
            result_low.retained_count()
        );
    }

    /// The modulation audit entry carries the correct pressure and budget values.
    #[test]
    fn modulation_entry_carries_correct_budget_fields() {
        let mut ctrl = KvController::with_pre_trained_weights();
        let mut log = AuditLog::new();
        let blocks = make_blocks(10, &[]);
        let nominal = 10;
        let signals = InteroceptiveSignals::neutral().with_memory_pressure(1.0);

        gate_working_context_with_signals(
            &mut ctrl, &blocks, nominal, &signals, "agent", "task", &mut log,
        );

        if let AuditEntry::KvMemoryPressureModulation {
            agent_id,
            task_id,
            nominal_budget,
            effective_budget,
            ..
        } = &log.entries()[0]
        {
            assert_eq!(agent_id, "agent");
            assert_eq!(task_id, "task");
            assert_eq!(*nominal_budget, nominal);
            // effective_budget must equal effective_budget_under_pressure(10, 1.0) = 7
            assert_eq!(
                *effective_budget,
                effective_budget_under_pressure(nominal, 1.0)
            );
        } else {
            panic!("first entry should be KvMemoryPressureModulation");
        }
    }

    /// Pressure at exactly 0.5 does not trigger a modulation entry (no budget change).
    #[test]
    fn boundary_pressure_at_half_does_not_trigger_modulation_entry() {
        let mut ctrl = KvController::with_pre_trained_weights();
        let mut log = AuditLog::new();
        let blocks = make_blocks(10, &[]);
        let signals = InteroceptiveSignals::neutral().with_memory_pressure(0.5);

        gate_working_context_with_signals(&mut ctrl, &blocks, 10, &signals, "a", "t", &mut log);

        // Pressure 0.5 maps to effective_budget(10, 0.5) = 10 → no reduction → no modulation entry.
        assert_eq!(
            log.len(),
            1,
            "pressure=0.5 does not reduce budget; expect only KvGatePass"
        );
    }
}
