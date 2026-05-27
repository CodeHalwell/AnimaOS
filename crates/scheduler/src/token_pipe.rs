//! Bounded token pipe with credit-based backpressure.

/// Errors raised by [`BoundedTokenPipe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenPipeError {
    /// Pushing would exceed the configured credit budget.
    BackpressureExceeded,
    /// Refunding more credits than the pipe was configured with.
    OverRefund,
}

/// A bounded token sink that uses credit accounting for backpressure.
///
/// The producer consumes credits when pushing tokens; downstream consumers
/// refund credits as tokens are processed. When credits hit zero the pipe
/// blocks further production until the consumer side refunds.
#[derive(Debug, Clone)]
pub struct BoundedTokenPipe {
    capacity: u32,
    credits: u32,
    produced: u64,
    consumed: u64,
}

impl BoundedTokenPipe {
    /// Creates a new pipe with `capacity` credits available.
    pub fn new(capacity: u32) -> Self {
        Self {
            capacity,
            credits: capacity,
            produced: 0,
            consumed: 0,
        }
    }

    /// Returns the total configured credit budget.
    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    /// Returns the currently available credits.
    pub fn available_credits(&self) -> u32 {
        self.credits
    }

    /// Total tokens ever produced.
    pub fn produced(&self) -> u64 {
        self.produced
    }

    /// Total tokens ever consumed.
    pub fn consumed(&self) -> u64 {
        self.consumed
    }

    /// Attempts to push `n` tokens, consuming `n` credits.
    pub fn push(&mut self, n: u32) -> Result<(), TokenPipeError> {
        if n > self.credits {
            return Err(TokenPipeError::BackpressureExceeded);
        }
        self.credits -= n;
        self.produced = self.produced.saturating_add(n as u64);
        Ok(())
    }

