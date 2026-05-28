//! In-process broadcast hub for operator events.
//!
//! The hub is the single fan-out point inside an agent process: every producer
//! (the audit tailer, the interoception sensor bundle, direct callers) pushes
//! [`OperatorEvent`]s in, and every connected operator — SSE browser, TUI,
//! serial bridge — gets a copy out.
//!
//! It deliberately uses only `std` primitives (`mpsc` + `Mutex`) so the
//! `console` crate adds no third-party dependencies to the workspace.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Mutex;
use std::time::Instant;

use console_proto::OperatorEvent;
use interoception::{InteroceptiveSignals, SignalPublisher};

/// How many recent events a freshly-connected client is replayed.
const REPLAY_CAPACITY: usize = 256;

struct Subscriber {
    id: u64,
    tx: Sender<OperatorEvent>,
}

#[derive(Default)]
struct HubInner {
    subscribers: Vec<Subscriber>,
    /// Rolling buffer of recent feed events (tasks, messages, gate, audit)
    /// replayed to new clients so a dashboard opened mid-session has context.
    recent: VecDeque<OperatorEvent>,
    /// The most recent `State` snapshot — replayed first so a new client paints
    /// the current lifecycle immediately rather than waiting for the next change.
    last_state: Option<OperatorEvent>,
    /// The most recent `Vitals` snapshot, replayed for the same reason.
    last_vitals: Option<OperatorEvent>,
    next_id: u64,
}

/// A subscription handle returned by [`ConsoleHub::subscribe`].
pub struct Subscription {
    /// Snapshot events (current vitals + state + recent feed) to render before
    /// the live stream begins.
    pub snapshot: Vec<OperatorEvent>,
    /// The live event stream.
    pub rx: Receiver<OperatorEvent>,
    id: u64,
}

/// Fan-out hub for [`OperatorEvent`]s. Cheap to clone via `Arc`.
pub struct ConsoleHub {
    inner: Mutex<HubInner>,
    start: Instant,
}

impl Default for ConsoleHub {
    fn default() -> Self {
        Self::new()
    }
}

impl ConsoleHub {
    /// Create an empty hub.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HubInner::default()),
            start: Instant::now(),
        }
    }

    /// Seconds since the hub (≈ the agent) started.
    pub fn uptime_secs(&self) -> u64 {
        self.start.elapsed().as_secs()
    }

    /// Publish an event to every live subscriber and the replay buffer.
    ///
    /// Dead subscribers (receiver dropped) are reaped lazily here.
    pub fn publish(&self, event: OperatorEvent) {
        let mut inner = self.inner.lock().expect("hub poisoned");

        // Keep the latest snapshot of the two "current value" event kinds.
        match &event {
            OperatorEvent::State { .. } => inner.last_state = Some(event.clone()),
            OperatorEvent::Vitals { .. } => inner.last_vitals = Some(event.clone()),
            OperatorEvent::Heartbeat { .. } => {} // not worth replaying
            _ => {
                inner.recent.push_back(event.clone());
                while inner.recent.len() > REPLAY_CAPACITY {
                    inner.recent.pop_front();
                }
            }
        }

        // Fan out, dropping any subscriber whose receiver has hung up.
        inner
            .subscribers
            .retain(|s| s.tx.send(event.clone()).is_ok());
    }

    /// Register a new subscriber, returning the replay snapshot plus a live
    /// receiver. Call [`ConsoleHub::unsubscribe`] when the client disconnects
    /// (or simply drop the receiver — dead subscribers are reaped on publish).
    pub fn subscribe(&self) -> Subscription {
        let (tx, rx) = mpsc::channel();
        let mut inner = self.inner.lock().expect("hub poisoned");
        let id = inner.next_id;
        inner.next_id += 1;

        let mut snapshot = Vec::new();
        if let Some(v) = &inner.last_vitals {
            snapshot.push(v.clone());
        }
        if let Some(s) = &inner.last_state {
            snapshot.push(s.clone());
        }
        snapshot.extend(inner.recent.iter().cloned());

        inner.subscribers.push(Subscriber { id, tx });
        Subscription { snapshot, rx, id }
    }

    /// Explicitly remove a subscriber by its [`Subscription::id`].
    pub fn unsubscribe(&self, id: u64) {
        let mut inner = self.inner.lock().expect("hub poisoned");
        inner.subscribers.retain(|s| s.id != id);
    }

    /// Current subscriber count (test/diagnostic aid).
    pub fn subscriber_count(&self) -> usize {
        self.inner.lock().expect("hub poisoned").subscribers.len()
    }
}

