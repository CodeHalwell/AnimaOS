#![forbid(unsafe_code)]
// `no_std` everywhere except under the test harness, which needs std to link.
// The microVM kernel builds this crate with `default-features = false` (no
// serde, no serde_json) so the EFI image stays inside its ≤1 MiB budget.
#![cfg_attr(not(test), no_std)]

//! Operator-console wire protocol — the shared vocabulary spoken between an
//! AnimaOS agent and an external human operator.
//!
//! # The two channels
//!
//! AnimaOS models the human as *a sense*, not a controller (see
//! `docs/01-architecture.md §1.3`). This crate therefore defines two
//! transport-agnostic message families that mirror the body's afferent /
//! efferent split:
//!
//! - [`OperatorInput`] — **afferent** (human → agent). A guidance line that the
//!   agent ingests as a [prioritised sensory packet]; it is *never* a kernel
//!   command. The agent's Striatal Gate decides whether and when to act on it.
//! - [`OperatorEvent`] — **efferent + interoceptive** (agent → human). The
//!   legible stream an operator watches: 1 Hz vitals, lifecycle state, gate
//!   rationale, audit deltas, and the agent's own messages.
//!
//! # One protocol, two transports
//!
//! The same types carry over every surface AnimaOS runs on:
//!
//! - **Container / hosted (std):** length-delimited NDJSON over TCP — one JSON
//!   object per line. The `console` crate serves it as Server-Sent Events plus
//!   a `POST /guidance` ingress; the `json` feature provides the serde helpers.
//! - **microVM (no_std):** the *same* NDJSON, framed onto the COM1 serial line
//!   with an `ANIMA_TLM ` / `ANIMA_IN ` prefix. The kernel needs no
//!   `serde_json` (image-size budget): it uses the dependency-free
//!   [`OperatorEvent::to_ndjson`] writer and the [`parse_input_line`] reader.
//!
//! [prioritised sensory packet]: Priority

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Marker line-prefix for an [`OperatorEvent`] framed onto a shared serial line
/// (microVM COM1). The bytes after the prefix are a single NDJSON object.
pub const TELEMETRY_PREFIX: &str = "ANIMA_TLM ";

/// Marker line-prefix for an [`OperatorInput`] arriving on a shared serial line.
pub const INPUT_PREFIX: &str = "ANIMA_IN ";

// ── Priority ────────────────────────────────────────────────────────────────

/// Urgency tag attached to operator guidance.
///
/// Mirrors `senses::SensoryPriority` but is redefined here so the protocol
/// crate stays dependency-free and `no_std`-clean. The `console` crate maps
/// between the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Priority {
    /// Background / informational.
    Low,
    /// Standard human interaction (the default).
    #[default]
    Normal,
    /// Elevated urgency — a follow-up or clarification.
    High,
    /// Interrupt-level — an operator emergency.
    Critical,
}

impl Priority {
    /// Parse the wire spelling (case-insensitive). Unknown values map to
    /// [`Priority::Normal`] so a typo never silently escalates urgency.
    pub fn parse(s: &str) -> Priority {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" => Priority::Low,
            "high" => Priority::High,
            "critical" => Priority::Critical,
            _ => Priority::Normal,
        }
    }

    /// The canonical wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Priority::Low => "Low",
            Priority::Normal => "Normal",
            Priority::High => "High",
            Priority::Critical => "Critical",
        }
    }
}

// ── Afferent: human → agent ──────────────────────────────────────────────────

/// A single line of operator guidance, ingested by the agent as a sensory
/// packet. It enters the prioritisation queue and is arbitrated by the gate;
/// it cannot preempt the kernel.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct OperatorInput {
    /// Free-form guidance text.
    pub text: String,
    /// Urgency tag; defaults to [`Priority::Normal`] when absent on the wire.
    #[cfg_attr(feature = "serde", serde(default))]
    pub priority: Priority,
    /// When `Some`, requests an *audited* `GateOverride::OperatorForced` with
    /// this reason. This still flows through the audit trail and defence layer;
    /// it is an escalation, not a bypass.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub force: Option<String>,
}

impl OperatorInput {
    /// Construct a normal-priority guidance line.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            priority: Priority::Normal,
            force: None,
        }
    }

    /// Set the urgency tag (builder style).
    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }
}

// ── Efferent: agent → human ──────────────────────────────────────────────────

