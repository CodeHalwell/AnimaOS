//! Bridge from the durable audit log to the operator event stream.
//!
//! `vita`'s `AuditLog` already persists every lifecycle event as newline-
//! delimited JSON (`$ANIMA_AUDIT_DIR/<agent_id>.jsonl`, EX.2). Rather than
//! reach into `vita`'s internals — or take a dependency on it — the console
//! *tails that file* and translates each `AuditEntry` into an
//! [`OperatorEvent`]. This keeps the console fully decoupled: it observes the
//! same durable record an operator would `tail -f`, and works against any agent
//! process (the `serve` driver, the two-agent demo, the container `hosted`
//! service) without modification.
//!
//! The mapping is intentionally generic. `AuditEntry` serialises as an
//! externally-tagged enum (`{"VariantName": { …fields }}`), so we switch on the
//! single top-level key and pull the fields out of a [`serde_json::Value`]. Any
//! variant without a richer dedicated [`OperatorEvent`] falls through to a
//! one-line [`OperatorEvent::Audit`].

use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use console_proto::OperatorEvent;
use serde_json::Value;

use crate::hub::ConsoleHub;

/// Translate one audit-log JSON line into an [`OperatorEvent`].
///
/// Returns `None` for lines that aren't a recognised single-variant object
/// (which should not occur for well-formed audit logs, but we stay defensive).
pub fn event_from_audit_line(line: &str) -> Option<OperatorEvent> {
    let value: Value = serde_json::from_str(line).ok()?;
    event_from_audit_value(&value)
}

