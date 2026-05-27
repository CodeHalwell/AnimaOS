#![forbid(unsafe_code)]
#![cfg_attr(not(feature = "std"), no_std)]

//! Interoceptive engine: real-time telemetry for autonomic self-regulation.
//!
//! # `no_std` support (E4.5)
//!
//! When built with `default-features = false`, the following subset is available:
//!
//! | Type                    | Always | std-only |
//! |------------------------|--------|----------|
//! | `InteroceptiveSignals` | ✓      |          |
//! | `HomeostaticMonitor`   | ✓      |          |
//! | `SignalPublisher` trait | ✓      |          |
//! | `NullPublisher`        | ✓      |          |
//! | `FnPublisher`          | ✓      |          |
//! | `FinancialBudgetSensor`|        | ✓        |
//! | `PowerSensor`          |        | ✓        |
//! | `AttentionSensor`      |        | ✓        |
//! | `InteroceptiveSensorBundle` |   | ✓        |

// alloc is needed for VecDeque in no_std builds.
#[cfg(not(feature = "std"))]
extern crate alloc;

// VecDeque lives in std::collections under std, alloc::collections under no_std.
#[cfg(not(feature = "std"))]
use alloc::collections::VecDeque;
#[cfg(feature = "std")]
use std::collections::VecDeque;

// std-only sensor modules (financial budget uses HashMap, power uses std::fs)
#[cfg(feature = "std")]
pub mod budget;
#[cfg(feature = "std")]
pub mod power;

// signals is always available (pure types)
pub mod signals;

// std-only re-exports
#[cfg(feature = "std")]
pub use budget::{BudgetConfig, CostTable, FinancialBudgetSensor, SpendRecord};
#[cfg(feature = "std")]
pub use power::{
    AttentionConfig, AttentionReading, AttentionSensor, PowerConfig, PowerReading, PowerSensor,
};

// Always-available re-exports
pub use signals::{FnPublisher, InteroceptiveSignals, NullPublisher, SignalPublisher};

// ── HomeostaticMonitor (E3.2) ─────────────────────────────────────────────────

/// Tracks interoceptive telemetry for lifecycle self-regulation.
///
/// Originally from E3.2; extended in E5.7 to serve as a compute-pressure and
/// thermal-load proxy via [`HomeostaticMonitor::compute_systemic_stress_index`].
///
/// Available in `no_std` builds (uses `alloc::collections::VecDeque`).
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

// ── InteroceptiveSensorBundle (std only) ──────────────────────────────────────

/// Bundle of all interoceptive sensors (E5.7, S5.7.1–S5.7.3).
///
/// **std only** — depends on `FinancialBudgetSensor` (HashMap) and
/// `PowerSensor` (std::fs).  In `no_std` builds, construct
/// [`InteroceptiveSignals`] directly using [`InteroceptiveSignals::neutral`].
#[cfg(feature = "std")]
pub struct InteroceptiveSensorBundle {
    /// Financial budget sensor (S5.7.2).
    pub financial: FinancialBudgetSensor,
    /// Power state sensor (S5.7.3).
    pub power: PowerSensor,
    /// Attention / user-presence sensor (S5.7.3).
    pub attention: AttentionSensor,
}

#[cfg(feature = "std")]
impl InteroceptiveSensorBundle {
    /// Creates a bundle with default sensor configuration.
    pub fn with_defaults() -> Self {
        Self {
            financial: FinancialBudgetSensor::with_defaults(),
            power: PowerSensor::disabled(),
            attention: AttentionSensor::disabled(),
        }
    }

    /// Samples all sensors and derives the [`InteroceptiveSignals`] snapshot.
    pub fn sample(
        &self,
        monitor: &HomeostaticMonitor,
        active_tokens: u32,
        max_context: u32,
        now_ns: u64,
    ) -> InteroceptiveSignals {
        let latency_stress = if monitor.beta > 0.0 {
            (monitor.compute_systemic_stress_index(0, 1) / monitor.beta - 1.0).clamp(0.0, 1.0)
        } else {
            0.0
        };

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

    // ── HomeostaticMonitor (no_std + std) ────────────────────────────────────

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

    // ── InteroceptiveSensorBundle (std only) ─────────────────────────────────

    #[cfg(feature = "std")]
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

    #[cfg(feature = "std")]
    #[test]
    fn memory_pressure_derived_from_active_tokens() {
        let monitor = HomeostaticMonitor::new(1.0, 0.5, 8);
        let bundle = InteroceptiveSensorBundle::with_defaults();
        let signals = bundle.sample(&monitor, 2048, 4096, 0);
        assert!(
            (signals.memory_pressure - 0.5).abs() < 1e-5,
            "expected memory_pressure=0.5, got {}",
            signals.memory_pressure
        );
    }

    #[cfg(feature = "std")]
    #[test]
    fn zero_max_context_does_not_panic_and_gives_zero_memory_pressure() {
        let monitor = HomeostaticMonitor::new(1.0, 0.5, 8);
        let bundle = InteroceptiveSensorBundle::with_defaults();
        let signals = bundle.sample(&monitor, 100, 0, 0);
        assert_eq!(signals.memory_pressure, 0.0);
    }

    #[cfg(feature = "std")]
    #[test]
    fn financial_budget_sensor_integration_full_budget() {
        let monitor = HomeostaticMonitor::new(1.0, 0.5, 8);
        let bundle = InteroceptiveSensorBundle::with_defaults();
        let signals = bundle.sample(&monitor, 0, 4096, 0);
        assert_eq!(
            signals.financial_budget, 1.0,
            "empty cost table means no spend deducted"
        );
    }
}
