#![forbid(unsafe_code)]

//! Per-user token and request rate limiting — Epic E18.
//!
//! Each user that interacts with the agent through any E10 channel is subject
//! to configurable token and request limits based on their [`TrustTier`].
//! [`Operator`]-tier users are never rate-limited; [`Unknown`]-tier users
//! receive the tightest budget by default.
//!
//! # Rolling windows
//!
//! Limits are enforced over two rolling windows:
//! - **Hourly**: a 1-hour sliding window for token consumption and request count.
//! - **Daily**: a 24-hour sliding window for token consumption only.
//!
//! Window entries that have fallen outside the window are lazily drained on
//! each [`UserQuotaTracker::check_and_consume`] call.
//!
//! # Escalation
//!
//! After `policy.escalation_threshold` consecutive quota violations by the same
//! user, [`UserQuotaTracker::should_escalate`] returns `true`.  The caller is
//! responsible for emitting an `AuditEntry::QuotaEscalated` and calling
//! [`UserQuotaTracker::record_escalation`] to start the cooldown timer.

use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};
use users::profile::TrustTier;

// ── Window constants ──────────────────────────────────────────────────────────

const HOUR_NS: u64 = 3_600_000_000_000;
const DAY_NS: u64 = 86_400_000_000_000;

/// Default number of consecutive violations before [`UserQuotaTracker::should_escalate`]
/// returns `true`.
pub const DEFAULT_ESCALATION_THRESHOLD: u32 = 5;

/// Default minimum nanosecond gap between two escalation events for the same user.
pub const DEFAULT_ESCALATION_COOLDOWN_NS: u64 = 300_000_000_000; // 5 minutes

// ── TierLimits ────────────────────────────────────────────────────────────────

/// Per-trust-tier quota limits.
///
/// Use [`TierLimits::UNLIMITED`] for the [`TrustTier::Operator`] tier.
/// A `tokens_per_hour` / `tokens_per_day` / `requests_per_hour` value of
/// [`u64::MAX`] signals "unlimited".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TierLimits {
    /// Maximum tokens that may be consumed in any rolling 1-hour window.
    pub tokens_per_hour: u64,
    /// Maximum tokens that may be consumed in any rolling 24-hour window.
    pub tokens_per_day: u64,
    /// Maximum requests that may be made in any rolling 1-hour window.
    pub requests_per_hour: u64,
}

impl TierLimits {
    /// A sentinel limit that is never exceeded — used for `Operator`-tier users.
    pub const UNLIMITED: Self = Self {
        tokens_per_hour: u64::MAX,
        tokens_per_day: u64::MAX,
        requests_per_hour: u64::MAX,
    };

    /// Returns `true` when all limits are set to the unlimited sentinel.
    pub fn is_unlimited(&self) -> bool {
        self.tokens_per_hour == u64::MAX
            && self.tokens_per_day == u64::MAX
            && self.requests_per_hour == u64::MAX
    }
}

// ── QuotaPolicy ───────────────────────────────────────────────────────────────

/// Per-tier rate-limit configuration.
///
/// The defaults give `Unknown` users a tight budget and `Operator` users
/// unlimited access.  All values are configurable at runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaPolicy {
    /// Limits for [`TrustTier::Unknown`] users.
    pub unknown: TierLimits,
    /// Limits for [`TrustTier::Verified`] users.
    pub verified: TierLimits,
    /// Limits for [`TrustTier::Trusted`] users.
    pub trusted: TierLimits,
    /// Limits for [`TrustTier::Operator`] users — defaults to unlimited.
    pub operator: TierLimits,
    /// Number of consecutive violations before [`UserQuotaTracker::should_escalate`]
    /// returns `true`.
    pub escalation_threshold: u32,
    /// Minimum nanoseconds between two escalation events for the same user.
    pub escalation_cooldown_ns: u64,
}

impl Default for QuotaPolicy {
    fn default() -> Self {
        Self {
            unknown: TierLimits {
                tokens_per_hour: 10_000,
                tokens_per_day: 50_000,
                requests_per_hour: 10,
            },
            verified: TierLimits {
                tokens_per_hour: 50_000,
                tokens_per_day: 200_000,
                requests_per_hour: 50,
            },
            trusted: TierLimits {
                tokens_per_hour: 200_000,
                tokens_per_day: 1_000_000,
                requests_per_hour: 200,
            },
            operator: TierLimits::UNLIMITED,
            escalation_threshold: DEFAULT_ESCALATION_THRESHOLD,
            escalation_cooldown_ns: DEFAULT_ESCALATION_COOLDOWN_NS,
        }
    }
}

