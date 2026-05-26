// crates/interoception/src/signals.rs
//! Canonical signal contract for E5.7 Interoceptive Modulation (S5.7.1).
//!
//! [`InteroceptiveSignals`] is the single authoritative source of all six
//! normalised homeostatic scalars. Every consumer — Striatal Gate (E5.2),
//! Thalamic Router (E5.7), and any future learned controllers — reads from
//! this struct rather than constructing signal values ad hoc.
//!
//! ## Signal semantics
//!
//! | Field | 0.0 = … | 1.0 = … |
//! |-------|---------|---------|
//! | `thermal_load` | no thermal stress | CPU at throttle limit |
//! | `compute_pressure` | idle | fully saturated |
//! | `memory_pressure` | ample working memory | near-OOM |
//! | `power_budget` | battery exhausted / 0 % | on AC or fully charged |
//! | `financial_budget` | daily API budget exhausted | budget untouched |
//! | `attention_demand` | agent fully in background | user actively engaged |
//!
//! ## Publication contract (S5.7.1)
//!
//! The [`SignalPublisher`] trait is the 1 Hz publication sink. Implementors
//! write the snapshot to an audit log, telemetry stream, or MPSC channel.
//! The sensor layer is deliberately free of vita-crate dependencies, so it
//! receives the sink via dependency injection.

#![forbid(unsafe_code)]

/// The canonical set of six normalised interoceptive scalars (all in `[0, 1]`).
///
/// Constructed by [`crate::InteroceptiveSensorBundle::sample`] and consumed by
/// the gate and router via `vita::HomeostaticSignals::from_interoceptive`.
#[derive(Debug, Clone, PartialEq)]
pub struct InteroceptiveSignals {
    /// CPU/GPU thermal occupancy (`0.0` = cool, `1.0` = at throttle limit).
    pub thermal_load: f32,
    /// Compute-pipeline saturation (`0.0` = idle, `1.0` = saturated).
    pub compute_pressure: f32,
    /// Working-memory (L1/L2) fill fraction (`0.0` = empty, `1.0` = full).
    pub memory_pressure: f32,
    /// Available power budget (`1.0` = wall-power or full battery, `0.0` = flat).
    pub power_budget: f32,
    /// Remaining financial API budget fraction (`1.0` = fully available, `0.0` = exhausted).
    pub financial_budget: f32,
    /// User presence/attention level (`1.0` = full attention, `0.0` = absent/idle).
    pub attention_demand: f32,
}

impl InteroceptiveSignals {
    /// All signals at their lowest-stress / fully-available values.
    ///
    /// Suitable as a default when real sensor data is unavailable.
    pub fn neutral() -> Self {
        Self {
            thermal_load: 0.0,
            compute_pressure: 0.0,
            memory_pressure: 0.0,
            power_budget: 1.0,
            financial_budget: 1.0,
            attention_demand: 0.5,
        }
    }

    /// All signals at their maximum-stress / fully-depleted values.
    ///
    /// Used in the E5.7 stress harness (exit criterion 1) to verify that
    /// gate and router modulation engages correctly at the extremes.
    pub fn maximum_stress() -> Self {
        Self {
            thermal_load: 1.0,
            compute_pressure: 1.0,
            memory_pressure: 1.0,
            power_budget: 0.0,
            financial_budget: 0.0,
            attention_demand: 1.0,
        }
    }

    /// Validate that all fields lie in `[0, 1]`.
    pub fn is_valid(&self) -> bool {
        [
            self.thermal_load,
            self.compute_pressure,
            self.memory_pressure,
            self.power_budget,
            self.financial_budget,
            self.attention_demand,
        ]
        .iter()
        .all(|&v| (0.0..=1.0).contains(&v))
    }

    /// Compute a weighted aggregate stress level in `[0, 1]`.
    ///
    /// Weights mirror the gate's coefficient table (E5.2):
    ///
    /// | Signal | Weight |
    /// |--------|--------|
    /// | `thermal_load` | 30 % |
    /// | `compute_pressure` | 30 % |
    /// | `memory_pressure` | 20 % |
    /// | `financial_budget` (inverted) | 10 % |
    /// | `power_budget` (inverted) | 10 % |
    ///
    /// `attention_demand` is excluded from the aggregate — it lowers the gate
    /// threshold rather than raising the stress level.
    pub fn aggregate_stress(&self) -> f32 {
        let w_thermal = 0.30 * self.thermal_load;
        let w_compute = 0.30 * self.compute_pressure;
        let w_memory = 0.20 * self.memory_pressure;
        let w_finance = 0.10 * (1.0 - self.financial_budget);
        let w_power = 0.10 * (1.0 - self.power_budget);
        (w_thermal + w_compute + w_memory + w_finance + w_power).clamp(0.0, 1.0)
    }

