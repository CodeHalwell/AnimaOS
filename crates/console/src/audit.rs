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
        "TaskStarted" => OperatorEvent::TaskStarted {
            task_id: u64f("task_id"),
            prompt: s("prompt"),
        },
        "TaskCompleted" => OperatorEvent::AgentMessage {
            task_id: u64f("task_id"),
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
        loop {
            match std::fs::File::open(&self.path) {
                Ok(file) => {
                    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
                    if len < offset {
                        // File was truncated or rotated — start over.
                        offset = 0;
                    }
                    if len > offset {
                        offset = self.drain_from(file, offset, &mut last_vitals);
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
                    if let Some(event) = event_from_audit_line(line.trim_end()) {
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
                        self.hub.publish(event);
                    }
                }
                Err(_) => break,
            }
        }
        consumed
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

        let ev = sub.rx.recv_timeout(Duration::from_secs(2)).expect("event");
        assert!(matches!(ev, OperatorEvent::TaskStarted { task_id: 1, .. }));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
