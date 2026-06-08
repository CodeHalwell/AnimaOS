//! Token usage analysis — S25.1.
//!
//! Derives token-spend statistics from `TaskStarted` / `TaskCompleted` /
//! `TaskFailed` entries in the audit log.  No live network or provider
//! instrumentation is required; all data comes from the entries already written
//! by the scheduler and vita somatic loop.

use serde::{Deserialize, Serialize};
use vita::audit::AuditEntry;

// ── TokenStats ────────────────────────────────────────────────────────────────

/// Descriptive statistics for a sample of per-task token counts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TokenStats {
    /// Number of samples.
    pub count: usize,
    /// Sum of all samples.
    pub total: u64,
    /// Arithmetic mean.
    pub mean: f64,
    /// Minimum value.
    pub min: u64,
    /// Maximum value.
    pub max: u64,
    /// 50th-percentile (median).
    pub p50: u64,
    /// 95th-percentile.
    pub p95: u64,
    /// 99th-percentile.
    pub p99: u64,
}

impl TokenStats {
    /// Compute statistics from a non-empty sample vector.
    ///
    /// Returns `None` when `samples` is empty.
    pub fn from_samples(mut samples: Vec<u64>) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }
        samples.sort_unstable();
        let total: u64 = samples.iter().sum();
        let count = samples.len();
        let mean = total as f64 / count as f64;
        let min = samples[0];
        let max = samples[count - 1];
        let p50 = samples[count / 2];
        let p95 = samples[(count * 95 / 100).min(count - 1)];
        let p99 = samples[(count * 99 / 100).min(count - 1)];
        Some(TokenStats {
            count,
            total,
            mean,
            min,
            max,
            p50,
            p95,
            p99,
        })
    }
}

// ── TierTokenStats ────────────────────────────────────────────────────────────

/// Token usage aggregated by MLFQ tier.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TierTokenStats {
    /// MLFQ tier index (0 = High, 1 = Medium, 2 = Low).
    pub tier: u8,
    /// Number of tasks dispatched on this tier.
    pub tasks: usize,
    /// Total tokens emitted by tasks on this tier.
    pub total_tokens: u64,
    /// Mean tokens per task on this tier.
    pub mean_tokens: f64,
}

// ── TokenReport ───────────────────────────────────────────────────────────────

/// Token usage report derived from `TaskCompleted` audit entries.
///
/// Produced by [`compute_token_report`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TokenReport {
    /// Total tokens emitted across all completed tasks in the window.
    pub total_tokens: u64,
    /// Number of tasks that completed successfully.
    pub tasks_completed: usize,
    /// Number of tasks that failed or were cancelled.
    pub tasks_failed: usize,
    /// Per-task token statistics (`None` when no completions were observed).
    pub per_task: Option<TokenStats>,
    /// Token usage broken down by MLFQ dispatch tier.
    pub by_tier: Vec<TierTokenStats>,
}

// ── compute_token_report ──────────────────────────────────────────────────────

