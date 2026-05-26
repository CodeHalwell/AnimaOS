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
//! The sensor maintains an in-process ledger of spend records. The caller
//! supplies token counts and model names; the sensor applies its cost table
//! to derive USD amounts. This keeps the crate free of network I/O while
//! remaining useful for real workloads.

#![forbid(unsafe_code)]

use std::collections::HashMap;

// ── Spend record ──────────────────────────────────────────────────────────────

/// A single API spend event recorded by the caller.
#[derive(Debug, Clone)]
pub struct SpendRecord {
    /// Provider name (e.g. `"anthropic"`, `"openai"`).
    pub provider: String,
    /// Total tokens consumed (input + output combined).
    pub tokens: u64,
    /// Model name (e.g. `"claude-sonnet-4-6"`, `"gpt-4o"`).
    pub model: String,
    /// Wall-clock timestamp in nanoseconds since the Unix epoch.
    pub timestamp_ns: u64,
}

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
/// Maintains an in-process spend ledger and derives the normalised
/// `financial_budget` scalar for the Striatal Gate (E5.2) and the
/// Thalamic Router (E5.3) modulation path (E5.7).
#[derive(Debug, Clone)]
pub struct FinancialBudgetSensor {
    config: BudgetConfig,
    costs: CostTable,
    ledger: Vec<SpendRecord>,
}

impl FinancialBudgetSensor {
    /// Creates a sensor with the given budget config and cost table.
    pub fn new(config: BudgetConfig, costs: CostTable) -> Self {
        Self {
            config,
            costs,
            ledger: Vec::new(),
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
    pub fn record_spend(&mut self, provider: &str, tokens: u64, model: &str, timestamp_ns: u64) {
        // Prune stale records (any day other than the current UTC day) before
        // appending.  `spend_usd_on_day` only sums records from today, so
        // historical records accumulate unused and would cause O(N) growth.
        const DAY_NS: u64 = 86_400_000_000_000;
        let current_day = timestamp_ns / DAY_NS;
        self.ledger
            .retain(|r| r.timestamp_ns / DAY_NS == current_day);
        self.ledger.push(SpendRecord {
            provider: provider.to_owned(),
            tokens,
            model: model.to_owned(),
            timestamp_ns,
        });
    }

    /// Returns the total API spend in USD for the UTC day that contains
    /// `reference_ns` (nanoseconds since the Unix epoch).
    ///
    /// Day boundaries are midnight UTC, computed as integer division by
    /// 86 400 × 10⁹ (nanoseconds per day).
    pub fn spend_usd_on_day(&self, reference_ns: u64) -> f64 {
        const DAY_NS: u64 = 86_400_000_000_000;
        let day_bucket = reference_ns / DAY_NS;
        self.ledger
            .iter()
            .filter(|r| r.timestamp_ns / DAY_NS == day_bucket)
            .map(|r| self.costs.cost_usd(r.tokens, &r.model))
            .sum()
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

    /// Returns the number of spend records in the ledger.
    pub fn ledger_len(&self) -> usize {
        self.ledger.len()
    }

    /// Returns the budget configuration.
    pub fn config(&self) -> &BudgetConfig {
        &self.config
    }

    /// Clears all spend records for the current day (useful for testing or
    /// for resetting the ledger at midnight).
    pub fn clear_ledger(&mut self) {
        self.ledger.clear();
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
    fn record_spend_accumulates_in_ledger() {
        let mut sensor = FinancialBudgetSensor::with_defaults();
        sensor.record_spend("openai", 100, "gpt-4o", ts(0, 0));
        sensor.record_spend("anthropic", 200, "claude-sonnet-4-6", ts(0, 1));
        assert_eq!(sensor.ledger_len(), 2);
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