impl QuotaPolicy {
    /// Returns the [`TierLimits`] for the given [`TrustTier`].
    pub fn for_tier(&self, tier: TrustTier) -> &TierLimits {
        match tier {
            TrustTier::Unknown => &self.unknown,
            TrustTier::Verified => &self.verified,
            TrustTier::Trusted => &self.trusted,
            TrustTier::Operator => &self.operator,
        }
    }
}

// ── ExceededReason ────────────────────────────────────────────────────────────

/// The specific limit that caused a [`QuotaResult::Exceeded`] response.
#[derive(Debug, Clone, PartialEq)]
pub enum ExceededReason {
    /// The rolling hourly token consumption ceiling was hit.
    HourlyTokenLimit {
        /// Tokens already consumed in the current hour.
        used: u64,
        /// Hourly limit for this user's tier.
        limit: u64,
    },
    /// The rolling daily token consumption ceiling was hit.
    DailyTokenLimit {
        /// Tokens already consumed in the current day.
        used: u64,
        /// Daily limit for this user's tier.
        limit: u64,
    },
    /// The rolling hourly request count ceiling was hit.
    HourlyRequestLimit {
        /// Requests made in the current hour.
        used: u64,
        /// Hourly request limit for this user's tier.
        limit: u64,
    },
}

impl ExceededReason {
    /// Returns a human-readable description suitable for audit entries.
    pub fn description(&self) -> String {
        match self {
            ExceededReason::HourlyTokenLimit { used, limit } => {
                format!("hourly token limit: used {used} / {limit}")
            }
            ExceededReason::DailyTokenLimit { used, limit } => {
                format!("daily token limit: used {used} / {limit}")
            }
            ExceededReason::HourlyRequestLimit { used, limit } => {
                format!("hourly request limit: used {used} / {limit}")
            }
        }
    }
}

// ── QuotaResult ───────────────────────────────────────────────────────────────

/// The result of a [`UserQuotaTracker::check_and_consume`] call.
#[derive(Debug, Clone, PartialEq)]
pub enum QuotaResult {
    /// The request was within quota and tokens have been consumed.
    Allowed {
        /// Remaining tokens in the hourly window after this request.
        remaining_hourly_tokens: u64,
        /// Remaining tokens in the daily window after this request.
        remaining_daily_tokens: u64,
        /// Remaining requests in the hourly window after this request.
        remaining_hourly_requests: u64,
    },
    /// The request exceeded quota and was not consumed.
    Exceeded {
        /// The user whose quota was exceeded.
        user_id: String,
        /// Which specific limit was hit.
        reason: ExceededReason,
        /// Earliest nanosecond timestamp at which the user may retry.
        retry_after_ns: u64,
    },
}

impl QuotaResult {
    /// Returns `true` when the request was allowed.
    pub fn is_allowed(&self) -> bool {
        matches!(self, QuotaResult::Allowed { .. })
    }

    /// Returns `true` when the request was denied due to exceeded quota.
    pub fn is_exceeded(&self) -> bool {
        matches!(self, QuotaResult::Exceeded { .. })
    }
}

// ── QuotaSnapshot ─────────────────────────────────────────────────────────────

/// A read-only view of one user's current quota usage, suitable for CLI display.
#[derive(Debug, Clone)]
pub struct QuotaSnapshot {
    /// Stable user identifier.
    pub user_id: String,
    /// Trust tier at snapshot time.
    pub trust_tier: TrustTier,
    /// Tokens consumed in the last rolling hour.
    pub hourly_tokens_used: u64,
    /// Hourly token ceiling for this tier.
    pub hourly_tokens_limit: u64,
    /// Tokens consumed in the last rolling 24 hours.
    pub daily_tokens_used: u64,
    /// Daily token ceiling for this tier.
    pub daily_tokens_limit: u64,
    /// Requests made in the last rolling hour.
    pub hourly_requests_used: u64,
    /// Hourly request ceiling for this tier.
    pub hourly_requests_limit: u64,
    /// Number of consecutive quota violations pending escalation check.
    pub consecutive_violations: u32,
}

impl QuotaSnapshot {
    /// Tokens still available in the hourly window.
    pub fn hourly_tokens_remaining(&self) -> u64 {
        self.hourly_tokens_limit
            .saturating_sub(self.hourly_tokens_used)
    }