/// Translate a parsed audit entry value into an [`OperatorEvent`].
pub fn event_from_audit_value(value: &Value) -> Option<OperatorEvent> {
    let obj = value.as_object()?;
    // Externally-tagged enum: exactly one key, the variant name.
    let (variant, fields) = obj.iter().next()?;

    let s = |k: &str| {
        fields
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let u64f = |k: &str| fields.get(k).and_then(Value::as_u64).unwrap_or(0);
    let f32f = |k: &str| fields.get(k).and_then(Value::as_f64).unwrap_or(0.0) as f32;
    let boolf = |k: &str| fields.get(k).and_then(Value::as_bool).unwrap_or(false);
    let opt_s = |k: &str| {
        fields
            .get(k)
            .and_then(Value::as_str)
            .map(std::string::ToString::to_string)
    };

    let event = match variant.as_str() {
        // `message_id` is filled in by `CorrelationTracker`, which has the
        // cross-line state this per-line mapping deliberately lacks.
        "TaskStarted" => OperatorEvent::TaskStarted {
            task_id: u64f("task_id"),
            message_id: None,
            prompt: s("prompt"),
        },
        "TaskCompleted" => OperatorEvent::AgentMessage {
            task_id: u64f("task_id"),
            message_id: None,
            tokens: fields
                .get("tokens_emitted")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32,
            text: s("response"),
        },
        "TaskFailed" => OperatorEvent::Audit {
            kind: "TaskFailed".into(),
            detail: format!("task {} failed: {}", u64f("task_id"), s("error")),
        },
        "GateDecision" => OperatorEvent::Gate {
            message_id: None,
            invoke: boolf("invoke"),
            cost_class: opt_s("cost_class"),
            value_score: f32f("value_score"),
            threshold: f32f("threshold_applied"),
            override_active: boolf("override_active"),
            reasoning: s("reasoning"),
        },
        "InteroceptiveSnapshot" => OperatorEvent::Vitals {
            thermal_load: f32f("thermal_load"),
            compute_pressure: f32f("compute_pressure"),
            memory_pressure: f32f("memory_pressure"),
            power_budget: f32f("power_budget"),
            financial_budget: f32f("financial_budget"),
            attention_demand: f32f("attention_demand"),
            aggregate_stress: f32f("aggregate_stress"),
        },
        // Lifecycle transitions become coarse State events so a pure
        // audit-tail attachment still paints the state panel. The `serve`
        // driver additionally publishes precise State events (with real agenda
        // depth) straight to the hub.
        "SleepEntered" => OperatorEvent::State {
            lifecycle: "Sleep".into(),
            sleep_phase: None,
            agenda_depth: 0,
        },
        "WakeEntered" => OperatorEvent::State {
            lifecycle: "Awake".into(),
            sleep_phase: None,
            agenda_depth: 0,
        },
        "SleepPhaseStarted" => OperatorEvent::State {
            lifecycle: "Sleep".into(),
            sleep_phase: Some(s("phase")),
            agenda_depth: 0,
        },
        "SleepPhaseCompleted" => OperatorEvent::Audit {
            kind: "SleepPhaseCompleted".into(),
            detail: format!(
                "phase {} {}",
                s("phase"),
                if boolf("success") { "ok" } else { "FAILED" }
            ),
        },
        "MemoryPressureEvent" => OperatorEvent::Audit {
            kind: "MemoryPressureEvent".into(),
            detail: format!(
                "{} ({}/{} tokens)",
                s("level"),
                u64f("active_tokens"),
                u64f("max_context")
            ),
        },
        // Security-relevant — surfaced prominently in the feed.
        "DefenceVeto" => OperatorEvent::Audit {
            kind: "DefenceVeto".into(),
            detail: format!(
                "{} blocked {}: {}",
                s("detector"),
                s("action_blocked"),
                s("reason")
            ),
        },
        "AttentionDemandEscalated" => OperatorEvent::Audit {
            kind: "AttentionDemandEscalated".into(),
            detail: format!(
                "{} vetoes in window — operator attention requested",
                u64f("veto_count")
            ),
        },
        "CortexFault" => OperatorEvent::Audit {
            kind: "CortexFault".into(),
            detail: format!("task {}: {}", s("task_id"), s("error")),
        },
        "IdentityUpdated" => OperatorEvent::Audit {
            kind: "IdentityUpdated".into(),
            detail: format!("{} = {}", s("key"), s("new_value")),
        },
        // Everything else: a compact generic audit line so nothing is silently
        // dropped from the operator's view.
        other => OperatorEvent::Audit {
            kind: other.to_string(),
            detail: fields.as_object().map(compact_fields).unwrap_or_default(),
        },
    };
    Some(event)
}

/// Render a fields object as a compact `k=v, k=v` summary for generic audit
/// entries, skipping the noisy `agent_id`.
fn compact_fields(fields: &serde_json::Map<String, Value>) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (k, v) in fields {
        if k == "agent_id" {
            continue;
        }
        let rendered = match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        parts.push(format!("{k}={rendered}"));
    }
    parts.join(", ")
}

// ── Operator-message correlation (E33 S33.2) ─────────────────────────────────

/// Upper bound on the in-flight `task_id → message_id` links the tailer holds.
///
/// A link is dropped as soon as its task settles, so this is only approached
/// when tasks are admitted much faster than they complete.
const MAX_TRACKED_LINKS: usize = 256;

/// Carries operator-message correlation across audit lines.
///
/// `vita` records the link between an operator message and the task it caused
/// as its own [`vita::AuditEntry::OperatorMessageLinked`] entry, written
/// *before* anything else about that task.  This tracker is the reader half:
/// it remembers each link and stamps the `message_id` onto the gate decision,
/// the task start and the eventual reply, so a console can show the status of
/// one specific message instead of inferring it from arrival order.
///
/// State lives here rather than in the wire protocol because the tailer
/// re-reads the audit file from offset 0 after a restart, which replays the
/// link entries and rebuilds the map for free.
#[derive(Debug, Default)]
pub struct CorrelationTracker {
    links: std::collections::BTreeMap<u64, String>,
}

impl CorrelationTracker {
    /// A tracker with no links yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of links currently held (test/diagnostic aid).
    pub fn tracked(&self) -> usize {
        self.links.len()
    }

    /// Translate one audit JSONL line, applying and updating correlation.
    ///
    /// Returns `None` both for unrecognised lines and for the link entries
    /// themselves — they are plumbing, not something an operator should see in
    /// the telemetry feed.
    pub fn translate_line(&mut self, line: &str) -> Option<OperatorEvent> {
        let value: Value = serde_json::from_str(line).ok()?;
        self.translate(&value)
    }

