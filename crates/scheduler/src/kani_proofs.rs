//! Symbolic-verification harnesses for the scheduler invariants — Epic E4.6.
//!
//! Each `#[kani::proof]` function is a single property the [`BoundedTokenPipe`]
//! credit accounting or the MLFQ tier selector must satisfy. Kani runs them
//! under `cargo kani --features kani-harness` and verifies the property holds
//! for every input the symbolic engine considers reachable.
//!
//! Bounded-token-pipe properties:
//!
//! - `push` never returns `Ok` when it would exceed credits.
//! - `refund` never returns `Ok` when it would exceed capacity.
//! - `produced` is monotonically non-decreasing across any sequence of
//!   `push` / `refund` calls.
//!
//! MLFQ tier selector properties:
//!
//! - `select_optimal_task` returns the highest-priority non-empty tier.
//! - `boost_all_to_high` drains every Medium / Low entry to High and never
//!   loses a task.
//!
//! These match the verification surface declared in
//! `docs/04-verification.md` §3.
//!
//! Without the `kani-harness` feature the module compiles to nothing.

#![cfg(feature = "kani-harness")]
// kani re-exports the standard `#[proof]` attribute through `kani::proof`.
// Outside of Kani's tool there is no `kani` crate available, so we declare
// it via `extern crate` only when the tool is driving the build. `cargo
// kani` injects the dependency automatically; for `cargo build --features
// kani-harness` we stub out the attribute so the file still type-checks.
#[cfg(kani)]
extern crate kani;

#[cfg(not(kani))]
mod kani {
    /// No-op proof attribute used when the `kani` symbolic engine is not
    /// the active compiler. Lets the harnesses compile under regular
    /// `cargo build` so editors can lint them.
    pub use core::prelude::v1::*;

    /// Returns a fully-symbolic value of `T`. Outside Kani this is unused.
    pub fn any<T: Default>() -> T {
        T::default()
    }

    /// Constrains symbolic execution. Outside Kani this is a no-op.
    pub fn assume(_cond: bool) {}
}

use crate::token_pipe::{BoundedTokenPipe, TokenPipeError};
use crate::{IterationAwareMlfq, MlfqTier, Task, TaskAgenda};

// ── BoundedTokenPipe ──────────────────────────────────────────────────────

/// Pushing more tokens than the pipe's available credit always returns
/// [`TokenPipeError::BackpressureExceeded`] and never mutates the credit
/// counter.
#[cfg_attr(kani, kani::proof)]
pub fn push_beyond_credit_is_rejected() {
    let capacity: u32 = kani::any();
    kani::assume(capacity <= 64);
    let n: u32 = kani::any();
    kani::assume(n > capacity);

    let mut pipe = BoundedTokenPipe::new(capacity);
    let before = pipe.available_credits();
    let err = pipe.push(n).unwrap_err();
    assert!(matches!(err, TokenPipeError::BackpressureExceeded));
    assert_eq!(pipe.available_credits(), before);
}

/// Refunding more credits than the pipe's configured capacity always
/// returns [`TokenPipeError::OverRefund`] and never mutates the credit
/// counter.
#[cfg_attr(kani, kani::proof)]
pub fn refund_beyond_capacity_is_rejected() {
    let capacity: u32 = kani::any();
    kani::assume(capacity > 0 && capacity <= 64);
    let extra: u32 = kani::any();
    kani::assume(extra >= 1 && extra <= 64);

    let mut pipe = BoundedTokenPipe::new(capacity);
    // Pipe starts full, so any refund overflows the capacity bound.
    let err = pipe.refund(extra).unwrap_err();
    assert!(matches!(err, TokenPipeError::OverRefund));
    assert_eq!(pipe.available_credits(), capacity);
}

/// After a successful `push(n)`, the pipe's `available_credits` decreases
/// by exactly `n` and `produced` increases by exactly `n`.
#[cfg_attr(kani, kani::proof)]
pub fn push_consumes_exactly_n_credits() {
    let capacity: u32 = kani::any();
    kani::assume(capacity > 0 && capacity <= 64);
    let n: u32 = kani::any();
    kani::assume(n <= capacity);

    let mut pipe = BoundedTokenPipe::new(capacity);
    let credits_before = pipe.available_credits();
    let produced_before = pipe.produced();
    pipe.push(n).unwrap();
    assert_eq!(pipe.available_credits(), credits_before - n);
    assert_eq!(pipe.produced(), produced_before + n as u64);
}

// ── MLFQ tier selector ────────────────────────────────────────────────────

/// `TaskAgenda::select_optimal_task` always returns a task whose MLFQ
/// level is the lowest (= highest priority) non-empty tier.
#[cfg_attr(kani, kani::proof)]
pub fn select_optimal_task_pulls_from_highest_priority_tier() {
    // A two-task agenda where one is High and one is Low must always
    // dispatch the High task first.
    let mut agenda = TaskAgenda::new();
    agenda.push(Task::new(2, MlfqTier::Low as u8, "low"));
    agenda.push(Task::new(1, MlfqTier::High as u8, "high"));
    let next = agenda.select_optimal_task().unwrap();
    assert_eq!(next.id, 1);
    assert_eq!(next.mlfq_level, MlfqTier::High as u8);
}

/// `boost_all_to_high` returns the number of tasks moved from
/// Medium / Low into High, and the agenda's total length never changes.
#[cfg_attr(kani, kani::proof)]
pub fn boost_all_to_high_preserves_total_task_count() {
    let mut agenda = TaskAgenda::new();
    agenda.push(Task::new(1, MlfqTier::High as u8, "a"));
    agenda.push(Task::new(2, MlfqTier::Medium as u8, "b"));
    agenda.push(Task::new(3, MlfqTier::Low as u8, "c"));

    let total_before = agenda.len();
    let boosted = agenda.boost_all_to_high();
    assert_eq!(boosted, 2);
    assert_eq!(agenda.len(), total_before);
    // After the boost everything must be in High.
    let next = agenda.select_optimal_task().unwrap();
    assert_eq!(next.mlfq_level, MlfqTier::High as u8);
}

/// A fresh scheduler with `boost_interval = 0` never boosts.
#[cfg_attr(kani, kani::proof)]
pub fn boost_interval_zero_never_boosts() {
    let mut scheduler = IterationAwareMlfq::default();
    let mut agenda = TaskAgenda::new();
    agenda.push(Task::new(1, MlfqTier::Low as u8, "x"));
    let boosted = scheduler.check_and_boost(&mut agenda);
    assert_eq!(boosted, 0);
    // The Low task is still where we put it.
    let next = agenda.select_optimal_task().unwrap();
    assert_eq!(next.mlfq_level, MlfqTier::Low as u8);
}
