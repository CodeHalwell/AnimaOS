#![forbid(unsafe_code)]

//! Per-workspace resource quotas — E31 S31.3.
//!
//! [`WorkspaceQuota`] defines the upper bounds on resource consumption for a
//! workspace.  [`QuotaUsage`] tracks current consumption and [`QuotaUsage::check`]
//! returns the first violated limit (if any).
//!
//! The defaults are intentionally generous; operators tighten them via the
//! `anima workspace set-quota` command.

use serde::{Deserialize, Serialize};

// ── QuotaViolation ────────────────────────────────────────────────────────────

/// Which quota limit was exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaViolation {
    /// Too many members in the workspace.
    MemberLimit,
    /// Daily token budget exhausted.
    DailyTokenLimit,
    /// Storage capacity exceeded.
    StorageLimit,
    /// Maximum concurrent active tasks reached.
    ActiveTaskLimit,
}

impl QuotaViolation {
    /// Human-readable description of the violated limit.
    pub fn as_str(self) -> &'static str {
        match self {
            QuotaViolation::MemberLimit => "member_limit",
            QuotaViolation::DailyTokenLimit => "daily_token_limit",
            QuotaViolation::StorageLimit => "storage_limit",
            QuotaViolation::ActiveTaskLimit => "active_task_limit",
        }
    }
}

impl std::fmt::Display for QuotaViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ── WorkspaceQuota ────────────────────────────────────────────────────────────

/// Resource limits for a workspace.
///
/// All limits are inclusive upper bounds.  A value of `u64::MAX` / `usize::MAX`
/// effectively means "unlimited" for that dimension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceQuota {
    /// Maximum number of members (including the owner).
    pub max_members: usize,
    /// Maximum tokens consumed per calendar day (UTC).
    pub max_daily_tokens: u64,
    /// Maximum bytes of stored memory (L1 + L2 + L3 combined).
    pub max_storage_bytes: u64,
    /// Maximum number of simultaneously active tasks.
    pub max_active_tasks: usize,
}

impl Default for WorkspaceQuota {
    fn default() -> Self {
        Self {
            max_members: 25,
            max_daily_tokens: 5_000_000,
            max_storage_bytes: 1_073_741_824, // 1 GiB
            max_active_tasks: 200,
        }
    }
}

impl WorkspaceQuota {
    /// Creates a quota with explicit limits.
    pub fn new(
        max_members: usize,
        max_daily_tokens: u64,
        max_storage_bytes: u64,
        max_active_tasks: usize,
    ) -> Self {
        Self {
            max_members,
            max_daily_tokens,
            max_storage_bytes,
            max_active_tasks,
        }
    }
}

// ── QuotaUsage ────────────────────────────────────────────────────────────────

/// Current resource consumption snapshot for a workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaUsage {
    /// Current member count.
    pub current_members: usize,
    /// Tokens consumed today (UTC calendar day).
    pub daily_tokens_used: u64,
    /// Bytes currently in storage.
    pub storage_bytes_used: u64,
    /// Currently active task count.
    pub active_tasks: usize,
}

impl QuotaUsage {
    /// Returns the first quota limit that would be exceeded if the current
    /// usage were admitted, or `None` when all limits are satisfied.
    ///
    /// Limits are checked in the order: members → tokens → storage → tasks,
    /// so the first violation is the most operationally critical.
    pub fn check(&self, quota: &WorkspaceQuota) -> Option<QuotaViolation> {
        if self.current_members > quota.max_members {
            return Some(QuotaViolation::MemberLimit);
        }
        if self.daily_tokens_used > quota.max_daily_tokens {
            return Some(QuotaViolation::DailyTokenLimit);
        }
        if self.storage_bytes_used > quota.max_storage_bytes {
            return Some(QuotaViolation::StorageLimit);
        }
        if self.active_tasks > quota.max_active_tasks {
            return Some(QuotaViolation::ActiveTaskLimit);
        }
        None
    }