    /// As [`CorrelationTracker::translate_line`], for an already-parsed entry.
    pub fn translate(&mut self, value: &Value) -> Option<OperatorEvent> {
        let obj = value.as_object()?;
        let (variant, fields) = obj.iter().next()?;

        if variant == "OperatorMessageLinked" {
            let task_id = fields.get("task_id").and_then(Value::as_u64)?;
            let message_id = fields.get("message_id").and_then(Value::as_str)?;
            if self.links.len() >= MAX_TRACKED_LINKS {
                // Task ids are monotonic, so the lowest key is the stalest.
                if let Some(oldest) = self.links.keys().next().copied() {
                    self.links.remove(&oldest);
                }
            }
            self.links.insert(task_id, message_id.to_string());
            return None;
        }

        // A failure settles the task as surely as a completion does, but maps
        // to a generic `Audit` event with no task_id field to key off.
        let failed_task = (variant == "TaskFailed")
            .then(|| fields.get("task_id").and_then(Value::as_u64))
            .flatten();

        let mut event = event_from_audit_value(value)?;
        match &mut event {
            OperatorEvent::TaskStarted {
                task_id,
                message_id,
                ..
            } => *message_id = self.links.get(task_id).cloned(),
            OperatorEvent::AgentMessage {
                task_id,
                message_id,
                ..
            } => {
                // The reply settles the task: attach and release in one step.
                *message_id = self.links.remove(task_id);
            }
            OperatorEvent::Gate {
                message_id, invoke, ..
            } => {
                // Sensory gate decisions carry `event_id = "sensory-<task_id>"`.
                let task_id = fields
                    .get("event_id")
                    .and_then(Value::as_str)
                    .and_then(|e| e.strip_prefix("sensory-"))
                    .and_then(|n| n.parse::<u64>().ok());
                *message_id = task_id.and_then(|id| self.links.get(&id).cloned());
                if !*invoke {
                    // Blocked: no task will follow, so the link is spent.
                    if let Some(id) = task_id {
                        self.links.remove(&id);
                    }
                }
            }
            _ => {}
        }
        if let Some(id) = failed_task {
            self.links.remove(&id);
        }
        Some(event)
    }
}

/// Follows an audit JSONL file, publishing each new entry to the hub as an
/// [`OperatorEvent`]. Blocks; intended to run on its own thread.
///
/// Robust to the file not existing yet (it is created on the agent's first
/// audit write) and to truncation/rotation (offset resets when the file shrinks).
pub struct AuditTailer {
    path: PathBuf,
    hub: Arc<ConsoleHub>,
    poll: Duration,
    min_vitals_interval: Duration,
}

impl AuditTailer {
    /// Create a tailer for `path` feeding `hub`, polling every 200 ms and
    /// down-sampling `Vitals` to at most one per second.
    pub fn new(path: impl Into<PathBuf>, hub: Arc<ConsoleHub>) -> Self {
        Self {
            path: path.into(),
            hub,
            poll: Duration::from_millis(200),
            min_vitals_interval: Duration::from_secs(1),
        }
    }

    /// Set the poll interval (mainly for tests).
    pub fn with_poll(mut self, poll: Duration) -> Self {
        self.poll = poll;
        self
    }

    /// Set the minimum spacing between published `Vitals` events. `vita` writes
    /// an `InteroceptiveSnapshot` on *every* somatic-loop iteration (far faster
    /// than the nominal 1 Hz); this down-samples them so the operator stream
    /// matches the documented 1 Hz vital-sign cadence. Other event kinds are
    /// never throttled.
    pub fn with_vitals_interval(mut self, interval: Duration) -> Self {
        self.min_vitals_interval = interval;
        self
    }

    /// Spawn the tailer on a background thread and return its handle.
    pub fn spawn(self) -> std::thread::JoinHandle<()> {
        std::thread::Builder::new()
            .name("anima-audit-tailer".into())
            .spawn(move || self.run())
            .expect("spawn audit tailer")
    }

