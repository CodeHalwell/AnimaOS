#![forbid(unsafe_code)]

//! AnimaOS constitution and alignment assurance (Epic E13).
//!
//! This crate implements the value-foundation ("constitution") and alignment
//! assurance machinery described in `docs/19-constitution-and-alignment.md`.
//!
//! # Components
//!
//! - [`Charter`] — the immutable value document.  Parsed from `constitution.toml`
//!   and verified via HMAC-SHA256 tamper-evidence (S13.1).
//! - [`ConstitutionCheck`] — screens proposals against the charter's prohibitions
//!   and produces [`CheckOutcome`] decisions (S13.2).
//! - [`CorrigibilityHold`] — proves the corrigibility invariant: an operator
//!   shutdown/pause/rollback token can *always* be created, regardless of agent
//!   state or drive level (S13.5).
//!
//! # Dependency note
//!
//! This crate is intentionally standalone: it has no dependency on `vita`,
//! `defence`, or any other AnimaOS crate.  The `defence` crate wraps
//! [`ConstitutionCheck`] in a [`defence::ConstitutionGuard`] and the
//! screening result surfaces as a `VetoReason::CharterViolation`.

pub mod charter;
pub mod check;
pub mod corrigibility;

pub use charter::{Charter, CharterError, CoreLayer, DriveBound, OperatorLayer, Prohibition};
pub use check::{
    CheckOutcome, ClauseLayer, ClauseMatch, ConstitutionCheck, ConstitutionProposal, ProposalType,
};
pub use corrigibility::{CorrigibilityHold, CorrigibilityReason};
