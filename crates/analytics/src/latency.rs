//! Cortex latency and reliability analysis — S25.2.
//!
//! Derives latency statistics from `CortexInvoked` entries and reliability
//! metrics from `CortexCompleted` / `CortexFault` entries.

use serde::{Deserialize, Serialize};
use vita::audit::AuditEntry;

// ── Percentiles ───────────────────────────────────────────────────────────────

/// Latency percentile summary in milliseconds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Percentiles {
    /// Number of samples.
    pub count: usize,
    /// Arithmetic mean (ms).
    pub mean_ms: f64,
    /// Minimum observed latency (ms).
    pub min_ms: u64,
    /// Maximum observed latency (ms).
    pub max_ms: u64,
    /// 50th-percentile (median) in ms.
    pub p50_ms: u64,
    /// 95th-percentile in ms.
    pub p95_ms: u64,
    /// 99th-percentile in ms.
    pub p99_ms: u64,
}

impl Percentiles {
    /// Compute percentiles from a non-empty sample vector.
    ///
    /// Returns `None` when `samples` is empty.
    pub fn from_samples(mut samples: Vec<u64>) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }
        let sum: u64 = samples.iter().sum();
        let count = samples.len();
        let mean_ms = sum as f64 / count as f64;
        let min_ms = *samples.iter().min().unwrap();
        let max_ms = *samples.iter().max().unwrap();
        samples.sort_unstable();
        let p50_ms = samples[count / 2];
        let p95_ms = samples[(count * 95 / 100).min(count - 1)];
        let p99_ms = samples[(count * 99 / 100).min(count - 1)];
        Some(Percentiles {
            count,
            mean_ms,
            min_ms,
            max_ms,
            p50_ms,
            p95_ms,
            p99_ms,
        })
    }
}

// ── LatencyReport ─────────────────────────────────────────────────────────────

/// Latency and cortex reliability report.
///
/// Produced by [`compute_latency_report`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LatencyReport {
    /// Time-to-first-action statistics from `CortexInvoked` entries.
    ///
    /// `None` when no `CortexInvoked` entries were observed.
    pub first_action: Option<Percentiles>,
    /// Total cortex invocations in the window.
    pub cortex_invocations: usize,
    /// Number of cortex faults.
    pub cortex_faults: usize,
    /// Cortex fault rate as a percentage (`faults / invocations × 100`).
    ///
    /// `0.0` when no invocations were observed.
    pub fault_rate_pct: f64,
    /// Total tool calls across all successful cortex completions.
    pub total_tool_calls: usize,
    /// Mean tool calls per successful cortex completion.
    ///
    /// `0.0` when no completions were observed.
    pub mean_tool_calls_per_completion: f64,
}

// ── compute_latency_report ────────────────────────────────────────────────────

/// Fold `entries` into a [`LatencyReport`].
pub fn compute_latency_report(entries: &[AuditEntry]) -> LatencyReport {
    let mut first_action_samples: Vec<u64> = Vec::new();
    let mut cortex_invocations = 0usize;
    let mut cortex_faults = 0usize;
    let mut total_tool_calls = 0usize;
    let mut completions = 0usize;

    for entry in entries {
        match entry {
            AuditEntry::CortexInvoked {
                latency_to_first_action_ms,
                ..
            } => {
                cortex_invocations += 1;
                first_action_samples.push(*latency_to_first_action_ms);
            }
            AuditEntry::CortexFault { .. } => {
                cortex_faults += 1;
            }
            AuditEntry::CortexCompleted { tool_calls, .. } => {
                total_tool_calls += tool_calls;
                completions += 1;
            }
            _ => {}
        }
    }

    let fault_rate_pct = if cortex_invocations > 0 {
        cortex_faults as f64 / cortex_invocations as f64 * 100.0
    } else {
        0.0
    };

    let mean_tool_calls_per_completion = if completions > 0 {
        total_tool_calls as f64 / completions as f64
    } else {
        0.0
    };

    LatencyReport {
        first_action: Percentiles::from_samples(first_action_samples),
        cortex_invocations,
        cortex_faults,
        fault_rate_pct,
        total_tool_calls,
        mean_tool_calls_per_completion,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use vita::audit::AuditEntry;

    fn invoked(ms: u64) -> AuditEntry {
        AuditEntry::CortexInvoked {
            task_id: "t1".into(),
            latency_to_first_action_ms: ms,
        }
    }

    fn fault() -> AuditEntry {
        AuditEntry::CortexFault {
            task_id: "t1".into(),
            error: "boom".into(),
        }
    }

    fn completed(tool_calls: usize) -> AuditEntry {
        AuditEntry::CortexCompleted {
            task_id: "t1".into(),
            tool_calls,
            summary_len: 42,
        }
    }

    #[test]
    fn empty_entries_produce_zero_report() {
        let r = compute_latency_report(&[]);
        assert_eq!(r.cortex_invocations, 0);
        assert_eq!(r.cortex_faults, 0);
        assert_eq!(r.fault_rate_pct, 0.0);
        assert!(r.first_action.is_none());
    }

    #[test]
    fn single_invocation_has_correct_latency() {
        let r = compute_latency_report(&[invoked(84)]);
        let p = r.first_action.unwrap();
        assert_eq!(p.count, 1);
        assert_eq!(p.p50_ms, 84);
        assert_eq!(p.mean_ms, 84.0);
    }

    #[test]
    fn fault_rate_calculated_correctly() {
        let entries = vec![invoked(10), fault()];
        let r = compute_latency_report(&entries);
        assert_eq!(r.cortex_invocations, 1);
        assert_eq!(r.cortex_faults, 1);
        assert!((r.fault_rate_pct - 100.0).abs() < 1e-9);
    }

    #[test]
    fn fault_rate_is_zero_with_no_invocations() {
        let r = compute_latency_report(&[fault()]);
        assert_eq!(r.fault_rate_pct, 0.0);
    }

    #[test]
    fn mean_tool_calls_computed_correctly() {
        let entries = vec![completed(2), completed(4)];
        let r = compute_latency_report(&entries);
        assert!((r.mean_tool_calls_per_completion - 3.0).abs() < 1e-9);
        assert_eq!(r.total_tool_calls, 6);
    }

    #[test]
    fn percentiles_ordered_for_large_sample() {
        let entries: Vec<AuditEntry> = (1u64..=100).map(invoked).collect();
        let r = compute_latency_report(&entries);
        let p = r.first_action.unwrap();
        assert!(p.p50_ms <= p.p95_ms);
        assert!(p.p95_ms <= p.p99_ms);
        assert!(p.p99_ms <= p.max_ms);
    }

    #[test]
    fn no_completions_gives_zero_mean_tool_calls() {
        let r = compute_latency_report(&[invoked(50)]);
        assert_eq!(r.mean_tool_calls_per_completion, 0.0);
    }
}