    /// Run the follow loop forever.
    pub fn run(&self) {
        let mut offset: u64 = 0;
        let mut last_vitals: Option<Instant> = None;
        // Rebuilt from scratch whenever the file is re-read from offset 0, so a
        // restart recovers every in-flight correlation (E33 S33.2).
        let mut links = CorrelationTracker::new();
        loop {
            match std::fs::File::open(&self.path) {
                Ok(file) => {
                    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
                    if len < offset {
                        // File was truncated or rotated — start over, and drop
                        // correlations that referred to the vanished lines.
                        offset = 0;
                        links = CorrelationTracker::new();
                    }
                    if len > offset {
                        offset = self.drain_from(file, offset, &mut last_vitals, &mut links);
                    }
                }
                Err(_) => {
                    // Not created yet — wait and retry.
                }
            }
            std::thread::sleep(self.poll);
        }
    }

    /// Read complete lines starting at `offset`, publish them, and return the
    /// new offset (end of the last complete line consumed). `Vitals` events are
    /// down-sampled to [`AuditTailer::min_vitals_interval`].
    fn drain_from(
        &self,
        file: std::fs::File,
        offset: u64,
        last_vitals: &mut Option<Instant>,
        links: &mut CorrelationTracker,
    ) -> u64 {
        let mut reader = BufReader::new(file);
        if reader.seek(SeekFrom::Start(offset)).is_err() {
            return offset;
        }
        let mut consumed = offset;
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break, // EOF
                Ok(n) => {
                    // Only treat a line as complete if it ended in '\n';
                    // a partial trailing line is left for the next poll.
                    if !line.ends_with('\n') {
                        break;
                    }
                    consumed += n as u64;
                    let trimmed = line.trim_end();
                    // Update the Prometheus metric registry from every line
                    // before the operator-event throttling (E21).
                    self.hub.update_metrics_from_json(trimmed);
                    if let Some(event) = links.translate_line(trimmed) {
                        if matches!(event, OperatorEvent::Vitals { .. }) {
                            let now = Instant::now();
                            let too_soon = last_vitals
                                .map(|t| now.duration_since(t) < self.min_vitals_interval)
                                .unwrap_or(false);
                            if too_soon {
                                continue;
                            }
                            *last_vitals = Some(now);
                        }
                        // The line's end offset is the event's stable sequence
                        // number — see `ConsoleHub::publish_at`.
                        self.hub.publish_at(consumed, event);
                    }
                }
                Err(_) => break,
            }
        }
        consumed
    }
}

#[cfg(test)]
mod correlation_tests {
    use super::*;

    fn link(task_id: u64, message_id: &str) -> String {
        format!(
            r#"{{"OperatorMessageLinked":{{"agent_id":"a","task_id":{task_id},"message_id":"{message_id}"}}}}"#
        )
    }

    #[test]
    fn link_entries_are_plumbing_and_never_reach_the_operator_feed() {
        let mut t = CorrelationTracker::new();
        assert!(t.translate_line(&link(9, "op-1")).is_none());
        assert_eq!(t.tracked(), 1);
    }

