//! [`AnalyticsEngine`] — the single entry point into the analytics crate.
//!
//! All four sub-reports are pure functions over `&[AuditEntry]`.  The engine
//! groups them behind a single type so callers have one import rather than
//! four.

use vita::audit::AuditEntry;

use crate::{
    gate::{compute_gate_report, GateReport},
    health::{compute_health_report, HealthReport},
    latency::{compute_latency_report, LatencyReport},
    token::{compute_token_report, TokenReport},
    SummaryReport,
};

/// Analytics engine — a namespace for all report-computation functions.
///
/// Every method is a pure, deterministic fold over a `&[AuditEntry]` slice.
/// No I/O or side-effects; safe to call from tests and from the CLI.
pub struct AnalyticsEngine;

impl AnalyticsEngine {
    /// Compute token usage analytics.
    pub fn token_report(entries: &[AuditEntry]) -> TokenReport {
        compute_token_report(entries)
    }

    /// Compute cortex latency and reliability analytics.
    pub fn latency_report(entries: &[AuditEntry]) -> LatencyReport {
        compute_latency_report(entries)
    }

    /// Compute Striatal Gate and routing analytics.
    pub fn gate_report(entries: &[AuditEntry]) -> GateReport {
        compute_gate_report(entries)
    }

    /// Compute overall agent health score and recommendations.
    pub fn health_report(entries: &[AuditEntry]) -> HealthReport {
        let token = compute_token_report(entries);
        let latency = compute_latency_report(entries);
        let gate = compute_gate_report(entries);
        compute_health_report(entries, &token, &latency, &gate)
    }

    /// Compute a full [`SummaryReport`] combining all four sub-reports.
    ///
    /// Each sub-report is computed in a single pass over `entries`; the
    /// function makes four passes total.  The `agent_id` argument is stored
    /// verbatim in the summary (it is not extracted from the entries themselves
    /// to avoid the ambiguity when entries span multiple agents).
    pub fn summary_report(entries: &[AuditEntry], agent_id: &str) -> SummaryReport {
        let token = compute_token_report(entries);
        let latency = compute_latency_report(entries);
        let gate = compute_gate_report(entries);
        let health = compute_health_report(entries, &token, &latency, &gate);
        SummaryReport {
            agent_id: agent_id.to_string(),
            entries_analyzed: entries.len(),
            token,
            latency,
            gate,
            health,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use vita::audit::AuditEntry;

    fn started(id: u64) -> AuditEntry {
        AuditEntry::TaskStarted {
            agent_id: "a".into(),
            task_id: id,
            tier: 0,
            prompt: "p".into(),
        }
    }

    fn completed(id: u64, tokens: u32) -> AuditEntry {
        AuditEntry::TaskCompleted {
            agent_id: "a".into(),
            task_id: id,
            tokens_emitted: tokens,
            response: "r".into(),
        }
    }

    #[test]
    fn summary_report_entries_analyzed_matches_input_length() {
        let entries = vec![started(1), completed(1, 100)];
        let s = AnalyticsEngine::summary_report(&entries, "a");
        assert_eq!(s.entries_analyzed, 2);
    }

    #[test]
    fn summary_report_agent_id_is_stored_verbatim() {
        let s = AnalyticsEngine::summary_report(&[], "my-agent");
        assert_eq!(s.agent_id, "my-agent");
    }

    #[test]
    fn token_report_delegates_to_compute_function() {
        let entries = vec![started(1), completed(1, 42)];
        let r = AnalyticsEngine::token_report(&entries);
        assert_eq!(r.total_tokens, 42);
    }

    #[test]
    fn health_report_grade_a_on_clean_log() {
        let r = AnalyticsEngine::health_report(&[]);
        assert_eq!(r.grade, "A");
    }

    #[test]
    fn summary_report_token_gate_latency_health_consistent() {
        let entries = vec![started(1), completed(1, 200)];
        let s = AnalyticsEngine::summary_report(&entries, "a");
        // Individual reports should agree with summary sub-fields.
        let tok = AnalyticsEngine::token_report(&entries);
        assert_eq!(s.token.total_tokens, tok.total_tokens);
        let gate = AnalyticsEngine::gate_report(&entries);
        assert_eq!(s.gate.total_evaluations, gate.total_evaluations);
    }
}
