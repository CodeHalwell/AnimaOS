// crates/interoception/src/budget.rs
//! Financial budget sensor — E5.7 Story S5.7.2.
//!
//! Tracks API token consumption per provider and model against configurable
//! daily and monthly spending limits. Exports a normalised `financial_budget`
//! scalar ∈ `[0, 1]`:
//!
//! ```text
//! financial_budget = 1.0 − (today_spend_usd / daily_usd_limit)
//!                  = clamp to [0, 1]
//! ```
//!
//! `1.0` → budget untouched; `0.0` → daily limit reached or exceeded.
//!
//! ## Design
//!
//! The sensor aggregates spend into per-UTC-day USD totals. The caller supplies
//! token counts and model names; the sensor applies its cost table to derive USD
//! amounts at record time. Aggregating — rather than retaining individual spend
//! events — bounds memory by the number of distinct days, never drops same-day
//! spend (no per-event cap to overflow), and keeps a stray out-of-order or
//! future-dated timestamp confined to its own day bucket instead of corrupting
//! the active day (CORE-5). This keeps the crate free of network I/O while
//! remaining useful for real workloads.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap};

// ── Cost table ────────────────────────────────────────────────────────────────

/// Cost-per-million-tokens lookup table for provider/model pairs.
///
/// Keys are model name strings; the special key `"*"` is the per-provider
/// wildcard fallback. Values are USD per 1 million tokens.
///
/// ```
/// use interoception::CostTable;
/// use std::collections::HashMap;
/// let mut ct = CostTable::default();
/// ct.0.insert("claude-sonnet-4-6".into(), 3.0);  // $3 / 1 M tokens
/// ct.0.insert("*".into(), 1.0);                  // default $1 / 1 M tokens
/// assert!((ct.cost_usd(1_000_000, "claude-sonnet-4-6") - 3.0).abs() < 1e-9);
/// assert!((ct.cost_usd(1_000_000, "unknown-model") - 1.0).abs() < 1e-9);
/// ```
#[derive(Debug, Clone, Default)]
pub struct CostTable(pub HashMap<String, f64>);

impl CostTable {
    /// Returns the cost in USD for `tokens` tokens on `model`.
    ///
    /// Lookup order: exact model match → wildcard `"*"` → `0.0` (free).
    pub fn cost_usd(&self, tokens: u64, model: &str) -> f64 {
        let rate = self
            .0
            .get(model)
            .or_else(|| self.0.get("*"))
            .copied()
            .unwrap_or(0.0);
        rate * (tokens as f64 / 1_000_000.0)
    }
}

// ── Budget configuration ──────────────────────────────────────────────────────

/// Spending limit configuration.
#[derive(Debug, Clone)]
pub struct BudgetConfig {
    /// Maximum total API spend in USD per UTC calendar day.
    ///
    /// The `financial_budget` scalar is computed against this limit.
    pub daily_usd_limit: f64,
    /// Maximum total API spend in USD per UTC calendar month.
    ///
    /// Currently informational — the scalar is keyed to the daily limit.
    pub monthly_usd_limit: f64,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            daily_usd_limit: 5.0,
            monthly_usd_limit: 100.0,
        }
    }
}

// ── Financial budget sensor ───────────────────────────────────────────────────

/// Financial budget sensor (S5.7.2).
///
/// Aggregates spend into per-UTC-day USD totals and derives the normalised
/// `financial_budget` scalar for the Striatal Gate (E5.2) and the
/// Thalamic Router (E5.3) modulation path (E5.7).
#[derive(Debug, Clone)]
pub struct FinancialBudgetSensor {
    config: BudgetConfig,
    costs: CostTable,
    /// Spend in USD keyed by UTC day (`timestamp_ns / DAY_NS`), aggregated at
    /// record time. Bounded to the most recent [`Self::MAX_DAYS`] days.
    daily_usd: BTreeMap<u64, f64>,
}

impl FinancialBudgetSensor {
    /// Nanoseconds per UTC day (the day-bucket width).
    const DAY_NS: u64 = 86_400_000_000_000;

    /// Rolling retention window: a daily/monthly budget never needs older
    /// history, so only the most recent `MAX_DAYS` day-buckets are kept.
    const MAX_DAYS: usize = 90;