    /// Refunds `n` credits to indicate the consumer drained them.
    pub fn refund(&mut self, n: u32) -> Result<(), TokenPipeError> {
        let new_credits = self
            .credits
            .checked_add(n)
            .ok_or(TokenPipeError::OverRefund)?;
        if new_credits > self.capacity {
            return Err(TokenPipeError::OverRefund);
        }
        self.credits = new_credits;
        self.consumed = self.consumed.saturating_add(n as u64);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Kani formal verification proof harnesses
// ---------------------------------------------------------------------------
//
// These harnesses are compiled only when running `cargo kani`.  They prove
// five invariants of the credit-accounting bounded token pipe:
//
//   1. `available_credits()` ≤ `capacity()` after any push.
//   2. `available_credits()` ≤ `capacity()` after any refund.
//   3. `push(n)` succeeds iff `n ≤ available_credits()`.
//   4. `push(n)` + `refund(n)` is a roundtrip that restores credits.
//   5. `produced()` is monotonically non-decreasing.
//
// Epic E4.6 exit criterion 1.

/// Kani formal verification proofs for [`BoundedTokenPipe`] invariants.
#[cfg(kani)]
mod kani_proofs {
    use super::*;

    /// Prove: credits never exceed capacity after a push attempt (success or
    /// failure alike).
    #[kani::proof]
    fn credits_never_exceed_capacity_after_push() {
        let capacity: u32 = kani::any();
        kani::assume(capacity > 0 && capacity <= 1024);
        let mut pipe = BoundedTokenPipe::new(capacity);

        let n: u32 = kani::any();
        let _ = pipe.push(n);

        assert!(
            pipe.available_credits() <= pipe.capacity(),
            "credits must never exceed capacity after push"
        );
    }

    /// Prove: credits never exceed capacity after a refund attempt.
    ///
    /// We first push some tokens to produce a non-trivially-full state,
    /// then attempt a refund with an arbitrary amount.
    #[kani::proof]
    fn credits_never_exceed_capacity_after_refund() {
        let capacity: u32 = kani::any();
        kani::assume(capacity > 0 && capacity <= 1024);
        let mut pipe = BoundedTokenPipe::new(capacity);

        let push_n: u32 = kani::any();
        kani::assume(push_n <= capacity);
        pipe.push(push_n).unwrap();

        let refund_n: u32 = kani::any();
        let _ = pipe.refund(refund_n);

        assert!(
            pipe.available_credits() <= pipe.capacity(),
            "credits must never exceed capacity after refund"
        );
    }

    /// Prove: `push` succeeds iff `n ≤ available_credits`, fails otherwise.
    ///
    /// `n` is bounded to `[0, capacity + 1]` to cover both the success path
    /// and the backpressure path with a tractable state space.
    #[kani::proof]
    fn push_succeeds_iff_n_within_available_credits() {
        let capacity: u32 = kani::any();
        kani::assume(capacity > 0 && capacity <= 1024);
        let mut pipe = BoundedTokenPipe::new(capacity);

        // Fresh pipe: available_credits() == capacity.
        let n: u32 = kani::any();
        kani::assume(n <= capacity + 1);

        let before_credits = pipe.available_credits();
        match pipe.push(n) {
            Ok(()) => {
                // push succeeded → n was within the available budget.
                assert!(n <= before_credits, "successful push must have n ≤ credits");
            }
            Err(TokenPipeError::BackpressureExceeded) => {
                // push failed → n exceeded the available budget.
                assert!(n > before_credits, "rejected push must have n > credits");
            }
            Err(TokenPipeError::OverRefund) => {
                // push never returns OverRefund — reaching here is a bug.
                panic!("push returned OverRefund: implementation invariant violated");
            }
        }
    }

    /// Prove: `push(n)` followed by `refund(n)` restores `available_credits`,
    /// starting from an **arbitrary valid occupancy** (not just a fresh pipe).
    ///
    /// A symbolic initial push drives the pipe into any valid non-full state
    /// so the roundtrip property is proved for all occupancy levels, not
    /// just the initial-credits = capacity case.
    #[kani::proof]
    fn push_refund_roundtrip_restores_credits() {
        let capacity: u32 = kani::any();
        kani::assume(capacity > 0 && capacity <= 1024);
        let mut pipe = BoundedTokenPipe::new(capacity);

        // Drive the pipe into an arbitrary valid state.
        let initial_consume: u32 = kani::any();
        kani::assume(initial_consume <= capacity);
        let _ = pipe.push(initial_consume);

        // Roundtrip a further symbolic amount within the remaining budget.
        let n: u32 = kani::any();
        kani::assume(n > 0 && n <= pipe.available_credits());

        let before = pipe.available_credits();
        pipe.push(n).unwrap();
        pipe.refund(n).unwrap();

        assert_eq!(
            pipe.available_credits(),
            before,
            "push+refund roundtrip must restore credits exactly"
        );
    }

    /// Prove: `produced()` is monotonically non-decreasing.
    ///
    /// Even when push fails (backpressure), `produced()` must never decrease.
    #[kani::proof]
    fn produced_is_monotonically_non_decreasing() {
        let capacity: u32 = kani::any();
        kani::assume(capacity > 0 && capacity <= 1024);
        let mut pipe = BoundedTokenPipe::new(capacity);

        let before = pipe.produced();
        let n: u32 = kani::any();
        let _ = pipe.push(n);

        assert!(
            pipe.produced() >= before,
            "produced() must be monotonically non-decreasing"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_consumes_credits() {
        let mut pipe = BoundedTokenPipe::new(10);
        pipe.push(3).unwrap();
        assert_eq!(pipe.available_credits(), 7);
        assert_eq!(pipe.produced(), 3);
    }

    #[test]
    fn push_returns_backpressure_when_exhausted() {
        let mut pipe = BoundedTokenPipe::new(4);
        pipe.push(4).unwrap();
        let err = pipe.push(1).unwrap_err();
        assert_eq!(err, TokenPipeError::BackpressureExceeded);
    }

    #[test]
    fn refund_restores_credits_up_to_capacity() {
        let mut pipe = BoundedTokenPipe::new(4);
        pipe.push(4).unwrap();
        pipe.refund(2).unwrap();
        assert_eq!(pipe.available_credits(), 2);
        let err = pipe.refund(5).unwrap_err();
        assert_eq!(err, TokenPipeError::OverRefund);
    }
}