    /// Override a single field and return a new snapshot.
    ///
    /// Useful in the stress harness for sweeping one signal while holding
    /// all others constant.
    pub fn with_thermal_load(mut self, v: f32) -> Self {
        self.thermal_load = v.clamp(0.0, 1.0);
        self
    }
    /// Override `compute_pressure`.
    pub fn with_compute_pressure(mut self, v: f32) -> Self {
        self.compute_pressure = v.clamp(0.0, 1.0);
        self
    }
    /// Override `memory_pressure`.
    pub fn with_memory_pressure(mut self, v: f32) -> Self {
        self.memory_pressure = v.clamp(0.0, 1.0);
        self
    }
    /// Override `power_budget`.
    pub fn with_power_budget(mut self, v: f32) -> Self {
        self.power_budget = v.clamp(0.0, 1.0);
        self
    }
    /// Override `financial_budget`.
    pub fn with_financial_budget(mut self, v: f32) -> Self {
        self.financial_budget = v.clamp(0.0, 1.0);
        self
    }
    /// Override `attention_demand`.
    pub fn with_attention_demand(mut self, v: f32) -> Self {
        self.attention_demand = v.clamp(0.0, 1.0);
        self
    }
}

impl Default for InteroceptiveSignals {
    fn default() -> Self {
        Self::neutral()
    }
}

// ── Signal publication (S5.7.1) ───────────────────────────────────────────────

/// Trait that publishes an [`InteroceptiveSignals`] snapshot at 1 Hz (S5.7.1).
///
/// The sink (audit log, telemetry channel, metrics endpoint) is injected via
/// a concrete implementation so that the sensor layer remains independent of
/// the vita crate.
pub trait SignalPublisher: Send + Sync {
    /// Called once per tick (nominally 1 Hz) with the latest snapshot.
    fn publish(&self, signals: &InteroceptiveSignals);
}

/// A [`SignalPublisher`] backed by an arbitrary closure.
///
/// ```
/// use interoception::{FnPublisher, InteroceptiveSignals, SignalPublisher};
/// let p = FnPublisher(|s: &InteroceptiveSignals| {
///     println!("stress={:.2}", s.aggregate_stress());
/// });
/// p.publish(&InteroceptiveSignals::neutral());
/// ```
pub struct FnPublisher<F: Fn(&InteroceptiveSignals) + Send + Sync>(pub F);

impl<F: Fn(&InteroceptiveSignals) + Send + Sync> SignalPublisher for FnPublisher<F> {
    fn publish(&self, signals: &InteroceptiveSignals) {
        (self.0)(signals)
    }
}

/// A no-op [`SignalPublisher`] for tests where publication side-effects are
/// not under test.
pub struct NullPublisher;

impl SignalPublisher for NullPublisher {
    fn publish(&self, _signals: &InteroceptiveSignals) {}
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_signals_are_all_valid() {
        assert!(InteroceptiveSignals::neutral().is_valid());
    }

    #[test]
    fn maximum_stress_signals_are_all_valid() {
        assert!(InteroceptiveSignals::maximum_stress().is_valid());
    }

    #[test]
    fn neutral_aggregate_stress_is_zero() {
        let s = InteroceptiveSignals::neutral();
        assert_eq!(
            s.aggregate_stress(),
            0.0,
            "neutral has no thermal/compute/memory load and full budgets"
        );
    }

    #[test]
    fn maximum_stress_aggregate_is_one() {
        let s = InteroceptiveSignals::maximum_stress();
        assert!(
            (s.aggregate_stress() - 1.0).abs() < 1e-5,
            "maximum stress should reach 1.0, got {}",
            s.aggregate_stress()
        );
    }

    #[test]
    fn aggregate_stress_thermal_only_contributes_thirty_percent() {
        let s = InteroceptiveSignals {
            thermal_load: 1.0,
            compute_pressure: 0.0,
            memory_pressure: 0.0,
            power_budget: 1.0,
            financial_budget: 1.0,
            attention_demand: 0.0,
        };
        assert!(
            (s.aggregate_stress() - 0.30).abs() < 1e-6,
            "thermal_load=1.0 contributes 30% weight: got {}",
            s.aggregate_stress()
        );
    }

    #[test]
    fn aggregate_stress_financial_pressure_inverted() {
        // Zero financial_budget = fully depleted → maximum financial stress contribution
        let s = InteroceptiveSignals {
            thermal_load: 0.0,
            compute_pressure: 0.0,
            memory_pressure: 0.0,
            power_budget: 1.0,
            financial_budget: 0.0,
            attention_demand: 0.0,
        };
        assert!(
            (s.aggregate_stress() - 0.10).abs() < 1e-6,
            "financial_budget=0 contributes 10% stress: got {}",
            s.aggregate_stress()
        );
    }

    #[test]
    fn with_thermal_load_builder_clamps_to_unit_interval() {
        let s = InteroceptiveSignals::neutral().with_thermal_load(2.0);
        assert_eq!(s.thermal_load, 1.0);
        let s2 = InteroceptiveSignals::neutral().with_thermal_load(-1.0);
        assert_eq!(s2.thermal_load, 0.0);
    }

    #[test]
    fn fn_publisher_invokes_callback_once_per_call() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let count = Arc::new(AtomicUsize::new(0));
        let count_clone = count.clone();
        let publisher = FnPublisher(move |_: &InteroceptiveSignals| {
            count_clone.fetch_add(1, Ordering::SeqCst);
        });
        publisher.publish(&InteroceptiveSignals::neutral());
        publisher.publish(&InteroceptiveSignals::neutral());
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn null_publisher_does_not_panic_and_is_a_no_op() {
        let publisher = NullPublisher;
        publisher.publish(&InteroceptiveSignals::neutral());
        // No assertion needed — the test just verifies no panic.
    }

    #[test]
    fn default_signals_equal_neutral() {
        assert_eq!(
            InteroceptiveSignals::default(),
            InteroceptiveSignals::neutral()
        );
    }
}