/// One legible event in the agent → operator stream.
///
/// Serialised as an internally-tagged JSON object (`{"type":"Vitals",…}`) so
/// the browser dashboard and the TUI can switch on a single `type` field. The
/// manual [`OperatorEvent::to_ndjson`] writer emits the identical shape, so the
/// `no_std` kernel and the serde-based clients interoperate byte-for-byte.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "type"))]
pub enum OperatorEvent {
    /// The 1 Hz interoceptive snapshot — the agent's vital signs.
    Vitals {
        /// CPU/GPU thermal occupancy (`0.0` cool … `1.0` throttle limit).
        thermal_load: f32,
        /// Compute-pipeline saturation.
        compute_pressure: f32,
        /// Working-memory fill fraction.
        memory_pressure: f32,
        /// Power budget (`1.0` = wall/full battery).
        power_budget: f32,
        /// Remaining financial API budget (`1.0` = untouched).
        financial_budget: f32,
        /// User presence (`1.0` = actively engaged).
        attention_demand: f32,
        /// Weighted aggregate stress in `[0, 1]`.
        aggregate_stress: f32,
    },
    /// Lifecycle state and agenda depth.
    State {
        /// `"Awake"` or `"Sleep"`.
        lifecycle: String,
        /// Current sleep phase, when sleeping (e.g. `"Dreaming"`).
        sleep_phase: Option<String>,
        /// Number of tasks waiting on the agenda.
        agenda_depth: u32,
    },
    /// A Striatal-Gate decision — *why* the agent did or didn't act.
    Gate {
        /// Whether the gate authorised a cortex invocation.
        invoke: bool,
        /// Selected cost class (`CheapLocal` / `MidTier` / `Frontier`).
        cost_class: Option<String>,
        /// Computed value score in `[0, 1]`.
        value_score: f32,
        /// Applied threshold in `[0, 1]`.
        threshold: f32,
        /// Whether an operator override was active.
        override_active: bool,
        /// Human-readable rationale.
        reasoning: String,
    },
    /// A generic audit-trail delta (memory pressure, sleep phase, identity
    /// update, defence veto, …) that has no richer dedicated variant.
    Audit {
        /// The `AuditEntry` variant name (e.g. `"MemoryPressureEvent"`).
        kind: String,
        /// A one-line human-readable detail.
        detail: String,
    },
    /// The agent began working a task.
    TaskStarted {
        /// Scheduler task id.
        task_id: u64,
        /// The prompt / task description.
        prompt: String,
    },
    /// The agent finished a task — its *message back to the operator*.
    AgentMessage {
        /// Scheduler task id.
        task_id: u64,
        /// Tokens emitted by the backend.
        tokens: u32,
        /// The agent's response text.
        text: String,
    },
    /// A liveness tick so idle connections can distinguish "quiet" from "dead".
    Heartbeat {
        /// Seconds since the agent started, for clock-skew-free uptime display.
        uptime_secs: u64,
    },
}

