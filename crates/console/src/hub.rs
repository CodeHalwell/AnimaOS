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
use metrics::MetricRegistry;

/// How many recent events a freshly-connected client is replayed.
const REPLAY_CAPACITY: usize = 256;

struct Subscriber {
    id: u64,
    tx: Sender<(u64, OperatorEvent)>,
}

#[derive(Default)]
struct HubInner {
    subscribers: Vec<Subscriber>,
    /// Rolling buffer of recent feed events (tasks, messages, gate, audit)
    /// replayed to new clients so a dashboard opened mid-session has context.
    /// Each entry carries its publish sequence number so reconnecting clients
    /// (SSE `Last-Event-ID`) can be replayed only what they have not seen.
    recent: VecDeque<(u64, OperatorEvent)>,
    /// The most recent `State` snapshot — replayed first so a new client paints
    /// the current lifecycle immediately rather than waiting for the next change.
    last_state: Option<(u64, OperatorEvent)>,
    /// The most recent `Vitals` snapshot, replayed for the same reason.
    last_vitals: Option<(u64, OperatorEvent)>,
    /// Monotonic event sequence (the SSE `id:` value).
    seq: u64,
    next_id: u64,
}

/// A subscription handle returned by [`ConsoleHub::subscribe`].
pub struct Subscription {
    /// Snapshot events (current vitals + state + recent feed) to render before
    /// the live stream begins, each tagged with its publish sequence number.
    pub snapshot: Vec<(u64, OperatorEvent)>,
    /// The live event stream, each event tagged with its sequence number.
    pub rx: Receiver<(u64, OperatorEvent)>,
    id: u64,
}

/// Fan-out hub for [`OperatorEvent`]s. Cheap to clone via `Arc`.
pub struct ConsoleHub {
    inner: Mutex<HubInner>,
    start: Instant,
    /// Prometheus-compatible metric registry fed by the audit tailer (E21).
    ///
    /// Kept in a separate `Mutex` so metrics reads do not contend with the
    /// high-frequency subscriber fan-out path.
    pub(crate) metrics: Mutex<MetricRegistry>,
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
            metrics: Mutex::new(MetricRegistry::new()),
        }
    }

    /// Update the metric registry from a raw JSONL audit-log line (E21).
    ///
    /// The line is parsed as a [`vita::AuditEntry`]; unrecognised or malformed
    /// lines are silently ignored.  Called by the [`crate::AuditTailer`] for
    /// every complete line it reads.
    pub fn update_metrics_from_json(&self, line: &str) {
        if let Ok(entry) = serde_json::from_str::<vita::AuditEntry>(line) {
            if let Ok(mut reg) = self.metrics.lock() {
                reg.update(&entry);
            }
        }
    }

    /// Render the current metric registry in Prometheus exposition format.
    ///
    /// Returns an empty string if the mutex is poisoned (should never happen).
    pub fn render_metrics(&self) -> String {
        self.metrics.lock().map(|r| r.render()).unwrap_or_default()
    }

    /// Return a concise human-readable metrics summary for the CLI.
    pub fn metrics_summary(&self) -> String {
        self.metrics.lock().map(|r| r.summary()).unwrap_or_default()
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
        let seq = inner.seq;
        Self::publish_locked(&mut inner, seq, event);
    }

    /// Publish an event with a caller-supplied sequence number.
    ///
    /// The audit tailer passes the audit-file byte offset of the line that
    /// produced the event: offsets are strictly increasing, append-only, and
    /// — crucially — identical for the same historical line across server
    /// restarts, so a reconnecting client's `Last-Event-ID` cursor remains
    /// valid against a freshly-started process re-reading the same file.
    /// The internal counter is advanced past `seq` so interleaved direct
    /// `publish` calls (e.g. auth-lockout audits) stay monotonic.
    pub fn publish_at(&self, seq: u64, event: OperatorEvent) {
        let mut inner = self.inner.lock().expect("hub poisoned");
        Self::publish_locked(&mut inner, seq, event);
    }

    /// Publish under an already-held lock — sequence allocation and delivery
    /// happen in one critical section, so concurrent publishers (audit
    /// tailer, HTTP server, serial bridge) can never mint duplicate or
    /// out-of-order SSE ids.
    fn publish_locked(inner: &mut HubInner, seq: u64, event: OperatorEvent) {
        inner.seq = inner.seq.max(seq + 1);

        // Keep the latest snapshot of the two "current value" event kinds.
        match &event {
            OperatorEvent::State { .. } => inner.last_state = Some((seq, event.clone())),
            OperatorEvent::Vitals { .. } => inner.last_vitals = Some((seq, event.clone())),
            OperatorEvent::Heartbeat { .. } => {} // not worth replaying
            _ => {
                inner.recent.push_back((seq, event.clone()));
                while inner.recent.len() > REPLAY_CAPACITY {
                    inner.recent.pop_front();
                }
            }
        }

        // Fan out, dropping any subscriber whose receiver has hung up.
        inner
            .subscribers
            .retain(|s| s.tx.send((seq, event.clone())).is_ok());
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
        // Replay in publish order so a client's `Last-Event-ID` cursor moves
        // monotonically through the snapshot.
        snapshot.sort_by_key(|(seq, _)| *seq);

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
            (_, OperatorEvent::Heartbeat { .. })
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
        // Snapshot replays in publish order: vitals, state, then the feed
        // event — and the sequence numbers must be strictly increasing so a
        // reconnecting client's Last-Event-ID cursor is meaningful.
        assert!(matches!(sub.snapshot[0], (0, OperatorEvent::Vitals { .. })));
        assert!(matches!(sub.snapshot[1], (1, OperatorEvent::State { .. })));
        assert!(matches!(
            sub.snapshot[2],
            (2, OperatorEvent::AgentMessage { .. })
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
            (
                _,
                OperatorEvent::Vitals {
                    aggregate_stress, ..
                },
            ) => assert!((aggregate_stress - 1.0).abs() < 1e-5),
            other => panic!("expected vitals, got {other:?}"),
        }
    }

    // ── E21 metric registry tests ─────────────────────────────────────────────

    #[test]
    fn update_metrics_from_valid_json_increments_counters() {
        let hub = ConsoleHub::new();
        let line = r#"{"SleepEntered":{"agent_id":"a"}}"#;
        hub.update_metrics_from_json(line);
        hub.update_metrics_from_json(line);
        let out = hub.render_metrics();
        assert!(
            out.contains("anima_sleep_cycles_total 2"),
            "sleep counter: {out}"
        );
    }

    #[test]
    fn update_metrics_from_malformed_json_is_silently_ignored() {
        let hub = ConsoleHub::new();
        hub.update_metrics_from_json("this is not json at all");
        hub.update_metrics_from_json("{\"UnknownVariant\":{}}");
        let out = hub.render_metrics();
        assert!(
            out.contains("anima_sleep_cycles_total 0"),
            "counters: {out}"
        );
    }

    #[test]
    fn render_metrics_returns_non_empty_prometheus_text() {
        let hub = ConsoleHub::new();
        let out = hub.render_metrics();
        assert!(
            out.contains("# HELP anima_tasks_total"),
            "prometheus: {out}"
        );
    }

    #[test]
    fn metrics_summary_contains_key_fields() {
        let hub = ConsoleHub::new();
        hub.update_metrics_from_json(
            r#"{"TaskCompleted":{"agent_id":"a","task_id":1,"tokens_emitted":50,"response":"ok"}}"#,
        );
        let s = hub.metrics_summary();
        assert!(s.contains("Completed"), "summary: {s}");
        assert!(s.contains("Tokens emitted"), "summary: {s}");
    }
}