    /// Returns `true` when adding `additional_members` would stay within the
    /// member limit.
    pub fn can_add_members(&self, additional: usize, quota: &WorkspaceQuota) -> bool {
        self.current_members.saturating_add(additional) <= quota.max_members
    }

    /// Returns the fraction of the daily token budget consumed in `[0.0, 1.0]`.
    ///
    /// Returns `1.0` when the quota is zero to avoid division by zero.
    pub fn token_budget_fraction(&self, quota: &WorkspaceQuota) -> f64 {
        if quota.max_daily_tokens == 0 {
            return 1.0;
        }
        (self.daily_tokens_used as f64 / quota.max_daily_tokens as f64).min(1.0)
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn tight_quota() -> WorkspaceQuota {
        WorkspaceQuota::new(3, 1_000, 512, 5)
    }

    #[test]
    fn default_quota_is_generous() {
        let q = WorkspaceQuota::default();
        assert_eq!(q.max_members, 25);
        assert!(q.max_daily_tokens >= 1_000_000);
    }

    #[test]
    fn usage_within_limits_returns_none() {
        let q = tight_quota();
        let u = QuotaUsage {
            current_members: 2,
            daily_tokens_used: 500,
            storage_bytes_used: 256,
            active_tasks: 3,
        };
        assert!(u.check(&q).is_none());
    }

    #[test]
    fn member_limit_detected() {
        let q = tight_quota();
        let u = QuotaUsage {
            current_members: 4,
            ..Default::default()
        };
        assert_eq!(u.check(&q), Some(QuotaViolation::MemberLimit));
    }

    #[test]
    fn daily_token_limit_detected() {
        let q = tight_quota();
        let u = QuotaUsage {
            daily_tokens_used: 1_001,
            ..Default::default()
        };
        assert_eq!(u.check(&q), Some(QuotaViolation::DailyTokenLimit));
    }

    #[test]
    fn storage_limit_detected() {
        let q = tight_quota();
        let u = QuotaUsage {
            storage_bytes_used: 600,
            ..Default::default()
        };
        assert_eq!(u.check(&q), Some(QuotaViolation::StorageLimit));
    }

    #[test]
    fn active_task_limit_detected() {
        let q = tight_quota();
        let u = QuotaUsage {
            active_tasks: 6,
            ..Default::default()
        };
        assert_eq!(u.check(&q), Some(QuotaViolation::ActiveTaskLimit));
    }

    #[test]
    fn member_limit_checked_before_token_limit() {
        let q = tight_quota();
        let u = QuotaUsage {
            current_members: 10,
            daily_tokens_used: 10_000,
            ..Default::default()
        };
        // Member limit is checked first.
        assert_eq!(u.check(&q), Some(QuotaViolation::MemberLimit));
    }

    #[test]
    fn can_add_members_within_limit() {
        let q = tight_quota(); // max_members = 3
        let u = QuotaUsage {
            current_members: 2,
            ..Default::default()
        };
        assert!(u.can_add_members(1, &q));
        assert!(!u.can_add_members(2, &q));
    }

    #[test]
    fn token_budget_fraction_is_bounded() {
        let q = WorkspaceQuota::new(10, 1_000, 512, 5);
        let u = QuotaUsage {
            daily_tokens_used: 500,
            ..Default::default()
        };
        let frac = u.token_budget_fraction(&q);
        assert!((frac - 0.5).abs() < 1e-9);
    }

    #[test]
    fn token_budget_fraction_caps_at_one() {
        let q = WorkspaceQuota::new(10, 100, 512, 5);
        let u = QuotaUsage {
            daily_tokens_used: 200,
            ..Default::default()
        };
        assert!((u.token_budget_fraction(&q) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn quota_usage_round_trips_through_json() {
        let u = QuotaUsage {
            current_members: 5,
            daily_tokens_used: 3_000,
            storage_bytes_used: 100_000,
            active_tasks: 2,
        };
        let json = serde_json::to_string(&u).unwrap();
        let restored: QuotaUsage = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, u);
    }
}
