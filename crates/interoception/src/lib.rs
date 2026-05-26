#![forbid(unsafe_code)]

//! Interoceptive engine: real-time telemetry for autonomic self-regulation.
//!
//! # E5.7 — Interoceptive Modulation
//!
//! This crate is extended in E5.7 with three new modules that complete the
//! sensor layer described in Stories S5.7.1–S5.7.3:
//!
//! | Module | Story | Purpose |
//! |--------|-------|---------|
//! [`signals`] | S5.7.1 | Canonical [`InteroceptiveSignals`] struct + [`SignalPublisher`] trait |
//! [`budget`]  | S5.7.2 | [`FinancialBudgetSensor`] — tracks API spend against daily limits |
//! [`power`]   | S5.7.3 | [`PowerSensor`] + [`AttentionSensor`] — opt-in hardware sensors |
//!
//! The [`InteroceptiveSensorBundle`] aggregates all three sensor families and
//! produces a single [`InteroceptiveSignals`] snapshot on each tick. Consumers
//! (the Striatal Gate and the Thalamic Router in `vita`) call
//! `HomeostaticSignals::from_interoceptive(&snapshot)` to convert the
//! canonical signals into the form the gate and router expect.

use std::collections::VecDeque;

pub mod budget;
pub mod power;
pub mod signals;

pub use budget::{BudgetConfig, CostTable, FinancialBudgetSensor, SpendRecord};
pub use power::{
    AttentionConfig, AttentionReading, AttentionSensor, PowerConfig, PowerReading, PowerSensor,
};
pub use signals::{FnPublisher, InteroceptiveSignals, NullPublisher, SignalPublisher};

// ── HomeostaticMonitor (E3.2) ─────────────────────────────────────────────────

/// Tracks interoceptive telemetry for lifecycle self-regulation.
///
/// Originally from E3.2; extended in E5.7 to serve as a compute-pressure and
/// thermal-load proxy via [`HomeostaticMonitor::compute_systemic_stress_index`].
#[derive(Debug, Clone)]
pub struct HomeostaticMonitor {
    /// Rolling TTFT samples (time-to-first-token, in milliseconds).
    pub rolling_ttft: VecDeque<f32>,
    /// Baseline TTFT value (milliseconds).
    pub baseline_ttft: f32,
    /// Balance parameter between latency and token pressure (0.0..=1.0).
    pub beta: f32,
    /// Maximum number of rolling samples retained.
    pub window_size: usize,
}

impl HomeostaticMonitor {
    /// Creates a monitor configured with sensible defaults.
    pub fn new(baseline_ttft: f32, beta: f32, window_size: usize) -> Self {
        Self {
            rolling_ttft: VecDeque::with_capacity(window_size.max(1)),
            baseline_ttft,
            beta: beta.clamp(0.0, 1.0),
            window_size: window_size.max(1),
        }
    }

    /// Records a new TTFT sample, evicting the oldest if the window is full.
    pub fn record_ttft(&mut self, sample_ms: f32) {
        if self.rolling_ttft.len() >= self.window_size {
            self.rolling_ttft.pop_front();
        }
        self.rolling_ttft.push_back(sample_ms);
    }

    /// Computes the composite systemic stress index.
    ///
    /// The result is a proxy for compute pressure / thermal load and is used
    /// by [`InteroceptiveSensorBundle::sample`] to populate the
    /// `thermal_load` and `compute_pressure` fields in E5.7.
    pub fn compute_systemic_stress_index(&self, active_tokens: u32, max_context: u32) -> f32 {
        let latency_ratio = if self.rolling_ttft.is_empty() {
            1.0
        } else {
            let avg_ttft = self.rolling_ttft.iter().sum::<f32>() / self.rolling_ttft.len() as f32;
            if self.baseline_ttft > 0.0 {
                avg_ttft / self.baseline_ttft
            } else {
                1.0
            }
        };
        let memory_ratio = if max_context == 0 {
            1.0
        } else {
            active_tokens as f32 / max_context as f32
        };

        (self.beta * latency_ratio) + ((1.0 - self.beta) * memory_ratio)
    }
}

// ── InteroceptiveSensorBundle (S5.7.1) ────────────────────────────────────────

