#![forbid(unsafe_code)]

use std::collections::VecDeque;

/// Tracks interoceptive telemetry for lifecycle self-regulation.
#[derive(Debug, Clone)]
pub struct HomeostaticMonitor {
    /// Rolling TTFT samples.
    pub rolling_ttft: VecDeque<f32>,
    /// Baseline TTFT value.
    pub baseline_ttft: f32,
    /// Balance parameter between latency and token pressure.
    pub beta: f32,
}

impl HomeostaticMonitor {
    /// Computes the composite systemic stress index.
    pub fn compute_systemic_stress_index(&self, active_tokens: u32, max_context: u32) -> f32 {
        if self.rolling_ttft.is_empty() {
            return 0.0;
        }

        let avg_ttft = self.rolling_ttft.iter().sum::<f32>() / self.rolling_ttft.len() as f32;
        let latency_ratio = if self.baseline_ttft > 0.0 {
            avg_ttft / self.baseline_ttft
        } else {
            1.0
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
    fn stress_index_is_zero_without_ttft_samples() {
        let monitor = HomeostaticMonitor {
            rolling_ttft: VecDeque::new(),
            baseline_ttft: 1.0,
            beta: 0.5,
        };

        assert_eq!(monitor.compute_systemic_stress_index(100, 1000), 0.0);
    }

    #[test]
    fn stress_index_combines_latency_and_memory_pressure() {
        let monitor = HomeostaticMonitor {
            rolling_ttft: VecDeque::from(vec![2.0, 4.0]),
            baseline_ttft: 2.0,
            beta: 0.25,
        };

        let index = monitor.compute_systemic_stress_index(100, 200);

        assert!((index - 0.75).abs() < f32::EPSILON);
    }
}
