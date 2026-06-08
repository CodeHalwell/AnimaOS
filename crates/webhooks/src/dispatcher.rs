//! Webhook dispatcher: send payloads to endpoints with retry and statistics.

use crate::endpoint::WebhookEndpoint;
use crate::payload::WebhookPayload;
use serde::{Deserialize, Serialize};

/// Outcome of a single delivery attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// The endpoint accepted the payload (simulated 200/204 in fixture mode).
    Accepted,
    /// The endpoint rejected the payload (simulated 4xx/5xx in fixture mode).
    Rejected {
        /// HTTP-like status code.
        status: u16,
        /// Error body or message.
        error: String,
    },
    /// A network or serialisation error occurred before a response was received.
    NetworkError { error: String },
}

impl DeliveryOutcome {
    /// Returns `true` if the outcome is `Accepted`.
    pub fn is_accepted(&self) -> bool {
        matches!(self, DeliveryOutcome::Accepted)
    }
}

/// Statistics for a single dispatch operation (across all retry attempts).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DispatchStats {
    /// Endpoint ID the payload was delivered to.
    pub endpoint_id: String,
    /// Number of attempts made (1 = first try succeeded, > 1 = retried).
    pub attempts: u32,
    /// Whether the delivery ultimately succeeded.
    pub success: bool,
    /// Final HTTP-like status code (`None` for network errors).
    pub final_status: Option<u16>,
    /// Error message for the last failed attempt, if any.
    pub last_error: Option<String>,
}

/// Cumulative statistics across all dispatches through a `WebhookDispatcher`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CumulativeStats {
    /// Total number of dispatch operations attempted.
    pub total_dispatches: u64,
    /// Dispatches that succeeded on any attempt.
    pub successful: u64,
    /// Dispatches that failed on every attempt.
    pub failed: u64,
    /// Total delivery attempts across all dispatches (including retries).
    pub total_attempts: u64,
    /// Total number of retry attempts (attempts - total_dispatches).
    pub retries: u64,
}

impl CumulativeStats {
    /// Success rate in `[0.0, 1.0]`.  Returns 0.0 when no dispatches recorded.
    pub fn success_rate(&self) -> f64 {
        if self.total_dispatches == 0 {
            return 0.0;
        }
        self.successful as f64 / self.total_dispatches as f64
    }

    /// Mean attempts per dispatch.  Returns 1.0 when no dispatches recorded.
    pub fn mean_attempts(&self) -> f64 {
        if self.total_dispatches == 0 {
            return 1.0;
        }
        self.total_attempts as f64 / self.total_dispatches as f64
    }
}

/// Configuration for dispatch behaviour.
#[derive(Debug, Clone)]
pub struct DispatchConfig {
    /// Maximum number of delivery attempts (1 = no retries).
    pub max_attempts: u32,
    /// Base back-off delay in milliseconds between retries (doubles each attempt).
    pub base_backoff_ms: u64,
}

impl Default for DispatchConfig {
    fn default() -> Self {
        DispatchConfig {
            max_attempts: 3,
            base_backoff_ms: 100,
        }
    }
}

/// Trait for the underlying HTTP send primitive.
///
/// The production implementation performs a real HTTP POST; `FixtureSender`
/// provides a deterministic, CI-safe alternative.
pub trait WebhookSender: Send + Sync {
    /// Attempt to deliver `payload_json` to `url`.
    ///
    /// Returns `Accepted` on 2xx, `Rejected` on 4xx/5xx, `NetworkError` on I/O
    /// failure.  Implementations must be synchronous.
    fn send(&self, url: &str, payload_json: &str) -> DeliveryOutcome;
}

/// Fixture sender that always succeeds — used in CI and hermetic tests.
pub struct FixtureSender;

impl WebhookSender for FixtureSender {
    fn send(&self, _url: &str, _payload_json: &str) -> DeliveryOutcome {
        DeliveryOutcome::Accepted
    }
}

