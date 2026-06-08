//! `DiagnosticCheck` trait and result types.
//!
//! Every built-in and custom diagnostic check implements [`DiagnosticCheck`].
//! The trait is object-safe so checks can be composed in a `Vec<Box<dyn DiagnosticCheck>>`.

use serde::Serialize;

/// Coarse-grained health status for a single diagnostic check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    /// The subsystem is operating within expected parameters.
    Healthy,
    /// The subsystem is functional but shows early warning signs.
    Degraded,
    /// The subsystem is in a state that requires immediate attention.
    Critical,
    /// The check could not be executed (missing data, unavailable subsystem).
    Unknown,
}

impl HealthStatus {
    /// Returns `true` when the status warrants operator intervention.
    ///
    /// `Unknown` returns `false` — it means the check could not be evaluated
    /// (e.g. no data yet), not that a problem was detected.
    pub fn needs_attention(&self) -> bool {
        matches!(self, Self::Degraded | Self::Critical)
    }

    /// Ordinal severity used to compute the aggregate `worst` status.
    ///
    /// `Unknown` ranks below `Degraded` so that a single missing data-point
    /// does not mask otherwise-healthy aggregate status.
    ///
    /// | Status    | Severity |
    /// |-----------|----------|
    /// | Healthy   | 0        |
    /// | Unknown   | 1        |
    /// | Degraded  | 2        |
    /// | Critical  | 3        |
    pub fn severity(&self) -> u8 {
        match self {
            Self::Healthy => 0,
            Self::Unknown => 1,
            Self::Degraded => 2,
            Self::Critical => 3,
        }
    }

    /// Returns the worst (highest severity) of two statuses.
    pub fn worst(a: HealthStatus, b: HealthStatus) -> HealthStatus {
        if a.severity() >= b.severity() {
            a
        } else {
            b
        }
    }
}

/// The outcome of a single diagnostic check.
#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    /// Short identifier for the check (e.g. `"memory_pressure"`).
    pub check_id: &'static str,
    /// Human-readable name for display purposes.
    pub display_name: &'static str,
    /// Coarse health status.
    pub status: HealthStatus,
    /// One-line summary of the observation.
    pub summary: String,
    /// Concrete remediation suggestion shown only when status is not Healthy.
    pub remediation: Option<String>,
    /// Optional structured detail (JSON value) for programmatic consumers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

impl CheckResult {
    /// Construct a healthy result with no remediation or detail.
    pub fn healthy(
        check_id: &'static str,
        display_name: &'static str,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            check_id,
            display_name,
            status: HealthStatus::Healthy,
            summary: summary.into(),
            remediation: None,
            detail: None,
        }
    }

    /// Construct a degraded result.
    pub fn degraded(
        check_id: &'static str,
        display_name: &'static str,
        summary: impl Into<String>,
        remediation: impl Into<String>,
    ) -> Self {
        Self {
            check_id,
            display_name,
            status: HealthStatus::Degraded,
            summary: summary.into(),
            remediation: Some(remediation.into()),
            detail: None,
        }
    }

    /// Construct a critical result.
    pub fn critical(
        check_id: &'static str,
        display_name: &'static str,
        summary: impl Into<String>,
        remediation: impl Into<String>,
    ) -> Self {
        Self {
            check_id,
            display_name,
            status: HealthStatus::Critical,
            summary: summary.into(),
            remediation: Some(remediation.into()),
            detail: None,
        }
    }

    /// Construct an unknown result (check could not be evaluated).
    pub fn unknown(
        check_id: &'static str,
        display_name: &'static str,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            check_id,
            display_name,
            status: HealthStatus::Unknown,
            summary: reason.into(),
            remediation: None,
            detail: None,
        }
    }

    /// Attach a structured detail payload.
    pub fn with_detail(mut self, detail: serde_json::Value) -> Self {
        self.detail = Some(detail);
        self
    }
}

/// A single diagnostic check that can be executed over an [`AuditSnapshot`].
///
/// Implement this trait for each subsystem or concern you want to monitor.
/// Checks are stateless — all inputs are provided via [`AuditSnapshot`] so
/// results are reproducible from the same data.
pub trait DiagnosticCheck: Send + Sync {
    /// Short stable identifier (used in audit log and CLI output).
    fn check_id(&self) -> &'static str;
    /// Human-readable name.
    fn display_name(&self) -> &'static str;
    /// Run the check against the provided snapshot and return a result.
    fn run(&self, snapshot: &crate::AuditSnapshot) -> CheckResult;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worst_status_selects_higher_severity() {
        assert_eq!(
            HealthStatus::worst(HealthStatus::Healthy, HealthStatus::Degraded),
            HealthStatus::Degraded
        );
        assert_eq!(
            HealthStatus::worst(HealthStatus::Critical, HealthStatus::Degraded),
            HealthStatus::Critical
        );
        assert_eq!(
            HealthStatus::worst(HealthStatus::Healthy, HealthStatus::Healthy),
            HealthStatus::Healthy
        );
    }

    #[test]
    fn needs_attention_only_for_degraded_and_critical() {
        assert!(!HealthStatus::Healthy.needs_attention());
        assert!(HealthStatus::Degraded.needs_attention());
        assert!(HealthStatus::Critical.needs_attention());
        // Unknown means the check could not run — it does not demand remediation.
        assert!(!HealthStatus::Unknown.needs_attention());
    }

    #[test]
    fn check_result_healthy_has_no_remediation() {
        let r = CheckResult::healthy("test", "Test Check", "All good");
        assert!(r.remediation.is_none());
        assert_eq!(r.status, HealthStatus::Healthy);
    }

    #[test]
    fn check_result_degraded_has_remediation() {
        let r = CheckResult::degraded("test", "Test Check", "High load", "Scale down tasks");
        assert!(r.remediation.is_some());
        assert_eq!(r.status, HealthStatus::Degraded);
    }

    #[test]
    fn check_result_with_detail_stores_json() {
        let r = CheckResult::healthy("test", "Test", "ok")
            .with_detail(serde_json::json!({"value": 42}));
        assert!(r.detail.is_some());
    }
}