    /// Creates a sensor with the given budget config and cost table.
    pub fn new(config: BudgetConfig, costs: CostTable) -> Self {
        Self {
            config,
            costs,
            daily_usd: BTreeMap::new(),
        }
    }

    /// Creates a sensor with the default budget limits and an empty cost table.
    ///
    /// With an empty cost table all spend is recorded as $0.00 (effectively
    /// unlimited budget), which is the safe default for tests and new
    /// deployments that have not yet configured pricing.
    pub fn with_defaults() -> Self {
        Self::new(BudgetConfig::default(), CostTable::default())
    }

    /// Records a spend event.
    ///
    /// The caller must supply `timestamp_ns` as nanoseconds since the Unix
    /// epoch.  Use `std::time::SystemTime::UNIX_EPOCH.elapsed()` or a
    /// monotonic approximation in production; use a fixed constant in tests.
    ///
    /// The event's USD cost is derived from the cost table at record time and
    /// added to its UTC-day bucket. `provider` is accepted for call-site
    /// symmetry but does not affect cost (the cost table is keyed by model).
    pub fn record_spend(&mut self, _provider: &str, tokens: u64, model: &str, timestamp_ns: u64) {
        // Aggregate into the day bucket rather than retaining individual events.
        // This bounds memory by distinct days (not event volume, so a high-volume
        // day can't overflow a per-event cap and undercount), and a stray
        // out-of-order or future-dated timestamp simply lands in its own bucket
        // instead of wiping the active day or pinning a fragile max-day pruning
        // anchor (CORE-5).
        let day = timestamp_ns / Self::DAY_NS;
        let cost = self.costs.cost_usd(tokens, model);
        *self.daily_usd.entry(day).or_insert(0.0) += cost;

        // Retain only the most recent MAX_DAYS buckets. Pruning by day (never by
        // event count) means no same-day spend is ever dropped.
        while self.daily_usd.len() > Self::MAX_DAYS {
            let Some(&oldest) = self.daily_usd.keys().next() else {
                break;
            };
            self.daily_usd.remove(&oldest);
        }
    }

    /// Returns the total API spend in USD for the UTC day that contains
    /// `reference_ns` (nanoseconds since the Unix epoch).
    ///
    /// Day boundaries are midnight UTC, computed as integer division by
    /// 86 400 × 10⁹ (nanoseconds per day).
    pub fn spend_usd_on_day(&self, reference_ns: u64) -> f64 {
        let day = reference_ns / Self::DAY_NS;
        self.daily_usd.get(&day).copied().unwrap_or(0.0)
    }

    /// Normalised financial budget scalar for the UTC day that contains
    /// `now_ns`.
    ///
    /// Returns a value in `[0, 1]`:
    /// - `1.0` — daily budget is completely untouched.
    /// - `0.0` — daily limit reached or exceeded.
    /// - When `daily_usd_limit ≤ 0`, always returns `0.0`.
    pub fn financial_budget_scalar(&self, now_ns: u64) -> f32 {
        if self.config.daily_usd_limit <= 0.0 {
            return 0.0;
        }
        let spent = self.spend_usd_on_day(now_ns);
        let fraction_used = (spent / self.config.daily_usd_limit).clamp(0.0, 1.0);
        (1.0 - fraction_used) as f32
    }

    /// Returns the number of distinct UTC days currently holding recorded spend.
    pub fn ledger_len(&self) -> usize {
        self.daily_usd.len()
    }

    /// Returns the budget configuration.
    pub fn config(&self) -> &BudgetConfig {
        &self.config
    }