    #[test]
    fn a_message_is_followed_from_gate_through_task_to_reply() {
        let mut t = CorrelationTracker::new();
        t.translate_line(&link(42, "op-7"));

        let gate = t
            .translate_line(
                r#"{"GateDecision":{"agent_id":"a","event_id":"sensory-42","invoke":true,"cost_class":"Frontier","urgency":1.0,"novelty":0.5,"user_facing":true,"semantic_class":"OperatorCommand","value_score":1.0,"threshold_applied":0.4,"thermal_load":0.0,"compute_pressure":0.0,"memory_pressure":0.0,"power_budget":1.0,"financial_budget":1.0,"attention_demand":0.0,"reasoning":"forced","override_active":true}}"#,
            )
            .expect("gate event");
        assert!(
            matches!(gate, OperatorEvent::Gate { message_id: Some(ref m), .. } if m == "op-7"),
            "gate lost correlation: {gate:?}"
        );

        let started = t
            .translate_line(
                r#"{"TaskStarted":{"agent_id":"a","task_id":42,"tier":0,"prompt":"hello"}}"#,
            )
            .expect("task event");
        assert!(
            matches!(started, OperatorEvent::TaskStarted { message_id: Some(ref m), .. } if m == "op-7"),
        );

        let reply = t
            .translate_line(
                r#"{"TaskCompleted":{"agent_id":"a","task_id":42,"tokens_emitted":3,"response":"hi"}}"#,
            )
            .expect("reply event");
        assert!(
            matches!(reply, OperatorEvent::AgentMessage { message_id: Some(ref m), .. } if m == "op-7"),
        );
        // The reply settles the task, so the link is released.
        assert_eq!(t.tracked(), 0, "link outlived the task it described");
    }

    #[test]
    fn a_gate_block_releases_the_link_because_no_task_will_follow() {
        let mut t = CorrelationTracker::new();
        t.translate_line(&link(7, "op-2"));
        let gate = t
            .translate_line(
                r#"{"GateDecision":{"agent_id":"a","event_id":"sensory-7","invoke":false,"cost_class":null,"urgency":0.1,"novelty":0.1,"user_facing":false,"semantic_class":"Background","value_score":0.1,"threshold_applied":0.4,"thermal_load":0.0,"compute_pressure":0.0,"memory_pressure":0.0,"power_budget":1.0,"financial_budget":1.0,"attention_demand":0.0,"reasoning":"below threshold","override_active":false}}"#,
            )
            .expect("gate event");
        assert!(
            matches!(gate, OperatorEvent::Gate { message_id: Some(ref m), invoke: false, .. } if m == "op-2"),
        );
        assert_eq!(t.tracked(), 0);
    }

