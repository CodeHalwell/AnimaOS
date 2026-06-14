//! Learned KV-cache gating controller — Epic E5.4.
//!
//! # Purpose
//!
//! The KV-cache controller decides **which blocks in the working context should
//! be retained under memory pressure**. It sits between the Thalamic Router
//! (E5.3) and the L1/L2 memory tiers: when a route's `MemoryScope` opts in
//! (`kv_controller: true`), the controller's gate scores replace LRU eviction
//! for that invocation's working-context management.
//!
//! # Architecture
//!
//! ```text
//! InvokeRequest (with MemoryScope::kv_controller = true)
//!          │
//!    KvController::select_blocks(features, budget)
//!          │
//!    KvGateDecision per block  ──► retain or evict
//!          │
//!    On fault ──► ControllerState::Faulted
//!              ──► LRU fallback (recency_score ranking)
//!              ──► AuditEntry::KvControllerFaulted
//! ```
//!
//! # Stories implemented
//!
//! | Story  | Module         | Description |
//! |--------|----------------|-------------|
//! | S5.4.1 | [`controller`] | Linear gate model (BlockGate trait + LinearGate + fault path) |
//! | S5.4.2 | [`trace`]      | Replay-quality logging of gate decisions per invocation |
//! | S5.4.3 | [`training`]   | Offline training-pair compiler and corpus with provenance |
//! | S5.4.4 | [`controller`] + [`vita`] integration | LRU fallback on fault; MemoryScope opt-in |
//! | S5.4.5 | [`eval`]       | Needle-recall benchmark against LRU (exit criteria 1 & 4) |
//!
//! # Exit criteria
//!
//! 1. **≥ 10 pp needle recall advantage** — verified by
//!    [`eval::tests::controller_beats_lru_by_at_least_ten_pp_needle_recall`].
//! 2. **Fault → LRU fallback within next decision** — verified by
//!    [`controller::tests::controller_faults_on_alwaysfault_gate`] and the
//!    vita integration test `kv_controller_fault_is_recorded_in_audit_log`.
//! 3. **Training-data provenance documented** — every [`training::TrainingPair`]
//!    carries a [`trace::TraceProvenance`] tag; aggregate counts are available via
//!    [`training::TrainingCorpus::provenance_summary`].
//! 4. **Ablation: frozen weights ≤ 10 pp over LRU** — verified by
//!    [`eval::tests::ablation_frozen_weights_do_not_beat_lru_by_more_than_noise`].
//!
//! # TurboQuant integration (E2.7)
//!
//! The [`controller::Quantizer`] trait is the integration seam for the
//! TurboQuant substrate from Epic E2.7. The default [`controller::NoQuantizer`]
//! is a transparent pass-through (similarity always = 1.0).
//!
//! With the `turboquant` feature enabled, [`turboquant::TurboQuantizer`]
//! implements [`controller::Quantizer`] backed by the real
//! [`memory::turboquant::TurboQuant`] vector quantiser: the controller's gate
//! score is multiplied by a normalised cosine similarity between the query and
//! the stored quantised block representation, giving the combined
//! controller+TurboQuant retention priority. Because `memory::turboquant` is
//! std-only, the `turboquant` feature implies `std` and is OFF by default, so
//! the no_std gate build is unaffected.

#![forbid(unsafe_code)]
#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc;

pub mod controller;
#[cfg(feature = "std")]
pub mod eval;
pub mod features;
#[cfg(feature = "std")]
pub mod trace;
#[cfg(feature = "std")]
pub mod training;
#[cfg(feature = "turboquant")]
pub mod turboquant;

// ── Public re-exports ─────────────────────────────────────────────────────────

pub use controller::{
    AlwaysFaultGate, BlockGate, ControllerState, ControllerWeights, GateError, KvController,
    KvGateDecision, LinearGate, NoQuantizer, Quantizer,
};
#[cfg(feature = "std")]
pub use eval::{
    run_controller_benchmark, run_controller_benchmark_on_features, run_lru_benchmark,
    run_lru_benchmark_on_features, NeedleBenchmarkConfig, NeedleRecallResult,
};
pub use features::{BlockFeatures, BlockRole};
#[cfg(feature = "std")]
pub use trace::{
    BlockTraceRecord, InvocationTrace, ProvenanceCounts, TraceCapture, TraceConfig, TraceProvenance,
};
#[cfg(feature = "std")]
pub use training::{compile_training_pairs, TrainingCorpus, TrainingPair};
#[cfg(feature = "turboquant")]
pub use turboquant::TurboQuantizer;