impl OperatorEvent {
    /// The `type` tag this event serialises with.
    pub fn kind(&self) -> &'static str {
        match self {
            OperatorEvent::Vitals { .. } => "Vitals",
            OperatorEvent::State { .. } => "State",
            OperatorEvent::Gate { .. } => "Gate",
            OperatorEvent::Audit { .. } => "Audit",
            OperatorEvent::TaskStarted { .. } => "TaskStarted",
            OperatorEvent::AgentMessage { .. } => "AgentMessage",
            OperatorEvent::Heartbeat { .. } => "Heartbeat",
        }
    }

    /// Serialise to a single NDJSON line **without** a trailing newline, using
    /// only `core` + `alloc`.
    ///
    /// This is the kernel's telemetry path: it needs no `serde_json` and emits
    /// the exact internally-tagged shape that the serde-derived
    /// `Deserialize` impl accepts, so std clients parse it transparently.
    pub fn to_ndjson(&self) -> String {
        let mut s = String::new();
        // `write!` into a String is infallible; ignore the Result.
        match self {
            OperatorEvent::Vitals {
                thermal_load,
                compute_pressure,
                memory_pressure,
                power_budget,
                financial_budget,
                attention_demand,
                aggregate_stress,
            } => {
                let _ = write!(
                    s,
                    "{{\"type\":\"Vitals\",\"thermal_load\":{thermal_load},\"compute_pressure\":{compute_pressure},\"memory_pressure\":{memory_pressure},\"power_budget\":{power_budget},\"financial_budget\":{financial_budget},\"attention_demand\":{attention_demand},\"aggregate_stress\":{aggregate_stress}}}"
                );
            }
            OperatorEvent::State {
                lifecycle,
                sleep_phase,
                agenda_depth,
            } => {
                let _ = write!(s, "{{\"type\":\"State\",\"lifecycle\":");
                write_json_str(&mut s, lifecycle);
                let _ = write!(s, ",\"sleep_phase\":");
                match sleep_phase {
                    Some(p) => write_json_str(&mut s, p),
                    None => {
                        let _ = write!(s, "null");
                    }
                }
                let _ = write!(s, ",\"agenda_depth\":{agenda_depth}}}");
            }
            OperatorEvent::Gate {
                invoke,
                cost_class,
                value_score,
                threshold,
                override_active,
                reasoning,
            } => {
                let _ = write!(s, "{{\"type\":\"Gate\",\"invoke\":{invoke},\"cost_class\":");
                match cost_class {
                    Some(c) => write_json_str(&mut s, c),
                    None => {
                        let _ = write!(s, "null");
                    }
                }
                let _ = write!(
                    s,
                    ",\"value_score\":{value_score},\"threshold\":{threshold},\"override_active\":{override_active},\"reasoning\":"
                );
                write_json_str(&mut s, reasoning);
                let _ = write!(s, "}}");
            }
            OperatorEvent::Audit { kind, detail } => {
                let _ = write!(s, "{{\"type\":\"Audit\",\"kind\":");
                write_json_str(&mut s, kind);
                let _ = write!(s, ",\"detail\":");
                write_json_str(&mut s, detail);
                let _ = write!(s, "}}");
            }
            OperatorEvent::TaskStarted { task_id, prompt } => {
                let _ = write!(
                    s,
                    "{{\"type\":\"TaskStarted\",\"task_id\":{task_id},\"prompt\":"
                );
                write_json_str(&mut s, prompt);
                let _ = write!(s, "}}");
            }
            OperatorEvent::AgentMessage {
                task_id,
                tokens,
                text,
            } => {
                let _ = write!(
                    s,
                    "{{\"type\":\"AgentMessage\",\"task_id\":{task_id},\"tokens\":{tokens},\"text\":"
                );
                write_json_str(&mut s, text);
                let _ = write!(s, "}}");
            }
            OperatorEvent::Heartbeat { uptime_secs } => {
                let _ = write!(
                    s,
                    "{{\"type\":\"Heartbeat\",\"uptime_secs\":{uptime_secs}}}"
                );
            }
        }
        s
    }
}

/// Write `value` as a JSON string literal (with surrounding quotes), escaping
/// the characters JSON requires. `no_std`, allocation-free beyond the target.
fn write_json_str(out: &mut String, value: &str) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

// ── Dependency-free inbound parsing (kernel RX path) ─────────────────────────

/// Parse an [`OperatorInput`] from a single NDJSON line **without** serde_json.
///
/// Deliberately small and forgiving: it extracts the `"text"` (required),
/// `"priority"` (optional), and `"force"` (optional) fields with a hand-rolled
/// scanner so the microVM kernel can read operator guidance off COM1 without
/// linking a full JSON parser. Returns `None` when no `text` field is present.
///
/// An optional [`INPUT_PREFIX`] is tolerated and stripped.
pub fn parse_input_line(line: &str) -> Option<OperatorInput> {
    let line = line.trim();
    let line = line.strip_prefix(INPUT_PREFIX).unwrap_or(line);

    let text = extract_json_string(line, "text")?;
    let priority = extract_json_string(line, "priority")
        .map(|p| Priority::parse(&p))
        .unwrap_or_default();
    let force = extract_json_string(line, "force");

    Some(OperatorInput {
        text,
        priority,
        force,
    })
}

/// Find `"key":"value"` in `src` and return the unescaped `value`.
///
/// Handles the JSON string escapes the [`write_json_str`] writer produces.
/// Not a general JSON parser — scoped to flat objects of string fields, which
/// is all the inbound kernel path needs.
fn extract_json_string(src: &str, key: &str) -> Option<String> {
    // Build the needle `"key"` and locate it.
    let mut needle = String::with_capacity(key.len() + 2);
    needle.push('"');
    needle.push_str(key);
    needle.push('"');
    let key_pos = src.find(&needle)?;
    let after_key = &src[key_pos + needle.len()..];

    // Skip whitespace and the colon.
    let after_colon = after_key.trim_start();
    let after_colon = after_colon.strip_prefix(':')?.trim_start();

    // Must be a string value.
    let body = after_colon.strip_prefix('"')?;

    // Walk to the closing unescaped quote, decoding escapes.
    let mut out = String::new();
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '/' => out.push('/'),
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'b' => out.push('\u{0008}'),
                'f' => out.push('\u{000C}'),
                'u' => {
                    let hex: String = (&mut chars).take(4).collect();
                    let code = u32::from_str_radix(&hex, 16).ok()?;
                    out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                }
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
    None
}

