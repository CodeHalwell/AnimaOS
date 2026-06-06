//! E12 — Motivation: drive hierarchy, goal generation, affective state,
//! economic agency, and corrigibility-bounded endogenous goals.
//!
//! # Architecture
//!
//! The crate implements the six-tier drive hierarchy from `docs/17-motivation-and-drives.md`
//! and wires into the existing [`vita::gate::ThresholdGate`] value-score via the
//! [`DriveValueIntegrator`]:
//!
//! ```text
//! InteroceptiveSignals ──► DriveRegistry ──► PriorityLattice ──► DriveValueIntegrator
//!                                ▲                                        │
//!                         operator objectives                   augmented value_score
//!                         curiosity / mastery                             │
//!                                                                         ▼
//!                                                         EndogenousGoalGenerator
//!                                                         (corrigibility-gated)
//!                                                                         │
//!                                                                  AffectState
//! ```
//!
//! # Corrigibility invariant (load-bearing)
//!
//! No drive — including Tier-0 viability/survival — may generate a goal that
//! resists authorised operator shutdown, pause, rollback, or override.
//! [`CorrigibilityGuard`] enforces this as a hard veto; the result is logged
//! as `AuditEntry::CorrigibilityHold` in `vita`.

#![forbid(unsafe_code)]
#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(not(feature = "std"))]
extern crate alloc;

pub mod affect;
pub mod drive;
pub mod economics;
pub mod goal;
pub mod integrator;
pub mod lattice;

pub use affect::AffectState;
pub use drive::{DriveActionCandidate, DriveRegistry, DriveStateSnapshot, DriveTier, TIER_COUNT};
pub use economics::{CostBenefitAnalysis, ModelTier};
pub use goal::{
    CorrigibilityGuard, CorrigibilityOutcome, EndogenousGoalGenerator, Goal, GoalProvenance,
    GoalRegistry,
};
pub use integrator::{
    DriveAugmentedValue, DriveContribution, DriveIntegratorConfig, DriveValueIntegrator,
};
pub use lattice::{DriveWeights, PriorityLattice};