impl Subscription {
    /// This subscription's hub id.
    pub fn id(&self) -> u64 {
        self.id
    }
}

/// The hub is a [`SignalPublisher`]: wiring it into an
/// `InteroceptiveSensorBundle` streams the 1 Hz vital signs straight to every
/// connected operator. This is the documented injection contract from
/// `interoception::signals` (S5.7.1).
impl SignalPublisher for ConsoleHub {
    fn publish(&self, signals: &InteroceptiveSignals) {
        ConsoleHub::publish(
            self,
            OperatorEvent::Vitals {
                thermal_load: signals.thermal_load,
                compute_pressure: signals.compute_pressure,
                memory_pressure: signals.memory_pressure,
                power_budget: signals.power_budget,
                financial_budget: signals.financial_budget,
                attention_demand: signals.attention_demand,
                aggregate_stress: signals.aggregate_stress(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vitals(stress: f32) -> OperatorEvent {
        OperatorEvent::Vitals {
            thermal_load: stress,
            compute_pressure: 0.0,
            memory_pressure: 0.0,
            power_budget: 1.0,
            financial_budget: 1.0,
            attention_demand: 0.0,
            aggregate_stress: stress,
        }
    }

    #[test]
    fn subscriber_receives_live_events() {
        let hub = ConsoleHub::new();
        let sub = hub.subscribe();
        hub.publish(OperatorEvent::Heartbeat { uptime_secs: 1 });
        assert!(matches!(
            sub.rx.recv().unwrap(),
            OperatorEvent::Heartbeat { .. }
        ));
    }

    #[test]
    fn new_subscriber_gets_snapshot_of_last_state_and_vitals() {
        let hub = ConsoleHub::new();
        hub.publish(vitals(0.3));
        hub.publish(OperatorEvent::State {
            lifecycle: "Awake".into(),
            sleep_phase: None,
            agenda_depth: 2,
        });
        hub.publish(OperatorEvent::AgentMessage {
            task_id: 1,
            tokens: 10,
            text: "hi".into(),
        });

        let sub = hub.subscribe();
        // Snapshot order: vitals, state, then recent feed.
        assert!(matches!(sub.snapshot[0], OperatorEvent::Vitals { .. }));
        assert!(matches!(sub.snapshot[1], OperatorEvent::State { .. }));
        assert!(matches!(
            sub.snapshot[2],
            OperatorEvent::AgentMessage { .. }
        ));
    }

    #[test]
    fn dropped_subscriber_is_reaped_on_publish() {
        let hub = ConsoleHub::new();
        let sub = hub.subscribe();
        assert_eq!(hub.subscriber_count(), 1);
        drop(sub);
        hub.publish(OperatorEvent::Heartbeat { uptime_secs: 1 });
        assert_eq!(hub.subscriber_count(), 0);
    }

    #[test]
    fn signal_publisher_emits_vitals() {
        let hub = ConsoleHub::new();
        let sub = hub.subscribe();
        SignalPublisher::publish(&hub, &InteroceptiveSignals::maximum_stress());
        match sub.rx.recv().unwrap() {
            OperatorEvent::Vitals {
                aggregate_stress, ..
            } => assert!((aggregate_stress - 1.0).abs() < 1e-5),
            other => panic!("expected vitals, got {other:?}"),
        }
    }
}