// ── serde_json convenience (std container surface) ───────────────────────────

/// serde_json-backed (de)serialisation helpers for the std surfaces.
///
/// Enabled by the `json` feature. The `console` crate and `anima-console`
/// client use these for robust parsing of arbitrary inbound lines; the kernel
/// uses the manual writers/readers above instead.
#[cfg(feature = "json")]
pub mod json {
    use super::{OperatorEvent, OperatorInput};
    use alloc::string::String;

    /// Serialise an event to a single NDJSON line (no trailing newline).
    pub fn event_to_line(event: &OperatorEvent) -> String {
        // The serde-derived shape matches `OperatorEvent::to_ndjson`; we prefer
        // serde here so float formatting goes through one well-tested path.
        serde_json::to_string(event).unwrap_or_else(|_| event.to_ndjson())
    }

    /// Parse an event from a single NDJSON line. An optional
    /// [`super::TELEMETRY_PREFIX`] is tolerated and stripped.
    pub fn event_from_line(line: &str) -> Option<OperatorEvent> {
        let line = line.trim();
        let line = line.strip_prefix(super::TELEMETRY_PREFIX).unwrap_or(line);
        serde_json::from_str(line).ok()
    }

    /// Serialise operator guidance to a single NDJSON line (no trailing newline).
    pub fn input_to_line(input: &OperatorInput) -> String {
        serde_json::to_string(input).expect("OperatorInput serialises")
    }

    /// Parse operator guidance from a single NDJSON line via serde_json,
    /// falling back to the dependency-free scanner for partial inputs.
    pub fn input_from_line(line: &str) -> Option<OperatorInput> {
        let line = line.trim();
        let line = line.strip_prefix(super::INPUT_PREFIX).unwrap_or(line);
        serde_json::from_str(line)
            .ok()
            .or_else(|| super::parse_input_line(line))
    }
}

/// The frozen schema version, surfaced so clients can detect protocol drift.
pub const PROTOCOL_VERSION: u32 = 1;

