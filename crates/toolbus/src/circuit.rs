//! Circuit breaker isolating failing tool pathways from healthy execution.

use std::time::{Duration, Instant};

/// Isolates failing tool pathways from healthy execution flows.
#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    /// Number of recent consecutive failures.
    pub failure_count: u32,
    /// Current breaker state.
    pub state: BreakerState,
    /// Time of the latest failure.
    pub last_failure: Option<Instant>,
    /// Cooldown before transitioning Open -> HalfOpen.
    pub cooldown: Duration,
}

/// Tool pathway state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    /// Pathway healthy.
    Closed,
    /// Fault detected; execution blocked.
    Open,
    /// Probe state before fully closing again.
    HalfOpen,
}

impl CircuitBreaker {
    /// Creates a new healthy circuit breaker with a 30s default cooldown.
    pub fn new() -> Self {
        Self {
            failure_count: 0,
            state: BreakerState::Closed,
            last_failure: None,
            cooldown: Duration::from_secs(30),
        }
    }

    /// Creates a new breaker with an explicit cooldown duration.
    pub fn with_cooldown(cooldown: Duration) -> Self {
        Self {
            cooldown,
            ..Self::new()
        }
    }

    /// Records a failure and opens the breaker when threshold is exceeded.
    pub fn record_failure(&mut self, open_threshold: u32) {
        self.failure_count = self.failure_count.saturating_add(1);
        self.last_failure = Some(Instant::now());
        if self.failure_count >= open_threshold {
            self.state = BreakerState::Open;
        }
    }

    /// Records a successful invocation, closing the breaker if probing.
    pub fn record_success(&mut self) {
        self.failure_count = 0;
        if self.state == BreakerState::HalfOpen {
            self.state = BreakerState::Closed;
        }
    }

    /// Returns pathway health and transitions Open -> HalfOpen after cooldown.
    pub fn verify_pathway_health(&mut self) -> Result<(), &'static str> {
        if self.state == BreakerState::Open {
            if let Some(last_fail) = self.last_failure {
                if last_fail.elapsed() > self.cooldown {
                    self.state = BreakerState::HalfOpen;
                    return Ok(());
                }
            }
            return Err("Execution pathway blocked by active circuit breaker.");
        }
        Ok(())
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_breaker_blocks_execution_until_cooldown() {
        let mut breaker = CircuitBreaker {
            state: BreakerState::Open,
            last_failure: Some(Instant::now()),
            ..CircuitBreaker::new()
        };
        let result = breaker.verify_pathway_health();
        assert!(result.is_err());
        assert_eq!(breaker.state, BreakerState::Open);
    }

    #[test]
    fn open_breaker_moves_to_half_open_after_cooldown() {
        let mut breaker = CircuitBreaker {
            state: BreakerState::Open,
            last_failure: Some(Instant::now() - Duration::from_secs(31)),
            ..CircuitBreaker::new()
        };
        let result = breaker.verify_pathway_health();
        assert!(result.is_ok());
        assert_eq!(breaker.state, BreakerState::HalfOpen);
    }

    #[test]
    fn record_success_closes_half_open_breaker() {
        let mut breaker = CircuitBreaker {
            state: BreakerState::HalfOpen,
            ..CircuitBreaker::new()
        };
        breaker.record_success();
        assert_eq!(breaker.state, BreakerState::Closed);
        assert_eq!(breaker.failure_count, 0);
    }
}