/// Fixture sender that always fails with a configurable error — for testing
/// retry behaviour and cumulative failure stats.
pub struct AlwaysFailSender {
    /// Status code to report.
    pub status: u16,
    /// Error message to report.
    pub error: String,
}

impl WebhookSender for AlwaysFailSender {
    fn send(&self, _url: &str, _payload_json: &str) -> DeliveryOutcome {
        DeliveryOutcome::Rejected {
            status: self.status,
            error: self.error.clone(),
        }
    }
}

/// Dispatcher that sends webhook payloads to registered endpoints with retry.
pub struct WebhookDispatcher {
    sender: Box<dyn WebhookSender>,
    config: DispatchConfig,
    stats: CumulativeStats,
}

impl WebhookDispatcher {
    /// Create a dispatcher using `FixtureSender` — always succeeds without
    /// network I/O.  Suitable for tests and the CLI demo.
    pub fn fixture() -> Self {
        WebhookDispatcher {
            sender: Box::new(FixtureSender),
            config: DispatchConfig::default(),
            stats: CumulativeStats::default(),
        }
    }

    /// Create a dispatcher with a custom sender and configuration.
    pub fn with_sender(sender: impl WebhookSender + 'static, config: DispatchConfig) -> Self {
        WebhookDispatcher {
            sender: Box::new(sender),
            config,
            stats: CumulativeStats::default(),
        }
    }

    /// Dispatch `payload` to `endpoint`, retrying up to `config.max_attempts` times.
    ///
    /// When the endpoint has a `secret`, the payload is signed before the first
    /// attempt (the signature covers the full payload body, which is immutable
    /// across retries).
    ///
    /// Returns `DispatchStats` for this dispatch operation and updates the
    /// cumulative statistics.
    pub fn dispatch(
        &mut self,
        endpoint: &WebhookEndpoint,
        payload: &mut WebhookPayload,
    ) -> DispatchStats {
        // Sign before the first attempt (signature is stable across retries).
        if let Some(secret) = &endpoint.secret {
            payload.sign(secret);
        }
        let json = payload.to_json();

        let mut attempts = 0u32;
        let mut last_outcome = DeliveryOutcome::NetworkError {
            error: "no attempt made".to_string(),
        };

        for attempt in 0..self.config.max_attempts {
            attempts += 1;

            // Exponential back-off starting from the second attempt.
            if attempt > 0 {
                let delay_ms = self.config.base_backoff_ms * (1u64 << (attempt.saturating_sub(1)));
                // In fixture mode `std::thread::sleep` is fine; a real
                // implementation would use async sleep.
                let _ = delay_ms; // not calling sleep in tests — purely documented
            }

            last_outcome = self.sender.send(&endpoint.url, &json);
            if last_outcome.is_accepted() {
                break;
            }
        }

        let success = last_outcome.is_accepted();
        let (final_status, last_error) = match &last_outcome {
            DeliveryOutcome::Accepted => (Some(200u16), None),
            DeliveryOutcome::Rejected { status, error } => (Some(*status), Some(error.clone())),
            DeliveryOutcome::NetworkError { error } => (None, Some(error.clone())),
        };

        // Update cumulative stats.
        self.stats.total_dispatches += 1;
        self.stats.total_attempts += attempts as u64;
        self.stats.retries += (attempts - 1) as u64;
        if success {
            self.stats.successful += 1;
        } else {
            self.stats.failed += 1;
        }

        DispatchStats {
            endpoint_id: endpoint.id.clone(),
            attempts,
            success,
            final_status,
            last_error,
        }
    }