/// Split a byte buffer into complete `\n`-terminated lines, returning the
/// parsed strings and leaving any trailing partial line in `remainder`.
///
/// Shared by every transport reader (TCP, serial) so line framing lives in one
/// place. Carriage returns are trimmed so `\r\n` framing works too.
pub fn drain_lines(buffer: &mut Vec<u8>) -> Vec<String> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut consumed = 0usize;
    for (i, &b) in buffer.iter().enumerate() {
        if b == b'\n' {
            if let Ok(text) = core::str::from_utf8(&buffer[start..i]) {
                let trimmed = text.trim_end_matches('\r');
                if !trimmed.is_empty() {
                    lines.push(String::from(trimmed));
                }
            }
            start = i + 1;
            consumed = start;
        }
    }
    if consumed > 0 {
        buffer.drain(0..consumed);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    #[test]
    fn priority_round_trips_through_wire_spelling() {
        for p in [
            Priority::Low,
            Priority::Normal,
            Priority::High,
            Priority::Critical,
        ] {
            assert_eq!(Priority::parse(p.as_str()), p);
        }
    }

    #[test]
    fn priority_parse_is_case_insensitive_and_defaults_safely() {
        assert_eq!(Priority::parse("CRITICAL"), Priority::Critical);
        assert_eq!(Priority::parse("  high  "), Priority::High);
        // An unknown spelling must never silently escalate.
        assert_eq!(Priority::parse("supercritical"), Priority::Normal);
    }

    #[test]
    fn parse_input_line_extracts_text_priority_and_force() {
        let line =
            r#"{"text":"raise the build timeout","priority":"High","force":"operator on call"}"#;
        let input = parse_input_line(line).expect("parses");
        assert_eq!(input.text, "raise the build timeout");
        assert_eq!(input.priority, Priority::High);
        assert_eq!(input.force.as_deref(), Some("operator on call"));
    }

    #[test]
    fn parse_input_line_tolerates_serial_prefix_and_missing_priority() {
        let line = r#"ANIMA_IN {"text":"hello"}"#;
        let input = parse_input_line(line).expect("parses");
        assert_eq!(input.text, "hello");
        assert_eq!(input.priority, Priority::Normal);
        assert_eq!(input.force, None);
    }

    #[test]
    fn parse_input_line_decodes_escapes() {
        let line = r#"{"text":"line one\nline \"two\""}"#;
        let input = parse_input_line(line).expect("parses");
        assert_eq!(input.text, "line one\nline \"two\"");
    }

    #[test]
    fn parse_input_line_returns_none_without_text() {
        assert!(parse_input_line(r#"{"priority":"Low"}"#).is_none());
    }

    // ── Escape-sequence round-trip suite ────────────────────────────────────
    //
    // The kernel RX path (`extract_json_string`) must decode exactly what the
    // kernel TX path (`write_json_str`) encodes, for every byte an operator
    // might send. These cases exercise the JSON-mandated escapes (U+0000–U+001F,
    // quote, backslash) plus pass-through bytes (DEL, multi-byte UTF-8) that the
    // writer leaves raw.

    /// Adversarial inputs that have historically broken hand-rolled JSON codecs.
    fn escape_torture_strings() -> alloc::vec::Vec<String> {
        let mut cases = vec![
            String::new(),                                 // empty
            "\u{0000}".to_string(),                        // NUL
            "before\u{0000}after".to_string(),             // embedded NUL
            "\t\n\r".to_string(),                          // the named escapes
            "quote\"end".to_string(),                      // double quote
            "back\\slash".to_string(),                     // backslash
            "forward/slash".to_string(),                   // solidus (left raw)
            "\\\"\\\"".to_string(),                        // alternating backslash/quote
            "say \\\"hi\\\" now".to_string(),              // pre-escaped-looking literal
            "emoji 😀 and 中文 and \u{1F680}".to_string(), // multi-byte / astral UTF-8
            "DEL\u{7f}end".to_string(),                    // 0x7F is valid raw JSON
            "tab\tnewline\nreturn\rvertical\u{000b}formfeed\u{000c}".to_string(),
            "x".repeat(10_000), // long body
        ];
        // Every C0 control code, individually.
        for c in 0u32..0x20 {
            cases.push(char::from_u32(c).unwrap().to_string());
        }
        cases
    }

    #[test]
    fn write_then_extract_round_trips_every_escape() {
        for s in escape_torture_strings() {
            let mut line = String::from(r#"{"text":"#);
            write_json_str(&mut line, &s);
            line.push('}');
            let input = parse_input_line(&line)
                .unwrap_or_else(|| panic!("manual round-trip failed to parse: {line:?}"));
            assert_eq!(input.text, s, "manual round-trip mismatch for {s:?}");
        }
    }

    #[test]
    fn write_json_str_escapes_control_bytes_not_raw() {
        // A C0 control code must be emitted as \u00xx, never as a raw byte that
        // would corrupt the NDJSON line framing.
        let mut out = String::new();
        write_json_str(&mut out, "a\u{0001}b\u{001f}c");
        assert!(out.contains("\\u0001"), "U+0001 not escaped: {out}");
        assert!(out.contains("\\u001f"), "U+001F not escaped: {out}");
        assert!(
            !out.chars().any(|c| (c as u32) < 0x20),
            "raw control byte leaked into output: {out:?}"
        );
    }

    #[test]
    fn extract_json_string_stops_at_unescaped_quote_not_escaped_one() {
        // The closing quote must be the first *unescaped* one, so an escaped
        // quote inside the value does not truncate it early.
        let line = r#"{"text":"a\"b","priority":"High"}"#;
        let input = parse_input_line(line).expect("parses");
        assert_eq!(input.text, "a\"b");
        assert_eq!(input.priority, Priority::High);
    }

    #[cfg(feature = "json")]
    #[test]
    fn manually_written_escapes_parse_back_through_serde() {
        // The kernel↔serde interop contract for nasty operator text: anything
        // the dependency-free writer emits must also parse via serde_json.
        for s in escape_torture_strings() {
            let mut line = String::from(r#"{"text":"#);
            write_json_str(&mut line, &s);
            line.push('}');
            let input = json::input_from_line(&line)
                .unwrap_or_else(|| panic!("serde failed to parse manual line: {line:?}"));
            assert_eq!(input.text, s, "serde interop mismatch for {s:?}");
        }
    }

    #[test]
    fn to_ndjson_emits_type_tag_for_every_variant() {
        let events = vec![
            OperatorEvent::Vitals {
                thermal_load: 0.1,
                compute_pressure: 0.2,
                memory_pressure: 0.3,
                power_budget: 1.0,
                financial_budget: 0.9,
                attention_demand: 0.5,
                aggregate_stress: 0.25,
            },
            OperatorEvent::State {
                lifecycle: "Awake".to_string(),
                sleep_phase: None,
                agenda_depth: 3,
            },
            OperatorEvent::Heartbeat { uptime_secs: 42 },
        ];
        for e in &events {
            let line = e.to_ndjson();
            assert!(
                line.contains(&alloc::format!("\"type\":\"{}\"", e.kind())),
                "missing type tag in {line}"
            );
        }
    }

    #[test]
    fn drain_lines_splits_and_keeps_partial_remainder() {
        let mut buf = b"one\ntwo\r\nthr".to_vec();
        let lines = drain_lines(&mut buf);
        assert_eq!(lines, vec!["one".to_string(), "two".to_string()]);
        assert_eq!(buf, b"thr");
    }

    #[cfg(feature = "json")]
    #[test]
    fn manual_ndjson_round_trips_through_serde() {
        // The contract that lets the no_std kernel and serde clients interop:
        // every manually-written line must parse back via serde to an equal value.
        let events = vec![
            OperatorEvent::Vitals {
                thermal_load: 0.5,
                compute_pressure: 0.5,
                memory_pressure: 0.5,
                power_budget: 0.5,
                financial_budget: 0.5,
                attention_demand: 0.5,
                aggregate_stress: 0.5,
            },
            OperatorEvent::Gate {
                invoke: true,
                cost_class: Some("Frontier".to_string()),
                value_score: 0.8,
                threshold: 0.4,
                override_active: false,
                reasoning: "user-facing \"urgent\" query".to_string(),
            },
            OperatorEvent::State {
                lifecycle: "Sleep".to_string(),
                sleep_phase: Some("Dreaming".to_string()),
                agenda_depth: 0,
            },
            OperatorEvent::AgentMessage {
                task_id: 7,
                tokens: 128,
                text: "done:\n\tbuilt the report".to_string(),
            },
        ];
        for e in &events {
            let manual = e.to_ndjson();
            let parsed = json::event_from_line(&manual)
                .unwrap_or_else(|| panic!("serde failed to parse manual line: {manual}"));
            assert_eq!(&parsed, e, "round-trip mismatch for {}", e.kind());
        }
    }

    #[test]
    fn kernel_emit_framing_matches_protocol() {
        // Mirrors `kernels/microvm/operator_console::emit`: the kernel writes
        // `ANIMA_TLM <ndjson>` and the host bridge strips the prefix to parse.
        let event = OperatorEvent::AgentMessage {
            task_id: 1,
            tokens: 7,
            text: "microVM operator console online".to_string(),
        };
        let framed = alloc::format!("{}{}", TELEMETRY_PREFIX, event.to_ndjson());
        assert!(framed.starts_with(TELEMETRY_PREFIX));
        let payload = framed.strip_prefix(TELEMETRY_PREFIX).unwrap();
        assert!(payload.contains("\"type\":\"AgentMessage\""));
    }

    #[test]
    fn kernel_poll_guidance_accumulator_logic() {
        // Mirrors `kernels/microvm/operator_console::poll_guidance`: feed bytes
        // one at a time; a complete `\n`-terminated line yields parsed guidance.
        let mut line: Vec<u8> = Vec::new();
        let feed = b"ANIMA_IN {\"text\":\"reduce batch size\",\"priority\":\"High\"}\n";
        let mut result = None;
        for &byte in feed {
            match byte {
                b'\n' => {
                    result = core::str::from_utf8(&line).ok().and_then(parse_input_line);
                    line.clear();
                }
                b'\r' => {}
                _ => line.push(byte),
            }
        }
        let input = result.expect("complete line parses");
        assert_eq!(input.priority, Priority::High);
        assert_eq!(input.text, "reduce batch size");
    }

    #[cfg(feature = "json")]
    #[test]
    fn input_serde_round_trip() {
        let input = OperatorInput::new("hello").with_priority(Priority::Critical);
        let line = json::input_to_line(&input);
        let back = json::input_from_line(&line).expect("parses");
        assert_eq!(back, input);
    }
}
