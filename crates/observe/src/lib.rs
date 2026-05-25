#![forbid(unsafe_code)]

//! Interoceptive engine: real-time telemetry for autonomic self-regulation.

use std::collections::VecDeque;

/// Tracks interoceptive telemetry for lifecycle self-regulation.
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
