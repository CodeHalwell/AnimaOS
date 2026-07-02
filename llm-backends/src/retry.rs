//! Bounded retry with exponential backoff for transient LLM-provider failures
//! (IO-2).
//!
//! The live provider clients ([`crate::compat`], [`crate::ollama`],
//! [`crate::hub`]) issue blocking `ureq` requests. A single connect blip, a
//! `429 Too Many Requests`, or a `5xx` from the provider previously aborted the
//! whole completion. [`with_retry`] wraps the request so those transient
//! failures are retried a bounded number of times with jittered exponential
//! backoff, while genuine client errors (4xx other than 429) fail fast.

use std::time::Duration;

/// Bounded exponential-backoff retry policy.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Total attempts including the first (so `3` means one try + two retries).
    pub max_attempts: u32,
    /// Base backoff delay in milliseconds (doubled each retry, capped).
    pub base_delay_ms: u64,
    /// Maximum backoff delay in milliseconds.
    pub max_delay_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay_ms: 250,
            max_delay_ms: 8_000,
        }
    }
}

/// Returns `true` for `ureq` errors worth retrying: rate-limit / server statuses
/// (429, 500, 502, 503, 504) and any transport-level failure (connect, timeout,
/// DNS, I/O). Other 4xx client errors are treated as fatal so a bad request is
/// not retried pointlessly.
pub fn is_retryable(err: &ureq::Error) -> bool {
    match err {
        ureq::Error::StatusCode(code) => matches!(*code, 429 | 500 | 502 | 503 | 504),
        // Any non-status error is a transport fault (connect/timeout/DNS/io),
        // which is transient; retrying it is safe under the bounded attempt cap.
        _ => true,
    }
}

/// Computes the backoff (ms) before `attempt`'s retry: exponential on the base
/// delay, capped, with full jitter in `[delay/2, delay]` to avoid thundering
/// herds. Jitter is seeded from the clock to avoid a PRNG dependency.
fn backoff_ms(policy: &RetryPolicy, attempt: u32) -> u64 {
    let shift = (attempt.saturating_sub(1)).min(6);
    let exp = policy.base_delay_ms.saturating_mul(1u64 << shift);
    let capped = exp.min(policy.max_delay_ms).max(1);
    let half = capped / 2;
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    half + (seed % half.max(1))
}

/// Runs `op` with bounded exponential backoff, retrying only on
/// [`is_retryable`] failures. Returns the first success or the last error.
pub fn with_retry<T>(
    policy: &RetryPolicy,
    mut op: impl FnMut() -> Result<T, ureq::Error>,
) -> Result<T, ureq::Error> {
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match op() {
            Ok(value) => return Ok(value),
            Err(err) => {
                if attempt >= policy.max_attempts || !is_retryable(&err) {
                    return Err(err);
                }
                std::thread::sleep(Duration::from_millis(backoff_ms(policy, attempt)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn retryable_classification() {
        assert!(is_retryable(&ureq::Error::StatusCode(429)));
        assert!(is_retryable(&ureq::Error::StatusCode(503)));
        assert!(!is_retryable(&ureq::Error::StatusCode(400)));
        assert!(!is_retryable(&ureq::Error::StatusCode(404)));
    }

    #[test]
    fn succeeds_without_retry() {
        let calls = Cell::new(0);
        let r: Result<u8, ureq::Error> = with_retry(&RetryPolicy::default(), || {
            calls.set(calls.get() + 1);
            Ok(7)
        });
        assert_eq!(r.unwrap(), 7);
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn retries_then_succeeds() {
        let calls = Cell::new(0);
        let policy = RetryPolicy {
            max_attempts: 3,
            base_delay_ms: 1,
            max_delay_ms: 2,
        };
        let r: Result<u8, ureq::Error> = with_retry(&policy, || {
            calls.set(calls.get() + 1);
            if calls.get() < 3 {
                Err(ureq::Error::StatusCode(503))
            } else {
                Ok(1)
            }
        });
        assert_eq!(r.unwrap(), 1);
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn stops_after_max_attempts() {
        let calls = Cell::new(0);
        let policy = RetryPolicy {
            max_attempts: 2,
            base_delay_ms: 1,
            max_delay_ms: 2,
        };
        let r: Result<u8, ureq::Error> = with_retry(&policy, || {
            calls.set(calls.get() + 1);
            Err(ureq::Error::StatusCode(500))
        });
        assert!(r.is_err());
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn does_not_retry_fatal_status() {
        let calls = Cell::new(0);
        let r: Result<u8, ureq::Error> = with_retry(&RetryPolicy::default(), || {
            calls.set(calls.get() + 1);
            Err(ureq::Error::StatusCode(400))
        });
        assert!(r.is_err());
        assert_eq!(calls.get(), 1);
    }
}
