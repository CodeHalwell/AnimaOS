//! Interoceptive signal publisher that routes 1 Hz snapshots into the audit log.
//!
//! [`AuditSignalPublisher`] implements [`interoception::SignalPublisher`] so
//! the sensor bundle can be wired into vita without introducing a direct
//! dependency between the `interoception` crate and `AuditLog`.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use interoception::{InteroceptiveSignals, SignalPublisher};

use crate::audit::{AuditEntry, AuditLog};

/// A [`SignalPublisher`] that appends each snapshot to a shared [`AuditLog`].
///
/// The log is shared via `Arc<Mutex<AuditLog>>` so the publisher can be
/// passed to the sensor bundle while the lifecycle manager retains its own
/// handle (and the somatic loop continues to hold a mutable reference
/// through the manager).
pub struct AuditSignalPublisher {
    agent_id: String,
    log: Arc<Mutex<AuditLog>>,
}

impl AuditSignalPublisher {
    /// Creates a publisher that writes to the given shared log.
    pub fn new(agent_id: impl Into<String>, log: Arc<Mutex<AuditLog>>) -> Self {
        Self {
            agent_id: agent_id.into(),
            log,
        }
    }
}

impl SignalPublisher for AuditSignalPublisher {
    fn publish(&self, signals: &InteroceptiveSignals) {
        let tick_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

        let entry = AuditEntry::InteroceptiveSnapshot {
            agent_id: self.agent_id.clone(),
            tick_ns,
            thermal_load: signals.thermal_load,
            compute_pressure: signals.compute_pressure,
            memory_pressure: signals.memory_pressure,
            power_budget: signals.power_budget,
            financial_budget: signals.financial_budget,
            attention_demand: signals.attention_demand,
            aggregate_stress: signals.aggregate_stress(),
        };

        if let Ok(mut log) = self.log.lock() {
            log.push(entry);
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AuditEntry;

    #[test]
    fn publish_appends_interoceptive_snapshot_to_shared_log() {
        let log = Arc::new(Mutex::new(AuditLog::new()));
        let publisher = AuditSignalPublisher::new("test-agent", Arc::clone(&log));

        let signals = InteroceptiveSignals {
            thermal_load: 0.3,
            compute_pressure: 0.2,
            memory_pressure: 0.5,
            power_budget: 0.8,
            financial_budget: 0.9,
            attention_demand: 0.4,
        };
        publisher.publish(&signals);

        let log_guard = log.lock().unwrap();
        assert_eq!(log_guard.len(), 1);
        match &log_guard.entries()[0] {
            AuditEntry::InteroceptiveSnapshot {
                agent_id,
                thermal_load,
                memory_pressure,
                aggregate_stress,
                ..
            } => {
                assert_eq!(agent_id, "test-agent");
                assert!((thermal_load - 0.3).abs() < 1e-6);
                assert!((memory_pressure - 0.5).abs() < 1e-6);
                assert!(*aggregate_stress >= 0.0 && *aggregate_stress <= 1.0);
            }
            other => panic!("unexpected entry: {other:?}"),
        }
    }

    #[test]
    fn publish_twice_appends_two_entries() {
        let log = Arc::new(Mutex::new(AuditLog::new()));
        let publisher = AuditSignalPublisher::new("agent-x", Arc::clone(&log));
        let signals = InteroceptiveSignals::neutral();

        publisher.publish(&signals);
        publisher.publish(&signals);

        assert_eq!(log.lock().unwrap().len(), 2);
    }
}
