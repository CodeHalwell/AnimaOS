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