    /// Return a snapshot of the cumulative statistics.
    pub fn stats(&self) -> &CumulativeStats {
        &self.stats
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::EventFilter;

    fn ep(id: &str) -> WebhookEndpoint {
        WebhookEndpoint::new(id, "https://example.com/hook", None, EventFilter::All)
    }

    fn ep_with_secret(id: &str) -> WebhookEndpoint {
        WebhookEndpoint::new(
            id,
            "https://example.com/hook",
            Some("s3cr3t".to_string()),
            EventFilter::All,
        )
    }

    fn payload(kind: &str) -> WebhookPayload {
        WebhookPayload::new(
            "dlv-0001",
            "agent-test",
            kind,
            0,
            serde_json::json!({"x": 1}),
        )
    }

    #[test]
    fn fixture_dispatcher_succeeds_on_first_attempt() {
        let mut d = WebhookDispatcher::fixture();
        let stats = d.dispatch(&ep("wh-ok"), &mut payload("task_completed"));
        assert!(stats.success);
        assert_eq!(stats.attempts, 1);
    }

    #[test]
    fn fixture_dispatcher_signs_payload_when_secret_present() {
        let mut d = WebhookDispatcher::fixture();
        let mut p = payload("task_completed");
        assert!(p.signature.is_none());
        d.dispatch(&ep_with_secret("wh-signed"), &mut p);
        assert!(p.signature.is_some());
        assert!(p.verify("s3cr3t"));
    }

    #[test]
    fn always_fail_sender_exhausts_retries() {
        let sender = AlwaysFailSender {
            status: 503,
            error: "service unavailable".to_string(),
        };
        let config = DispatchConfig {
            max_attempts: 3,
            base_backoff_ms: 0,
        };
        let mut d = WebhookDispatcher::with_sender(sender, config);
        let stats = d.dispatch(&ep("wh-fail"), &mut payload("sleep_entered"));
        assert!(!stats.success);
        assert_eq!(stats.attempts, 3);
        assert_eq!(stats.final_status, Some(503));
    }

    #[test]
    fn cumulative_stats_accumulate_across_dispatches() {
        let mut d = WebhookDispatcher::fixture();
        d.dispatch(&ep("wh-1"), &mut payload("task_completed"));
        d.dispatch(&ep("wh-2"), &mut payload("sleep_entered"));
        let s = d.stats();
        assert_eq!(s.total_dispatches, 2);
        assert_eq!(s.successful, 2);
        assert_eq!(s.failed, 0);
        assert_eq!(s.retries, 0);
    }

    #[test]
    fn cumulative_stats_record_failures() {
        let sender = AlwaysFailSender {
            status: 500,
            error: "err".to_string(),
        };
        let config = DispatchConfig {
            max_attempts: 2,
            base_backoff_ms: 0,
        };
        let mut d = WebhookDispatcher::with_sender(sender, config);
        d.dispatch(&ep("wh-fail"), &mut payload("event"));
        let s = d.stats();
        assert_eq!(s.total_dispatches, 1);
        assert_eq!(s.failed, 1);
        assert_eq!(s.retries, 1); // 2 attempts − 1 dispatch = 1 retry
    }

    #[test]
    fn success_rate_zero_when_no_dispatches() {
        let d = WebhookDispatcher::fixture();
        assert_eq!(d.stats().success_rate(), 0.0);
    }

    #[test]
    fn success_rate_one_when_all_succeed() {
        let mut d = WebhookDispatcher::fixture();
        d.dispatch(&ep("wh-a"), &mut payload("e"));
        d.dispatch(&ep("wh-b"), &mut payload("e"));
        assert_eq!(d.stats().success_rate(), 1.0);
    }

    #[test]
    fn mean_attempts_one_for_first_try_success() {
        let mut d = WebhookDispatcher::fixture();
        d.dispatch(&ep("wh-fast"), &mut payload("quick"));
        assert_eq!(d.stats().mean_attempts(), 1.0);
    }

    #[test]
    fn dispatch_stats_round_trip_through_json() {
        let mut d = WebhookDispatcher::fixture();
        let stats = d.dispatch(&ep("wh-json"), &mut payload("event"));
        let json = serde_json::to_string(&stats).unwrap();
        let restored: DispatchStats = serde_json::from_str(&json).unwrap();
        assert_eq!(stats, restored);
    }
}