/// Fold `entries` into a [`TokenReport`].
///
/// The function makes a two-pass scan: first pass correlates `task_id` to
/// `tier` from `TaskStarted` entries; second pass aggregates token counts from
/// `TaskCompleted` entries using those correlations.
pub fn compute_token_report(entries: &[AuditEntry]) -> TokenReport {
    use std::collections::HashMap;

    // First pass: build task_id → tier map.
    let mut task_tiers: HashMap<u64, u8> = HashMap::new();
    let mut tasks_failed = 0usize;
    for entry in entries {
        match entry {
            AuditEntry::TaskStarted { task_id, tier, .. } => {
                task_tiers.insert(*task_id, *tier);
            }
            AuditEntry::TaskFailed { .. } => {
                tasks_failed += 1;
            }
            _ => {}
        }
    }

    // Second pass: aggregate tokens and correlate to tiers.
    let mut all_tokens: Vec<u64> = Vec::new();
    // tier → (task_count, total_tokens)
    let mut tier_agg: HashMap<u8, (usize, u64)> = HashMap::new();

    for entry in entries {
        if let AuditEntry::TaskCompleted {
            task_id,
            tokens_emitted,
            ..
        } = entry
        {
            let t = *tokens_emitted as u64;
            all_tokens.push(t);
            if let Some(&tier) = task_tiers.get(task_id) {
                let e = tier_agg.entry(tier).or_insert((0, 0));
                e.0 += 1;
                e.1 += t;
            }
        }
    }

    let total_tokens: u64 = all_tokens.iter().sum();
    let tasks_completed = all_tokens.len();
    let per_task = TokenStats::from_samples(all_tokens);

    let mut by_tier: Vec<TierTokenStats> = tier_agg
        .into_iter()
        .map(|(tier, (tasks, total_tokens))| TierTokenStats {
            tier,
            tasks,
            total_tokens,
            mean_tokens: if tasks > 0 {
                total_tokens as f64 / tasks as f64
            } else {
                0.0
            },
        })
        .collect();
    by_tier.sort_by_key(|t| t.tier);

    TokenReport {
        total_tokens,
        tasks_completed,
        tasks_failed,
        per_task,
        by_tier,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use vita::audit::AuditEntry;

    fn make_started(task_id: u64, tier: u8) -> AuditEntry {
        AuditEntry::TaskStarted {
            agent_id: "a".into(),
            task_id,
            tier,
            prompt: "test".into(),
        }
    }

    fn make_completed(task_id: u64, tokens: u32) -> AuditEntry {
        AuditEntry::TaskCompleted {
            agent_id: "a".into(),
            task_id,
            tokens_emitted: tokens,
            response: "ok".into(),
        }
    }

    fn make_failed(task_id: u64) -> AuditEntry {
        AuditEntry::TaskFailed {
            agent_id: "a".into(),
            task_id,
            error: "err".into(),
        }
    }

    #[test]
    fn empty_entries_produce_zero_report() {
        let r = compute_token_report(&[]);
        assert_eq!(r.total_tokens, 0);
        assert_eq!(r.tasks_completed, 0);
        assert_eq!(r.tasks_failed, 0);
        assert!(r.per_task.is_none());
        assert!(r.by_tier.is_empty());
    }

    #[test]
    fn single_completed_task_totals_are_correct() {
        let entries = vec![make_started(1, 0), make_completed(1, 100)];
        let r = compute_token_report(&entries);
        assert_eq!(r.total_tokens, 100);
        assert_eq!(r.tasks_completed, 1);
        assert_eq!(r.tasks_failed, 0);
        let stats = r.per_task.unwrap();
        assert_eq!(stats.total, 100);
        assert_eq!(stats.count, 1);
    }

    #[test]
    fn failed_tasks_counted_independently_of_completions() {
        let entries = vec![make_started(1, 0), make_completed(1, 50), make_failed(2)];
        let r = compute_token_report(&entries);
        assert_eq!(r.tasks_completed, 1);
        assert_eq!(r.tasks_failed, 1);
    }

    #[test]
    fn tokens_aggregated_by_tier_correctly() {
        let entries = vec![
            make_started(1, 0),
            make_started(2, 0),
            make_started(3, 2),
            make_completed(1, 100),
            make_completed(2, 200),
            make_completed(3, 50),
        ];
        let r = compute_token_report(&entries);
        assert_eq!(r.total_tokens, 350);
        let tier0 = r.by_tier.iter().find(|t| t.tier == 0).unwrap();
        assert_eq!(tier0.tasks, 2);
        assert_eq!(tier0.total_tokens, 300);
        assert!((tier0.mean_tokens - 150.0).abs() < 1e-9);
        let tier2 = r.by_tier.iter().find(|t| t.tier == 2).unwrap();
        assert_eq!(tier2.tasks, 1);
        assert_eq!(tier2.total_tokens, 50);
    }

    #[test]
    fn token_stats_percentiles_are_ordered() {
        let samples: Vec<u64> = (1..=100).collect();
        let stats = TokenStats::from_samples(samples).unwrap();
        assert!(stats.p50 <= stats.p95);
        assert!(stats.p95 <= stats.p99);
        assert!(stats.p99 <= stats.max);
    }

    #[test]
    fn token_stats_returns_none_for_empty() {
        assert!(TokenStats::from_samples(vec![]).is_none());
    }

    #[test]
    fn mean_tokens_computed_correctly() {
        let entries = vec![
            make_started(1, 1),
            make_started(2, 1),
            make_completed(1, 100),
            make_completed(2, 200),
        ];
        let r = compute_token_report(&entries);
        let stats = r.per_task.unwrap();
        assert!((stats.mean - 150.0).abs() < 1e-9);
    }
}