    /// Tokens still available in the daily window.
    pub fn daily_tokens_remaining(&self) -> u64 {
        self.daily_tokens_limit
            .saturating_sub(self.daily_tokens_used)
    }

    /// Requests still available in the hourly window.
    pub fn hourly_requests_remaining(&self) -> u64 {
        self.hourly_requests_limit
            .saturating_sub(self.hourly_requests_used)
    }
}

// ── UserUsage (internal) ──────────────────────────────────────────────────────

struct UserUsage {
    /// `(timestamp_ns, token_count)` events within the hourly window.
    hourly_tokens: VecDeque<(u64, u64)>,
    /// `(timestamp_ns, token_count)` events within the daily window.
    daily_tokens: VecDeque<(u64, u64)>,
    /// `timestamp_ns` of each request in the hourly window.
    hourly_requests: VecDeque<u64>,
    /// Count of consecutive denied requests (resets on the first allowed request).
    consecutive_violations: u32,
    /// Nanosecond timestamp of the last escalation for this user.
    last_escalation_ns: u64,
}

impl UserUsage {
    fn new() -> Self {
        Self {
            hourly_tokens: VecDeque::new(),
            daily_tokens: VecDeque::new(),
            hourly_requests: VecDeque::new(),
            consecutive_violations: 0,
            last_escalation_ns: 0,
        }
    }

    /// Drops events that are no longer within their respective windows.
    fn drain_stale(&mut self, now_ns: u64) {
        let hour_cutoff = now_ns.saturating_sub(HOUR_NS);
        let day_cutoff = now_ns.saturating_sub(DAY_NS);

        while matches!(self.hourly_tokens.front(), Some(&(ts, _)) if ts < hour_cutoff) {
            self.hourly_tokens.pop_front();
        }
        while matches!(self.daily_tokens.front(), Some(&(ts, _)) if ts < day_cutoff) {
            self.daily_tokens.pop_front();
        }
        while matches!(self.hourly_requests.front(), Some(&ts) if ts < hour_cutoff) {
            self.hourly_requests.pop_front();
        }
    }

    /// Tokens consumed in the current hourly window (after draining stale entries).
    fn hourly_tokens_used(&self) -> u64 {
        self.hourly_tokens.iter().map(|(_, t)| t).sum()
    }

    /// Tokens consumed in the current daily window (after draining stale entries).
    fn daily_tokens_used(&self) -> u64 {
        self.daily_tokens.iter().map(|(_, t)| t).sum()
    }

    /// Requests made in the current hourly window (after draining stale entries).
    fn hourly_requests_used(&self) -> u64 {
        self.hourly_requests.len() as u64
    }

    /// Compute hourly-window token usage without mutating (for snapshots).
    fn hourly_tokens_used_at(&self, now_ns: u64) -> u64 {
        let cutoff = now_ns.saturating_sub(HOUR_NS);
        self.hourly_tokens
            .iter()
            .filter(|(ts, _)| *ts >= cutoff)
            .map(|(_, t)| t)
            .sum()
    }

    /// Compute daily-window token usage without mutating (for snapshots).
    fn daily_tokens_used_at(&self, now_ns: u64) -> u64 {
        let cutoff = now_ns.saturating_sub(DAY_NS);
        self.daily_tokens
            .iter()
            .filter(|(ts, _)| *ts >= cutoff)
            .map(|(_, t)| t)
            .sum()
    }

    /// Compute hourly request count without mutating (for snapshots).
    fn hourly_requests_used_at(&self, now_ns: u64) -> u64 {
        let cutoff = now_ns.saturating_sub(HOUR_NS);
        self.hourly_requests
            .iter()
            .filter(|&&ts| ts >= cutoff)
            .count() as u64
    }
}

// ── UserQuotaTracker ──────────────────────────────────────────────────────────

/// Rolling-window quota tracker for all users seen by the agent.
///
/// Create with [`UserQuotaTracker::new`] supplying a [`QuotaPolicy`], or use
/// [`UserQuotaTracker::with_default_policy`] for the out-of-box defaults.
///
/// All operations take `now_ns` (nanosecond Unix timestamp) to allow
/// hermetic testing without touching the system clock.
pub struct UserQuotaTracker {
    policy: QuotaPolicy,
    usage: HashMap<String, UserUsage>,
}