    /// Clears all recorded spend (useful for testing or for a manual reset).
    pub fn clear_ledger(&mut self) {
        self.daily_usd.clear();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: nanoseconds for day `d`, hour `h` (UTC, epoch = day 0).
    fn ts(d: u64, h: u64) -> u64 {
        d * 86_400_000_000_000 + h * 3_600_000_000_000
    }

    fn sensor_at_ten_dollars_per_million() -> FinancialBudgetSensor {
        let mut costs = CostTable::default();
        costs.0.insert("*".to_owned(), 10.0); // $10 / 1 M tokens
        FinancialBudgetSensor::new(
            BudgetConfig {
                daily_usd_limit: 10.0,
                monthly_usd_limit: 100.0,
            },
            costs,
        )
    }

    #[test]
    fn fresh_sensor_budget_scalar_is_one() {
        let sensor = sensor_at_ten_dollars_per_million();
        assert_eq!(sensor.financial_budget_scalar(ts(1, 12)), 1.0);
    }

    #[test]
    fn half_daily_budget_spent_gives_scalar_of_half() {
        let mut sensor = sensor_at_ten_dollars_per_million();
        // $10 / 1 M × 500 k = $5 = 50% of $10 daily limit
        sensor.record_spend("anthropic", 500_000, "model", ts(1, 9));
        let scalar = sensor.financial_budget_scalar(ts(1, 12));
        assert!((scalar - 0.5).abs() < 1e-5, "expected 0.5, got {scalar}");
    }

    #[test]
    fn exceeded_budget_clamps_to_zero() {
        let mut sensor = sensor_at_ten_dollars_per_million();
        // $10 / 1 M × 2 M = $20 > $10 daily limit → clamped to 0.0
        sensor.record_spend("anthropic", 2_000_000, "model", ts(1, 9));
        assert_eq!(sensor.financial_budget_scalar(ts(1, 12)), 0.0);
    }

    #[test]
    fn spend_on_different_day_does_not_affect_today() {
        let mut sensor = sensor_at_ten_dollars_per_million();
        sensor.record_spend("anthropic", 2_000_000, "model", ts(1, 9));
        // Query on day 2 — fresh budget
        assert_eq!(sensor.financial_budget_scalar(ts(2, 9)), 1.0);
    }

    #[test]
    fn multiple_providers_accumulate_spend_on_same_day() {
        let mut sensor = sensor_at_ten_dollars_per_million();
        sensor.record_spend("anthropic", 250_000, "model", ts(1, 0)); // $2.50
        sensor.record_spend("openai", 250_000, "model", ts(1, 1)); // $2.50
                                                                   // total = $5 / $10 = 0.5
        let scalar = sensor.financial_budget_scalar(ts(1, 12));
        assert!((scalar - 0.5).abs() < 1e-5, "expected 0.5, got {scalar}");
    }

    #[test]
    fn out_of_order_older_timestamp_does_not_wipe_todays_spend() {
        let mut sensor = sensor_at_ten_dollars_per_million();
        // Spend half the daily budget on day 5.
        sensor.record_spend("anthropic", 500_000, "model", ts(5, 10)); // $5
        assert!((sensor.financial_budget_scalar(ts(5, 12)) - 0.5).abs() < 1e-5);
        // A late/out-of-order record from the PREVIOUS day must not prune
        // day 5's ledger and reset the budget scalar back toward 1.0 (CORE-5).
        sensor.record_spend("anthropic", 100_000, "model", ts(4, 23)); // day 4
        let scalar = sensor.financial_budget_scalar(ts(5, 12));
        assert!(
            (scalar - 0.5).abs() < 1e-5,
            "day-5 spend must survive an out-of-order day-4 record, got {scalar}"
        );
    }

    #[test]
    fn future_dated_record_does_not_drop_todays_spend() {
        let mut sensor = sensor_at_ten_dollars_per_million();
        // Real day 5: two $2.50 charges.
        sensor.record_spend("anthropic", 250_000, "model", ts(5, 8));
        sensor.record_spend("anthropic", 250_000, "model", ts(5, 9));
        // A future-dated record (clock skew / replayed provider usage) arrives.
        sensor.record_spend("anthropic", 250_000, "model", ts(100, 0));
        // A subsequent legitimate day-5 record must still accumulate: the future
        // record must not have become a pruning anchor that dropped the earlier
        // day-5 spend (CORE-5).
        sensor.record_spend("anthropic", 250_000, "model", ts(5, 10));
        // Day-5 spend = $7.50 of the $10 limit → 0.25 remaining. If the future
        // record had pinned the anchor, earlier day-5 records would be pruned and
        // the scalar would wrongly climb back toward 1.0.
        let scalar = sensor.financial_budget_scalar(ts(5, 12));
        assert!(
            (scalar - 0.25).abs() < 1e-5,
            "day-5 spend must survive a future-dated record, got {scalar}"
        );
    }

    #[test]
    fn zero_daily_limit_always_returns_zero() {
        let sensor = FinancialBudgetSensor::new(
            BudgetConfig {
                daily_usd_limit: 0.0,
                monthly_usd_limit: 0.0,
            },
            CostTable::default(),
        );
        assert_eq!(sensor.financial_budget_scalar(ts(0, 0)), 0.0);
    }

    #[test]
    fn record_spend_aggregates_same_day_into_one_bucket() {
        let mut sensor = sensor_at_ten_dollars_per_million();
        sensor.record_spend("openai", 250_000, "model", ts(0, 0)); // $2.50
        sensor.record_spend("anthropic", 250_000, "model", ts(0, 1)); // $2.50
                                                                      // Two same-day events aggregate into a single day bucket…
        assert_eq!(sensor.ledger_len(), 1);
        // …and their spend accumulates exactly.
        assert!((sensor.spend_usd_on_day(ts(0, 12)) - 5.0).abs() < 1e-9);
        // A record on a different day opens a second bucket.
        sensor.record_spend("openai", 250_000, "model", ts(1, 0));
        assert_eq!(sensor.ledger_len(), 2);
    }

    #[test]
    fn high_volume_same_day_spend_is_not_capped() {
        let mut sensor = sensor_at_ten_dollars_per_million();
        // 20 000 tiny same-day charges — far past any plausible per-event cap.
        // With per-day aggregation none are dropped, so the total is exact.
        for _ in 0..20_000 {
            sensor.record_spend("anthropic", 100, "model", ts(3, 6)); // $0.001 each
        }
        // 20 000 × $0.001 = $20 → over the $10 daily limit → clamped to 0.0.
        assert_eq!(sensor.financial_budget_scalar(ts(3, 12)), 0.0);
        assert_eq!(sensor.ledger_len(), 1, "all same-day spend in one bucket");
    }

    #[test]
    fn old_days_are_pruned_beyond_the_retention_window() {
        let mut sensor = sensor_at_ten_dollars_per_million();
        // Record one event per day across more than the retention window.
        for day in 0..(FinancialBudgetSensor::MAX_DAYS as u64 + 10) {
            sensor.record_spend("anthropic", 100, "model", ts(day, 0));
        }
        // Memory stays bounded to the window…
        assert_eq!(sensor.ledger_len(), FinancialBudgetSensor::MAX_DAYS);
        // …keeping the most recent day and dropping the oldest.
        let last = FinancialBudgetSensor::MAX_DAYS as u64 + 9;
        assert!(sensor.spend_usd_on_day(ts(last, 12)) > 0.0);
        assert_eq!(sensor.spend_usd_on_day(ts(0, 12)), 0.0);
    }

    #[test]
    fn exact_model_match_takes_priority_over_wildcard() {
        let mut costs = CostTable::default();
        costs.0.insert("premium-model".to_owned(), 20.0);
        costs.0.insert("*".to_owned(), 1.0);
        let sensor = FinancialBudgetSensor::new(
            BudgetConfig {
                daily_usd_limit: 20.0,
                monthly_usd_limit: 200.0,
            },
            costs,
        );
        let mut s = sensor.clone();
        // $20 / 1 M × 1 M = $20 = full daily limit → 0.0
        s.record_spend("any", 1_000_000, "premium-model", ts(0, 0));
        assert_eq!(s.financial_budget_scalar(ts(0, 12)), 0.0);
    }

    #[test]
    fn clear_ledger_resets_spend_to_zero() {
        let mut sensor = sensor_at_ten_dollars_per_million();
        sensor.record_spend("anthropic", 1_000_000, "model", ts(1, 9));
        assert_eq!(sensor.financial_budget_scalar(ts(1, 12)), 0.0);
        sensor.clear_ledger();
        assert_eq!(sensor.financial_budget_scalar(ts(1, 12)), 1.0);
    }

    #[test]
    fn cost_table_wildcard_used_when_model_not_listed() {
        let mut costs = CostTable::default();
        costs.0.insert("*".to_owned(), 5.0); // $5 / 1 M
        let ct = costs;
        assert!((ct.cost_usd(1_000_000, "any-unknown-model") - 5.0).abs() < 1e-9);
    }

    #[test]
    fn cost_table_free_fallback_when_no_entry() {
        let ct = CostTable::default();
        assert_eq!(ct.cost_usd(999_999, "any-model"), 0.0);
    }
}