/// Bundle of all interoceptive sensors (E5.7, S5.7.1–S5.7.3).
///
/// [`sample`] reads all sensors and returns a single [`InteroceptiveSignals`]
/// snapshot. The caller is responsible for injecting the latest
/// [`HomeostaticMonitor`] reading and the current L1 context occupancy so
/// that `thermal_load`, `compute_pressure`, and `memory_pressure` can be
/// derived from runtime state.
///
/// ```
/// use interoception::{HomeostaticMonitor, InteroceptiveSensorBundle};
///
/// let mut monitor = HomeostaticMonitor::new(1.0, 0.5, 8);
/// monitor.record_ttft(1.0); // baseline — no stress
/// let bundle = InteroceptiveSensorBundle::with_defaults();
/// let now_ns = 0u64; // test epoch
/// let signals = bundle.sample(&monitor, 0, 4096, now_ns);
/// assert!(signals.is_valid());
/// ```
pub struct InteroceptiveSensorBundle {
    /// Financial budget sensor (S5.7.2).
    pub financial: FinancialBudgetSensor,
    /// Power state sensor (S5.7.3).
    pub power: PowerSensor,
    /// Attention / user-presence sensor (S5.7.3).
    pub attention: AttentionSensor,
}

impl InteroceptiveSensorBundle {
    /// Creates a bundle with default sensor configuration.
    ///
    /// - Financial sensor: $5/day limit, empty cost table (all spend = $0).
    /// - Power sensor: opt-in disabled (always returns AC power).
    /// - Attention sensor: opt-in disabled (always returns user-present).
    pub fn with_defaults() -> Self {
        Self {
            financial: FinancialBudgetSensor::with_defaults(),
            power: PowerSensor::disabled(),
            attention: AttentionSensor::disabled(),
        }
    }

    /// Samples all sensors and derives the [`InteroceptiveSignals`] snapshot.
    ///
    /// # Parameters
    ///
    /// - `monitor`: the `HomeostaticMonitor` used to proxy compute/thermal load.
    /// - `active_tokens`: current L1 context occupancy in tokens.
    /// - `max_context`: maximum context capacity in tokens.
    /// - `now_ns`: current time in nanoseconds since the Unix epoch.
    ///
    /// # Derivation
    ///
    /// | Signal | Source |
    /// |--------|--------|
    /// | `thermal_load` | `monitor.compute_systemic_stress_index(0, 1).clamp(0,1)` — latency-only proxy |
    /// | `compute_pressure` | same as `thermal_load` (TTFT ratio) |
    /// | `memory_pressure` | `active_tokens / max_context` |
    /// | `power_budget` | `power.power_budget_scalar()` |
    /// | `financial_budget` | `financial.financial_budget_scalar(now_ns)` |
    /// | `attention_demand` | `attention.attention_demand_scalar()` |
    pub fn sample(
        &self,
        monitor: &HomeostaticMonitor,
        active_tokens: u32,
        max_context: u32,
        now_ns: u64,
    ) -> InteroceptiveSignals {
        // Use the latency component of the stress index as a compute/thermal proxy.
        // We query with `active_tokens = 0, max_context = 1` to isolate the
        // latency term (memory_ratio = 0.0 when active_tokens = 0).
        let latency_stress = monitor.compute_systemic_stress_index(0, 1).clamp(0.0, 1.0);

        let memory_pressure = if max_context == 0 {
            0.0f32
        } else {
            (active_tokens as f32 / max_context as f32).clamp(0.0, 1.0)
        };

        InteroceptiveSignals {
            thermal_load: latency_stress,
            compute_pressure: latency_stress,
            memory_pressure,
            power_budget: self.power.power_budget_scalar(),
            financial_budget: self.financial.financial_budget_scalar(now_ns),
            attention_demand: self.attention.attention_demand_scalar(),
        }
    }