impl UserQuotaTracker {
    /// Creates a tracker with the given [`QuotaPolicy`].
    pub fn new(policy: QuotaPolicy) -> Self {
        Self {
            policy,
            usage: HashMap::new(),
        }
    }

    /// Creates a tracker with [`QuotaPolicy::default`].
    pub fn with_default_policy() -> Self {
        Self::new(QuotaPolicy::default())
    }

    /// Returns a reference to the active [`QuotaPolicy`].
    pub fn policy(&self) -> &QuotaPolicy {
        &self.policy
    }

    /// Checks whether `user_id` (at `tier`) may consume `tokens` right now.
    ///
    /// On [`QuotaResult::Allowed`] the tokens are recorded in both the hourly
    /// and daily windows and the consecutive-violation counter is reset.
    ///
    /// On [`QuotaResult::Exceeded`] the tokens are **not** consumed and the
    /// consecutive-violation counter is incremented.
    pub fn check_and_consume(
        &mut self,
        user_id: &str,
        tier: TrustTier,
        tokens: u64,
        now_ns: u64,
    ) -> QuotaResult {
        let limits = self.policy.for_tier(tier);

        // Operator tier bypasses all checks.
        if limits.is_unlimited() {
            return QuotaResult::Allowed {
                remaining_hourly_tokens: u64::MAX,
                remaining_daily_tokens: u64::MAX,
                remaining_hourly_requests: u64::MAX,
            };
        }

        let usage = self
            .usage
            .entry(user_id.to_owned())
            .or_insert_with(UserUsage::new);

        usage.drain_stale(now_ns);

        let hourly_tokens = usage.hourly_tokens_used();
        let daily_tokens = usage.daily_tokens_used();
        let hourly_reqs = usage.hourly_requests_used();

        // Check hourly request limit first (cheapest check — no addition needed).
        if hourly_reqs >= limits.requests_per_hour {
            usage.consecutive_violations += 1;
            return QuotaResult::Exceeded {
                user_id: user_id.to_owned(),
                reason: ExceededReason::HourlyRequestLimit {
                    used: hourly_reqs,
                    limit: limits.requests_per_hour,
                },
                retry_after_ns: now_ns + HOUR_NS,
            };
        }

        // Check hourly token limit.
        if hourly_tokens.saturating_add(tokens) > limits.tokens_per_hour {
            usage.consecutive_violations += 1;
            return QuotaResult::Exceeded {
                user_id: user_id.to_owned(),
                reason: ExceededReason::HourlyTokenLimit {
                    used: hourly_tokens,
                    limit: limits.tokens_per_hour,
                },
                retry_after_ns: now_ns + HOUR_NS,
            };
        }

        // Check daily token limit.
        if daily_tokens.saturating_add(tokens) > limits.tokens_per_day {
            usage.consecutive_violations += 1;
            return QuotaResult::Exceeded {
                user_id: user_id.to_owned(),
                reason: ExceededReason::DailyTokenLimit {
                    used: daily_tokens,
                    limit: limits.tokens_per_day,
                },
                retry_after_ns: now_ns + DAY_NS,
            };
        }

        // Allowed — consume.
        usage.hourly_tokens.push_back((now_ns, tokens));
        usage.daily_tokens.push_back((now_ns, tokens));
        usage.hourly_requests.push_back(now_ns);
        usage.consecutive_violations = 0;

        QuotaResult::Allowed {
            remaining_hourly_tokens: limits
                .tokens_per_hour
                .saturating_sub(hourly_tokens + tokens),
            remaining_daily_tokens: limits.tokens_per_day.saturating_sub(daily_tokens + tokens),
            remaining_hourly_requests: limits.requests_per_hour.saturating_sub(hourly_reqs + 1),
        }
    }

    /// Returns the consecutive-violation count for `user_id`.
    ///
    /// Returns `0` when the user has no tracked usage or no violations.
    pub fn consecutive_violations(&self, user_id: &str) -> u32 {
        self.usage
            .get(user_id)
            .map(|u| u.consecutive_violations)
            .unwrap_or(0)
    }

    /// Returns `true` when `user_id`'s violations have reached the escalation
    /// threshold **and** the escalation cooldown has elapsed since the last one.
    pub fn should_escalate(&self, user_id: &str, now_ns: u64) -> bool {
        let usage = match self.usage.get(user_id) {
            Some(u) => u,
            None => return false,
        };
        usage.consecutive_violations >= self.policy.escalation_threshold
            && now_ns.saturating_sub(usage.last_escalation_ns) >= self.policy.escalation_cooldown_ns
    }