    #[test]
    fn a_failure_releases_the_link_too() {
        let mut t = CorrelationTracker::new();
        t.translate_line(&link(5, "op-3"));
        let failed = t
            .translate_line(r#"{"TaskFailed":{"agent_id":"a","task_id":5,"error":"boom"}}"#)
            .expect("failure event");
        assert!(matches!(failed, OperatorEvent::Audit { ref kind, .. } if kind == "TaskFailed"));
        assert_eq!(t.tracked(), 0);
    }

    #[test]
    fn an_uncorrelated_task_still_produces_an_event_with_no_message_id() {
        // Tasks the agent starts by itself (intentions, sleep work) have no
        // operator message behind them; correlation must stay optional.
        let mut t = CorrelationTracker::new();
        let started = t
            .translate_line(
                r#"{"TaskStarted":{"agent_id":"a","task_id":1,"tier":0,"prompt":"self"}}"#,
            )
            .expect("task event");
        assert!(matches!(
            started,
            OperatorEvent::TaskStarted {
                message_id: None,
                ..
            }
        ));
    }

    #[test]
    fn the_link_map_is_bounded() {
        let mut t = CorrelationTracker::new();
        for i in 0..(MAX_TRACKED_LINKS + 50) {
            t.translate_line(&link(i as u64, &format!("op-{i}")));
        }
        assert!(
            t.tracked() <= MAX_TRACKED_LINKS,
            "unbounded at {} links",
            t.tracked()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_task_completed_to_agent_message() {
        let line = r#"{"TaskCompleted":{"agent_id":"a","task_id":7,"tokens_emitted":42,"response":"hello operator"}}"#;
        match event_from_audit_line(line).unwrap() {
            OperatorEvent::AgentMessage {
                task_id,
                tokens,
                text,
                ..
            } => {
                assert_eq!(task_id, 7);
                assert_eq!(tokens, 42);
                assert_eq!(text, "hello operator");
            }
            other => panic!("expected AgentMessage, got {other:?}"),
        }
    }

    #[test]
    fn maps_gate_decision() {
        let line = r#"{"GateDecision":{"agent_id":"a","event_id":"e1","invoke":true,"cost_class":"Frontier","urgency":0.9,"novelty":0.5,"user_facing":true,"semantic_class":"UserQuery","value_score":0.82,"threshold_applied":0.4,"thermal_load":0.1,"compute_pressure":0.0,"memory_pressure":0.0,"power_budget":1.0,"financial_budget":1.0,"attention_demand":0.7,"reasoning":"value 0.82 >= threshold 0.40","override_active":false}}"#;
        match event_from_audit_line(line).unwrap() {
            OperatorEvent::Gate {
                invoke,
                cost_class,
                value_score,
                threshold,
                ..
            } => {
                assert!(invoke);
                assert_eq!(cost_class.as_deref(), Some("Frontier"));
                assert!((value_score - 0.82).abs() < 1e-6);
                assert!((threshold - 0.4).abs() < 1e-6);
            }
            other => panic!("expected Gate, got {other:?}"),
        }
    }

    #[test]
    fn maps_interoceptive_snapshot_to_vitals() {
        let line = r#"{"InteroceptiveSnapshot":{"agent_id":"a","tick_ns":1,"thermal_load":0.2,"compute_pressure":0.3,"memory_pressure":0.4,"power_budget":0.9,"financial_budget":0.8,"attention_demand":0.5,"aggregate_stress":0.27}}"#;
        match event_from_audit_line(line).unwrap() {
            OperatorEvent::Vitals {
                memory_pressure,
                aggregate_stress,
                ..
            } => {
                assert!((memory_pressure - 0.4).abs() < 1e-6);
                assert!((aggregate_stress - 0.27).abs() < 1e-6);
            }
            other => panic!("expected Vitals, got {other:?}"),
        }
    }

    #[test]
    fn unknown_variant_falls_through_to_generic_audit() {
        let line = r#"{"RouterDecision":{"agent_id":"a","event_id":"e","route_id":"mid-tier","model_selector":"mid-tier","tool_scope_name":"std","tools_available":3,"tools_permitted":2,"memory_scope_identity":true,"memory_scope_l1":true,"memory_scope_l2":true,"memory_scope_l3":false,"max_turns":8,"max_tool_calls":8}}"#;
        match event_from_audit_line(line).unwrap() {
            OperatorEvent::Audit { kind, detail } => {
                assert_eq!(kind, "RouterDecision");
                assert!(detail.contains("route_id=mid-tier"), "detail was: {detail}");
                assert!(!detail.contains("agent_id"), "agent_id should be skipped");
            }
            other => panic!("expected generic Audit, got {other:?}"),
        }
    }

    #[test]
    fn defence_veto_is_surfaced_with_detector_and_reason() {
        let line = r#"{"DefenceVeto":{"agent_id":"a","invocation_id":"i","detector":"PromptInjectionDetector","action_blocked":"shell rm -rf","reason":"injection pattern"}}"#;
        match event_from_audit_line(line).unwrap() {
            OperatorEvent::Audit { kind, detail } => {
                assert_eq!(kind, "DefenceVeto");
                assert!(detail.contains("PromptInjectionDetector"));
                assert!(detail.contains("injection pattern"));
            }
            other => panic!("expected Audit, got {other:?}"),
        }
    }

    #[test]
    fn tailer_publishes_appended_lines() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("anima-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent.jsonl");
        let _ = std::fs::remove_file(&path);

        let hub = Arc::new(ConsoleHub::new());
        let sub = hub.subscribe();
        let tailer = AuditTailer::new(&path, hub.clone()).with_poll(Duration::from_millis(20));
        let _h = tailer.spawn();

        // Write after the tailer has started to exercise the "follow" path.
        std::thread::sleep(Duration::from_millis(50));
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(
            f,
            r#"{{"TaskStarted":{{"agent_id":"a","task_id":1,"tier":0,"prompt":"do the thing"}}}}"#
        )
        .unwrap();
        f.flush().unwrap();

        let (_, ev) = sub.rx.recv_timeout(Duration::from_secs(2)).expect("event");
        assert!(matches!(ev, OperatorEvent::TaskStarted { task_id: 1, .. }));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