    /// Publishes the current snapshot to `publisher` and returns the snapshot.
    ///
    /// Convenience wrapper for the 1 Hz tick loop: sample, publish, and
    /// return the value so the caller can use it for gate/router decisions.
    pub fn tick(
        &self,
        monitor: &HomeostaticMonitor,
        active_tokens: u32,
        max_context: u32,
        now_ns: u64,
        publisher: &dyn SignalPublisher,
    ) -> InteroceptiveSignals {
        let signals = self.sample(monitor, active_tokens, max_context, now_ns);
        publisher.publish(&signals);
        signals
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── HomeostaticMonitor (original tests) ──────────────────────────────────

    #[test]
    fn stress_index_includes_memory_pressure_without_ttft_samples() {
        let monitor = HomeostaticMonitor::new(1.0, 0.5, 16);
        assert_eq!(monitor.compute_systemic_stress_index(100, 1000), 0.55);
    }

    #[test]
    fn stress_index_combines_latency_and_memory_pressure() {
        let mut monitor = HomeostaticMonitor::new(2.0, 0.25, 16);
        monitor.record_ttft(2.0);
        monitor.record_ttft(4.0);
        let index = monitor.compute_systemic_stress_index(100, 200);
        assert!((index - 0.75).abs() < 1e-6);
    }

    #[test]
    fn record_ttft_evicts_oldest_when_window_full() {
        let mut monitor = HomeostaticMonitor::new(1.0, 0.5, 2);
        monitor.record_ttft(1.0);
        monitor.record_ttft(2.0);
        monitor.record_ttft(3.0);
        assert_eq!(monitor.rolling_ttft.len(), 2);
        assert_eq!(monitor.rolling_ttft.front().copied(), Some(2.0));
    }

    #[test]
    fn beta_is_clamped_to_unit_interval() {
        let monitor = HomeostaticMonitor::new(1.0, 5.0, 4);
        assert_eq!(monitor.beta, 1.0);
        let monitor = HomeostaticMonitor::new(1.0, -1.0, 4);
        assert_eq!(monitor.beta, 0.0);
    }

    // ── InteroceptiveSensorBundle (E5.7) ─────────────────────────────────────

    #[test]
    fn sample_with_neutral_monitor_produces_valid_signals() {
        let mut monitor = HomeostaticMonitor::new(1.0, 0.5, 8);
        monitor.record_ttft(1.0);
        let bundle = InteroceptiveSensorBundle::with_defaults();
        let signals = bundle.sample(&monitor, 0, 4096, 0);
        assert!(
            signals.is_valid(),
            "sample should produce valid signals: {signals:?}"
        );
    }

    #[test]
    fn memory_pressure_derived_from_active_tokens() {
        let monitor = HomeostaticMonitor::new(1.0, 0.5, 8);
        let bundle = InteroceptiveSensorBundle::with_defaults();
        // 2048 / 4096 = 0.5
        let signals = bundle.sample(&monitor, 2048, 4096, 0);
        assert!(
            (signals.memory_pressure - 0.5).abs() < 1e-5,
            "expected memory_pressure=0.5, got {}",
            signals.memory_pressure
        );
    }

    #[test]
    fn zero_max_context_does_not_panic_and_gives_zero_memory_pressure() {
        let monitor = HomeostaticMonitor::new(1.0, 0.5, 8);
        let bundle = InteroceptiveSensorBundle::with_defaults();
        let signals = bundle.sample(&monitor, 100, 0, 0);
        assert_eq!(signals.memory_pressure, 0.0);
    }

    #[test]
    fn financial_budget_sensor_integration_full_budget() {
        let monitor = HomeostaticMonitor::new(1.0, 0.5, 8);
        let bundle = InteroceptiveSensorBundle::with_defaults();
        let signals = bundle.sample(&monitor, 0, 4096, 0);
        // With defaults: no cost table → all spend free → budget always 1.0
        assert_eq!(
            signals.financial_budget, 1.0,
            "empty cost table means no spend deducted"
        );
    }

    #[test]
    fn power_budget_is_one_when_sensor_disabled() {
        let monitor = HomeostaticMonitor::new(1.0, 0.5, 8);
        let bundle = InteroceptiveSensorBundle::with_defaults();
        let signals = bundle.sample(&monitor, 0, 4096, 0);
        assert_eq!(
            signals.power_budget, 1.0,
            "disabled power sensor returns AC sentinel"
        );
    }

    #[test]
    fn tick_publishes_snapshot_and_returns_same_value() {
        use crate::signals::NullPublisher;
        let mut monitor = HomeostaticMonitor::new(1.0, 0.5, 8);
        monitor.record_ttft(1.0);
        let bundle = InteroceptiveSensorBundle::with_defaults();
        let sampled = bundle.sample(&monitor, 0, 4096, 0);
        let ticked = bundle.tick(&monitor, 0, 4096, 0, &NullPublisher);
        assert_eq!(
            sampled, ticked,
            "tick must publish and return the same snapshot as sample"
        );
    }

    #[test]
    fn tick_invokes_publisher_callback() {
        use crate::signals::FnPublisher;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = count.clone();
        let publisher = FnPublisher(move |_: &InteroceptiveSignals| {
            count_clone.fetch_add(1, Ordering::SeqCst);
        });
        let monitor = HomeostaticMonitor::new(1.0, 0.5, 8);
        let bundle = InteroceptiveSensorBundle::with_defaults();
        bundle.tick(&monitor, 0, 4096, 0, &publisher);
        bundle.tick(&monitor, 0, 4096, 0, &publisher);
        assert_eq!(
            count.load(Ordering::SeqCst),
            2,
            "publisher must be called once per tick"
        );
    }
}