    /// Records that an escalation was emitted for `user_id` at `now_ns`.
    ///
    /// Must be called after emitting an `AuditEntry::QuotaEscalated` so the
    /// cooldown timer resets.
    pub fn record_escalation(&mut self, user_id: &str, now_ns: u64) {
        if let Some(usage) = self.usage.get_mut(user_id) {
            usage.last_escalation_ns = now_ns;
        }
    }

    /// Clears all usage windows for `user_id`.
    ///
    /// Useful for operator-initiated resets via `anima quota reset <id>`.
    pub fn reset(&mut self, user_id: &str) {
        self.usage.remove(user_id);
    }

    /// Returns a point-in-time [`QuotaSnapshot`] for display.
    ///
    /// The snapshot reads without consuming — stale entries are excluded via
    /// the non-mutating `_at` accessors so the displayed values match the
    /// limits that would be applied on the next `check_and_consume`.
    pub fn snapshot(&self, user_id: &str, tier: TrustTier, now_ns: u64) -> QuotaSnapshot {
        let limits = self.policy.for_tier(tier);
        let (hourly_used, daily_used, hourly_reqs, violations) = self
            .usage
            .get(user_id)
            .map(|u| {
                (
                    u.hourly_tokens_used_at(now_ns),
                    u.daily_tokens_used_at(now_ns),
                    u.hourly_requests_used_at(now_ns),
                    u.consecutive_violations,
                )
            })
            .unwrap_or((0, 0, 0, 0));

        QuotaSnapshot {
            user_id: user_id.to_owned(),
            trust_tier: tier,
            hourly_tokens_used: hourly_used,
            hourly_tokens_limit: limits.tokens_per_hour,
            daily_tokens_used: daily_used,
            daily_tokens_limit: limits.tokens_per_day,
            hourly_requests_used: hourly_reqs,
            hourly_requests_limit: limits.requests_per_hour,
            consecutive_violations: violations,
        }
    }

    /// Returns all `user_id`s that have been tracked (sorted for determinism).
    pub fn all_tracked_users(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.usage.keys().cloned().collect();
        ids.sort();
        ids
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> u64 {
        1_700_000_000_000_000_000u64
    }

    fn tracker() -> UserQuotaTracker {
        UserQuotaTracker::with_default_policy()
    }

    // ── TierLimits ────────────────────────────────────────────────────────────

    #[test]
    fn tier_limits_unlimited_sentinel_has_max_values() {
        let u = TierLimits::UNLIMITED;
        assert_eq!(u.tokens_per_hour, u64::MAX);
        assert_eq!(u.tokens_per_day, u64::MAX);
        assert_eq!(u.requests_per_hour, u64::MAX);
        assert!(u.is_unlimited());
    }

    #[test]
    fn tier_limits_non_unlimited_is_not_unlimited() {
        let l = TierLimits {
            tokens_per_hour: 10_000,
            tokens_per_day: 50_000,
            requests_per_hour: 10,
        };
        assert!(!l.is_unlimited());
    }

    // ── QuotaPolicy ───────────────────────────────────────────────────────────

    #[test]
    fn policy_for_unknown_tier_has_tight_limits() {
        let p = QuotaPolicy::default();
        let l = p.for_tier(TrustTier::Unknown);
        assert_eq!(l.tokens_per_hour, 10_000);
        assert_eq!(l.tokens_per_day, 50_000);
        assert_eq!(l.requests_per_hour, 10);
    }

    #[test]
    fn policy_for_operator_tier_is_unlimited() {
        let p = QuotaPolicy::default();
        assert!(p.for_tier(TrustTier::Operator).is_unlimited());
    }

    #[test]
    fn policy_tiers_are_monotonically_increasing() {
        let p = QuotaPolicy::default();
        assert!(p.unknown.tokens_per_hour < p.verified.tokens_per_hour);
        assert!(p.verified.tokens_per_hour < p.trusted.tokens_per_hour);
        assert!(p.trusted.tokens_per_hour < p.operator.tokens_per_hour);
    }

    // ── check_and_consume — happy paths ──────────────────────────────────────

    #[test]
    fn operator_tier_is_always_allowed() {
        let mut t = tracker();
        for _ in 0..20 {
            let r = t.check_and_consume("op:1", TrustTier::Operator, 100_000, now());
            assert!(r.is_allowed(), "operator should never be rate-limited");
        }
    }

    #[test]
    fn within_limits_is_allowed_and_returns_remaining() {
        let mut t = tracker();
        let r = t.check_and_consume("u:1", TrustTier::Unknown, 1_000, now());
        match r {
            QuotaResult::Allowed {
                remaining_hourly_tokens,
                remaining_daily_tokens,
                remaining_hourly_requests,
            } => {
                assert_eq!(remaining_hourly_tokens, 9_000); // 10_000 - 1_000
                assert_eq!(remaining_daily_tokens, 49_000); // 50_000 - 1_000
                assert_eq!(remaining_hourly_requests, 9); // 10 - 1
            }
            QuotaResult::Exceeded { .. } => panic!("expected Allowed"),
        }
    }

    #[test]
    fn allowed_result_resets_consecutive_violations() {
        let mut t = tracker();
        let now = now();
        // Fill hourly budget fully (10 calls × 1 000 = 10 000 tokens, 10 reqs).
        for _ in 0..10 {
            t.check_and_consume("u:1", TrustTier::Unknown, 1_000, now);
        }
        // 11th call exceeds both the request limit and the hourly token limit.
        t.check_and_consume("u:1", TrustTier::Unknown, 1_000, now);
        let violations_before = t.consecutive_violations("u:1");
        assert!(violations_before > 0);
        // Reset by consuming within limits in a fresh window (advance time by 2 hours).
        let later = now + 2 * HOUR_NS;
        let r = t.check_and_consume("u:1", TrustTier::Unknown, 1, later);
        assert!(r.is_allowed());
        assert_eq!(t.consecutive_violations("u:1"), 0);
    }

    // ── check_and_consume — denial paths ─────────────────────────────────────

    #[test]
    fn exceeding_hourly_token_limit_is_denied() {
        let policy = QuotaPolicy {
            unknown: TierLimits {
                tokens_per_hour: 100,
                tokens_per_day: 10_000,
                requests_per_hour: 1_000,
            },
            ..QuotaPolicy::default()
        };
        let mut t = UserQuotaTracker::new(policy);
        let now = now();
        // Consume the full hourly budget.
        t.check_and_consume("u:1", TrustTier::Unknown, 100, now);
        // Next request should be denied.
        let r = t.check_and_consume("u:1", TrustTier::Unknown, 1, now);
        match r {
            QuotaResult::Exceeded { reason, .. } => {
                assert!(matches!(reason, ExceededReason::HourlyTokenLimit { .. }));
            }
            _ => panic!("expected Exceeded"),
        }
    }

    #[test]
    fn exceeding_daily_token_limit_is_denied() {
        let policy = QuotaPolicy {
            unknown: TierLimits {
                tokens_per_hour: 10_000,
                tokens_per_day: 50,
                requests_per_hour: 1_000,
            },
            ..QuotaPolicy::default()
        };
        let mut t = UserQuotaTracker::new(policy);
        let now = now();
        t.check_and_consume("u:1", TrustTier::Unknown, 50, now);
        let r = t.check_and_consume("u:1", TrustTier::Unknown, 1, now);
        match r {
            QuotaResult::Exceeded { reason, .. } => {
                assert!(matches!(reason, ExceededReason::DailyTokenLimit { .. }));
            }
            _ => panic!("expected Exceeded"),
        }
    }

    #[test]
    fn exceeding_hourly_request_limit_is_denied() {
        let policy = QuotaPolicy {
            unknown: TierLimits {
                tokens_per_hour: 1_000_000,
                tokens_per_day: 1_000_000,
                requests_per_hour: 2,
            },
            ..QuotaPolicy::default()
        };
        let mut t = UserQuotaTracker::new(policy);
        let now = now();
        t.check_and_consume("u:1", TrustTier::Unknown, 1, now);
        t.check_and_consume("u:1", TrustTier::Unknown, 1, now);
        let r = t.check_and_consume("u:1", TrustTier::Unknown, 1, now);
        match r {
            QuotaResult::Exceeded { reason, .. } => {
                assert!(matches!(reason, ExceededReason::HourlyRequestLimit { .. }));
            }
            _ => panic!("expected Exceeded"),
        }
    }

    #[test]
    fn exceeded_result_carries_user_id_and_retry_hint() {
        let policy = QuotaPolicy {
            unknown: TierLimits {
                tokens_per_hour: 5,
                tokens_per_day: 100,
                requests_per_hour: 100,
            },
            ..QuotaPolicy::default()
        };
        let mut t = UserQuotaTracker::new(policy);
        let now = now();
        t.check_and_consume("telegram:42", TrustTier::Unknown, 5, now);
        let r = t.check_and_consume("telegram:42", TrustTier::Unknown, 1, now);
        match r {
            QuotaResult::Exceeded {
                user_id,
                retry_after_ns,
                ..
            } => {
                assert_eq!(user_id, "telegram:42");
                assert!(retry_after_ns > now);
            }
            _ => panic!("expected Exceeded"),
        }
    }

    // ── rolling window ────────────────────────────────────────────────────────

    #[test]
    fn stale_events_are_drained_before_check() {
        let policy = QuotaPolicy {
            unknown: TierLimits {
                tokens_per_hour: 100,
                tokens_per_day: 10_000,
                requests_per_hour: 1_000,
            },
            ..QuotaPolicy::default()
        };
        let mut t = UserQuotaTracker::new(policy);
        let t0 = now();
        // Fill hourly budget.
        t.check_and_consume("u:1", TrustTier::Unknown, 100, t0);
        // Advance time by 2 hours — old events are now stale.
        let t1 = t0 + 2 * HOUR_NS;
        let r = t.check_and_consume("u:1", TrustTier::Unknown, 100, t1);
        assert!(
            r.is_allowed(),
            "stale hourly entries should have been drained"
        );
    }

    #[test]
    fn daily_window_is_independent_of_hourly_window() {
        let policy = QuotaPolicy {
            unknown: TierLimits {
                tokens_per_hour: 1_000,
                tokens_per_day: 100,
                requests_per_hour: 1_000,
            },
            ..QuotaPolicy::default()
        };
        let mut t = UserQuotaTracker::new(policy);
        let t0 = now();
        // Exhaust daily budget across multiple hours.
        t.check_and_consume("u:1", TrustTier::Unknown, 100, t0);
        // After 2 hours the hourly window is clear but daily window is not.
        let t1 = t0 + 2 * HOUR_NS;
        let r = t.check_and_consume("u:1", TrustTier::Unknown, 1, t1);
        match r {
            QuotaResult::Exceeded { reason, .. } => {
                assert!(matches!(reason, ExceededReason::DailyTokenLimit { .. }));
            }
            _ => panic!("expected DailyTokenLimit"),
        }
    }

    // ── escalation ────────────────────────────────────────────────────────────

    #[test]
    fn consecutive_violations_incremented_on_deny() {
        let policy = QuotaPolicy {
            unknown: TierLimits {
                tokens_per_hour: 0, // always exceeds
                tokens_per_day: 0,
                requests_per_hour: 0,
            },
            ..QuotaPolicy::default()
        };
        let mut t = UserQuotaTracker::new(policy);
        let now = now();
        t.check_and_consume("u:1", TrustTier::Unknown, 1, now);
        t.check_and_consume("u:1", TrustTier::Unknown, 1, now);
        assert_eq!(t.consecutive_violations("u:1"), 2);
    }

    #[test]
    fn escalation_fires_after_threshold_violations() {
        let policy = QuotaPolicy {
            unknown: TierLimits {
                tokens_per_hour: 0,
                tokens_per_day: 0,
                requests_per_hour: 0,
            },
            escalation_threshold: 3,
            escalation_cooldown_ns: 0,
            ..QuotaPolicy::default()
        };
        let mut t = UserQuotaTracker::new(policy);
        let now = now();
        // 1, 2 — not yet
        t.check_and_consume("u:1", TrustTier::Unknown, 1, now);
        t.check_and_consume("u:1", TrustTier::Unknown, 1, now);
        assert!(!t.should_escalate("u:1", now));
        // 3 — threshold reached
        t.check_and_consume("u:1", TrustTier::Unknown, 1, now);
        assert!(t.should_escalate("u:1", now));
    }

    #[test]
    fn escalation_is_rate_limited_by_cooldown() {
        let policy = QuotaPolicy {
            unknown: TierLimits {
                tokens_per_hour: 0,
                tokens_per_day: 0,
                requests_per_hour: 0,
            },
            escalation_threshold: 1,
            escalation_cooldown_ns: HOUR_NS,
            ..QuotaPolicy::default()
        };
        let mut t = UserQuotaTracker::new(policy);
        let now = now();
        t.check_and_consume("u:1", TrustTier::Unknown, 1, now);
        assert!(t.should_escalate("u:1", now));
        // Record the escalation.
        t.record_escalation("u:1", now);
        // Immediately after — still in cooldown.
        assert!(!t.should_escalate("u:1", now));
        // After cooldown expires.
        let later = now + HOUR_NS;
        // Trigger another violation so the threshold is active.
        t.check_and_consume("u:1", TrustTier::Unknown, 1, later);
        assert!(t.should_escalate("u:1", later));
    }

    // ── reset ─────────────────────────────────────────────────────────────────

    #[test]
    fn reset_clears_all_usage_for_user() {
        let policy = QuotaPolicy {
            unknown: TierLimits {
                tokens_per_hour: 100,
                tokens_per_day: 1_000,
                requests_per_hour: 5,
            },
            ..QuotaPolicy::default()
        };
        let mut t = UserQuotaTracker::new(policy);
        let now = now();
        // Fill up budget.
        for _ in 0..5 {
            t.check_and_consume("u:1", TrustTier::Unknown, 20, now);
        }
        assert!(t
            .check_and_consume("u:1", TrustTier::Unknown, 1, now)
            .is_exceeded());
        // Reset.
        t.reset("u:1");
        let r = t.check_and_consume("u:1", TrustTier::Unknown, 20, now);
        assert!(r.is_allowed(), "usage should be clear after reset");
    }

    // ── snapshot ──────────────────────────────────────────────────────────────

    #[test]
    fn snapshot_reflects_current_usage() {
        let policy = QuotaPolicy {
            unknown: TierLimits {
                tokens_per_hour: 1_000,
                tokens_per_day: 5_000,
                requests_per_hour: 10,
            },
            ..QuotaPolicy::default()
        };
        let mut t = UserQuotaTracker::new(policy);
        let now = now();
        t.check_and_consume("u:1", TrustTier::Unknown, 300, now);
        t.check_and_consume("u:1", TrustTier::Unknown, 200, now);
        let snap = t.snapshot("u:1", TrustTier::Unknown, now);
        assert_eq!(snap.hourly_tokens_used, 500);
        assert_eq!(snap.daily_tokens_used, 500);
        assert_eq!(snap.hourly_requests_used, 2);
        assert_eq!(snap.hourly_tokens_remaining(), 500);
        assert_eq!(snap.daily_tokens_remaining(), 4_500);
    }

    #[test]
    fn snapshot_excludes_stale_entries() {
        let policy = QuotaPolicy {
            unknown: TierLimits {
                tokens_per_hour: 1_000,
                tokens_per_day: 5_000,
                requests_per_hour: 10,
            },
            ..QuotaPolicy::default()
        };
        let mut t = UserQuotaTracker::new(policy);
        let t0 = now();
        t.check_and_consume("u:1", TrustTier::Unknown, 500, t0);
        // Two hours later the hourly entry is stale.
        let t1 = t0 + 2 * HOUR_NS;
        let snap = t.snapshot("u:1", TrustTier::Unknown, t1);
        assert_eq!(snap.hourly_tokens_used, 0);
        // But the daily entry is still within the 24h window.
        assert_eq!(snap.daily_tokens_used, 500);
    }

    #[test]
    fn snapshot_for_unknown_user_shows_zero_usage() {
        let t = tracker();
        let snap = t.snapshot("nobody:0", TrustTier::Unknown, now());
        assert_eq!(snap.hourly_tokens_used, 0);
        assert_eq!(snap.daily_tokens_used, 0);
        assert_eq!(snap.hourly_requests_used, 0);
        assert_eq!(snap.consecutive_violations, 0);
    }

    // ── all_tracked_users ─────────────────────────────────────────────────────

    #[test]
    fn all_tracked_users_returns_sorted_ids() {
        let mut t = tracker();
        let now = now();
        t.check_and_consume("c:3", TrustTier::Unknown, 1, now);
        t.check_and_consume("a:1", TrustTier::Unknown, 1, now);
        t.check_and_consume("b:2", TrustTier::Unknown, 1, now);
        let ids = t.all_tracked_users();
        assert_eq!(ids, vec!["a:1", "b:2", "c:3"]);
    }

    // ── ExceededReason::description ───────────────────────────────────────────

    #[test]
    fn exceeded_reason_description_is_human_readable() {
        let r = ExceededReason::HourlyTokenLimit {
            used: 10_000,
            limit: 10_000,
        };
        assert!(r.description().contains("hourly token limit"));
        assert!(r.description().contains("10000"));
    }
}
