#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

//! Afferent sensory input vector: parses text + PCM streams into typed packets.
//!
//! # E3.3 — Sensory Bridge
//!
//! This crate implements the human-facing ingestion surface described in
//! `docs/02-subsystems.md`.  Every inbound signal is wrapped in a
//! [`PrioritizedPacket`] before entering the internal queue, giving the
//! somatic execution loop in `vita` a single priority ordering to work from.
//!
//! Policy bounds enforcement is provided by the checked packetize methods
//! ([`SensoryBridge::packetize_text_checked`] and
//! [`SensoryBridge::packetize_pcm_checked`]).  The unchecked variants are
//! retained for internal and test use and assign [`SensoryPriority::Normal`].

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

// ── Priority ──────────────────────────────────────────────────────────────────

/// Priority level assigned to an incoming sensory packet.
///
/// Higher variants win when the somatic loop selects which sensory-derived
/// task to run next.  The derived [`Ord`] places `Critical > High > Normal >
/// Low`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SensoryPriority {
    /// Lowest urgency — background or informational input.
    Low = 0,
    /// Standard human interaction; the default when no priority is specified.
    Normal = 1,
    /// Elevated urgency, e.g. a follow-up question or clarification.
    High = 2,
    /// Interrupt-level urgency, e.g. an operator emergency override.
    Critical = 3,
}

// ── Packet types ──────────────────────────────────────────────────────────────

/// Streamed sensory payload emitted by the bridge.
#[derive(Debug, Clone, PartialEq)]
pub enum SensoryPacket {
    /// A discrete text-buffer payload.
    Text(String),
    /// PCM audio frame (16-bit little-endian samples).
    Pcm(Vec<i16>),
}

/// A sensory packet paired with its assigned priority level.
///
/// `vita`'s somatic loop consumes [`PrioritizedPacket`]s from the bridge and
/// maps the priority to an MLFQ tier before enqueueing the derived task.
#[derive(Debug, Clone, PartialEq)]
pub struct PrioritizedPacket {
    /// Sensory content.
    pub packet: SensoryPacket,
    /// Urgency level assigned at ingestion time.
    pub priority: SensoryPriority,
}

// ── Policy bounds ─────────────────────────────────────────────────────────────

/// External policy bounds from `/dev/anima/senses/human`.
///
/// Bounds are applied by the checked packetize methods at ingestion time.
/// They can be updated at runtime via [`SensoryBridge::set_active_bounds`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HumanGuidance {
    /// Free-form policy directives provided by the operator.
    pub policy_hint: String,
    /// Maximum allowed text-input length in bytes.  `None` means unlimited.
    pub max_text_length: Option<usize>,
    /// Text inputs that start with any of these prefixes are rejected.
    pub blocked_prefixes: Vec<String>,
}

impl HumanGuidance {
    /// Creates guidance with the given hint and no restrictions.
    pub fn new(policy_hint: impl Into<String>) -> Self {
        Self {
            policy_hint: policy_hint.into(),
            max_text_length: None,
            blocked_prefixes: Vec::new(),
        }
    }
}

impl Default for HumanGuidance {
    fn default() -> Self {
        Self::new("")
    }
}

// ── Errors ────────────────────────────────────────────────────────────────────

/// Errors raised when sensory input cannot be consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SensoryBridgeError {
    /// Input stream did not contain valid guidance.
    InvalidInput,
    /// Input violated the operator's active policy bounds.
    PolicyViolation {
        /// Human-readable explanation of the rejection.
        reason: String,
    },
}

// ── Bridge ────────────────────────────────────────────────────────────────────

/// Minimal sensory bridge for human intent signals.
///
/// The bridge maintains an internal FIFO queue of [`PrioritizedPacket`]s that
/// the somatic execution loop drains each iteration.  Thread-safe via inner
/// `Arc<Mutex<…>>` so that external producers (e.g. a separate I/O thread)
/// can push packets concurrently.
#[derive(Debug, Clone)]
pub struct SensoryBridge {
    active_bounds: Arc<Mutex<HumanGuidance>>,
    queue: Arc<Mutex<VecDeque<PrioritizedPacket>>>,
}

impl SensoryBridge {
    /// Creates a new sensory bridge with the given initial human guidance.
    pub fn new(active_bounds: HumanGuidance) -> Self {
        Self {
            active_bounds: Arc::new(Mutex::new(active_bounds)),
            queue: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    // ── Bound management ─────────────────────────────────────────────────────

    /// Returns the currently active human policy bounds.
    pub fn read_active_bounds(&self) -> Result<HumanGuidance, SensoryBridgeError> {
        Ok(self.active_bounds.lock().clone())
    }

    /// Replaces the active policy bounds.
    ///
    /// Packets already in the queue are not re-validated; the new bounds apply
    /// only to subsequent calls to the checked packetize methods.
    pub fn set_active_bounds(&self, guidance: HumanGuidance) {
        *self.active_bounds.lock() = guidance;
    }

    // ── Unchecked packetize (internal / test convenience) ────────────────────

    /// Enqueues a text packet at [`SensoryPriority::Normal`].
    ///
    /// Does **not** validate against policy bounds.  Prefer
    /// [`packetize_text_checked`](Self::packetize_text_checked) for
    /// externally-sourced input.
    pub fn packetize_text(&self, text: impl Into<String>) {
        self.queue.lock().push_back(PrioritizedPacket {
            packet: SensoryPacket::Text(text.into()),
            priority: SensoryPriority::Normal,
        });
    }

    /// Enqueues a PCM audio frame at [`SensoryPriority::Normal`].
    ///
    /// Does **not** validate against policy bounds.  Prefer
    /// [`packetize_pcm_checked`](Self::packetize_pcm_checked) for
    /// externally-sourced input.
    pub fn packetize_pcm(&self, samples: Vec<i16>) {
        self.queue.lock().push_back(PrioritizedPacket {
            packet: SensoryPacket::Pcm(samples),
            priority: SensoryPriority::Normal,
        });
    }

    // ── Checked packetize (policy-enforcing) ─────────────────────────────────

    /// Validates `text` against the active policy bounds and, if accepted,
    /// enqueues it with `priority`.
    ///
    /// # Errors
    ///
    /// Returns [`SensoryBridgeError::PolicyViolation`] when:
    /// - `text` is empty.
    /// - `text.len()` exceeds [`HumanGuidance::max_text_length`] (when set).
    /// - `text` starts with one of [`HumanGuidance::blocked_prefixes`].
    pub fn packetize_text_checked(
        &self,
        text: impl Into<String>,
        priority: SensoryPriority,
    ) -> Result<(), SensoryBridgeError> {
        let text = text.into();
        let bounds = self.active_bounds.lock().clone();

        if text.is_empty() {
            return Err(SensoryBridgeError::PolicyViolation {
                reason: "text input must not be empty".into(),
            });
        }
        if let Some(max_len) = bounds.max_text_length {
            if text.len() > max_len {
                return Err(SensoryBridgeError::PolicyViolation {
                    reason: format!("text length {} exceeds policy limit {max_len}", text.len()),
                });
            }
        }
        for prefix in &bounds.blocked_prefixes {
            if text.starts_with(prefix.as_str()) {
                return Err(SensoryBridgeError::PolicyViolation {
                    reason: format!("text starts with blocked prefix {prefix:?}"),
                });
            }
        }

        self.queue.lock().push_back(PrioritizedPacket {
            packet: SensoryPacket::Text(text),
            priority,
        });
        Ok(())
    }

    /// Validates `samples` against the active policy bounds and, if accepted,
    /// enqueues them with `priority`.
    ///
    /// # Errors
    ///
    /// Returns [`SensoryBridgeError::PolicyViolation`] when `samples` is empty
    /// (an empty PCM frame carries no useful information).
    pub fn packetize_pcm_checked(
        &self,
        samples: Vec<i16>,
        priority: SensoryPriority,
    ) -> Result<(), SensoryBridgeError> {
        if samples.is_empty() {
            return Err(SensoryBridgeError::PolicyViolation {
                reason: "PCM frame must not be empty".into(),
            });
        }
        self.queue.lock().push_back(PrioritizedPacket {
            packet: SensoryPacket::Pcm(samples),
            priority,
        });
        Ok(())
    }

    // ── Queue inspection & consumption ────────────────────────────────────────

    /// Returns `true` when at least one packet is waiting to be consumed.
    pub fn has_packets(&self) -> bool {
        !self.queue.lock().is_empty()
    }

    /// Pops the next sensory packet (priority stripped), if any.
    ///
    /// Retained for backward compatibility.  Prefer
    /// [`next_prioritized_packet`](Self::next_prioritized_packet) when
    /// priority-sensitive routing is needed.
    pub fn next_packet(&self) -> Option<SensoryPacket> {
        self.queue.lock().pop_front().map(|p| p.packet)
    }

    /// Pops the next priority-tagged packet, if any.
    ///
    /// `vita`'s somatic execution loop calls this each iteration to drain the
    /// incoming sensory queue before selecting the next task.
    pub fn next_prioritized_packet(&self) -> Option<PrioritizedPacket> {
        self.queue.lock().pop_front()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Existing functionality (backward-compat) ──────────────────────────────

    #[test]
    fn read_active_bounds_returns_current_guidance() {
        let bridge = SensoryBridge::new(HumanGuidance::new("low-cost"));
        let g = bridge.read_active_bounds().unwrap();
        assert_eq!(g.policy_hint, "low-cost");
    }

    #[test]
    fn packetize_and_pop_round_trip() {
        let bridge = SensoryBridge::new(HumanGuidance::new("x"));
        bridge.packetize_text("hello");
        bridge.packetize_pcm(vec![1, 2, 3]);
        assert!(matches!(bridge.next_packet(), Some(SensoryPacket::Text(_))));
        assert!(matches!(bridge.next_packet(), Some(SensoryPacket::Pcm(_))));
        assert!(bridge.next_packet().is_none());
    }

    // ── Priority tagging ─────────────────────────────────────────────────────

    #[test]
    fn text_packet_carries_default_normal_priority() {
        let bridge = SensoryBridge::new(HumanGuidance::new("x"));
        bridge.packetize_text("hello");
        let pkt = bridge.next_prioritized_packet().unwrap();
        assert_eq!(pkt.priority, SensoryPriority::Normal);
        assert!(matches!(pkt.packet, SensoryPacket::Text(_)));
    }

    #[test]
    fn pcm_packet_carries_default_normal_priority() {
        let bridge = SensoryBridge::new(HumanGuidance::new("x"));
        bridge.packetize_pcm(vec![1, 2, 3]);
        let pkt = bridge.next_prioritized_packet().unwrap();
        assert_eq!(pkt.priority, SensoryPriority::Normal);
        assert!(matches!(pkt.packet, SensoryPacket::Pcm(_)));
    }

    #[test]
    fn checked_text_accepts_valid_input_with_assigned_priority() {
        let bridge = SensoryBridge::new(HumanGuidance::new("policy"));
        bridge
            .packetize_text_checked("valid input", SensoryPriority::High)
            .expect("should accept valid text");
        let pkt = bridge.next_prioritized_packet().unwrap();
        assert_eq!(pkt.priority, SensoryPriority::High);
        assert!(matches!(&pkt.packet, SensoryPacket::Text(t) if t == "valid input"));
    }

    #[test]
    fn checked_pcm_accepts_valid_frame_with_assigned_priority() {
        let bridge = SensoryBridge::new(HumanGuidance::new("policy"));
        bridge
            .packetize_pcm_checked(vec![100, 200, 300], SensoryPriority::High)
            .expect("should accept non-empty PCM");
        let pkt = bridge.next_prioritized_packet().unwrap();
        assert_eq!(pkt.priority, SensoryPriority::High);
        assert!(matches!(&pkt.packet, SensoryPacket::Pcm(s) if s.len() == 3));
    }

    #[test]
    fn critical_priority_is_ordered_above_high() {
        assert!(SensoryPriority::Critical > SensoryPriority::High);
        assert!(SensoryPriority::High > SensoryPriority::Normal);
        assert!(SensoryPriority::Normal > SensoryPriority::Low);
    }

    // ── Policy-bounds enforcement (E3.3 exit criteria 2) ─────────────────────

    #[test]
    fn checked_text_rejects_empty_input_without_panicking() {
        let bridge = SensoryBridge::new(HumanGuidance::new("policy"));
        let err = bridge
            .packetize_text_checked("", SensoryPriority::Normal)
            .unwrap_err();
        assert!(
            matches!(err, SensoryBridgeError::PolicyViolation { .. }),
            "expected PolicyViolation, got {err:?}"
        );
        assert!(
            !bridge.has_packets(),
            "queue must stay empty after rejection"
        );
    }

    #[test]
    fn checked_text_rejects_input_exceeding_max_length_without_panicking() {
        let bridge = SensoryBridge::new(HumanGuidance {
            policy_hint: "strict-len".to_string(),
            max_text_length: Some(5),
            blocked_prefixes: Vec::new(),
        });
        let err = bridge
            .packetize_text_checked("too long input", SensoryPriority::Normal)
            .unwrap_err();
        assert!(matches!(err, SensoryBridgeError::PolicyViolation { .. }));
        assert!(!bridge.has_packets());
    }

    #[test]
    fn checked_text_accepts_input_exactly_at_max_length() {
        let bridge = SensoryBridge::new(HumanGuidance {
            policy_hint: "strict-len".to_string(),
            max_text_length: Some(5),
            blocked_prefixes: Vec::new(),
        });
        bridge
            .packetize_text_checked("hello", SensoryPriority::Normal)
            .expect("exactly-at-limit text should be accepted");
        assert!(bridge.has_packets());
    }

    #[test]
    fn checked_text_rejects_blocked_prefix_without_panicking() {
        let bridge = SensoryBridge::new(HumanGuidance {
            policy_hint: "no-sys".to_string(),
            max_text_length: None,
            blocked_prefixes: vec!["SYSTEM:".to_string(), "OVERRIDE:".to_string()],
        });
        let err = bridge
            .packetize_text_checked("SYSTEM: override policy", SensoryPriority::Critical)
            .unwrap_err();
        assert!(matches!(err, SensoryBridgeError::PolicyViolation { .. }));
        assert!(!bridge.has_packets());

        let err2 = bridge
            .packetize_text_checked("OVERRIDE: disable limits", SensoryPriority::Critical)
            .unwrap_err();
        assert!(matches!(err2, SensoryBridgeError::PolicyViolation { .. }));
    }

    #[test]
    fn checked_text_accepts_non_blocked_input() {
        let bridge = SensoryBridge::new(HumanGuidance {
            policy_hint: "no-sys".to_string(),
            max_text_length: None,
            blocked_prefixes: vec!["SYSTEM:".to_string()],
        });
        bridge
            .packetize_text_checked("user: normal query", SensoryPriority::Normal)
            .expect("non-blocked text should be accepted");
        assert!(bridge.has_packets());
    }

    #[test]
    fn checked_pcm_rejects_empty_frame_without_panicking() {
        let bridge = SensoryBridge::new(HumanGuidance::new("policy"));
        let err = bridge
            .packetize_pcm_checked(vec![], SensoryPriority::Normal)
            .unwrap_err();
        assert!(matches!(err, SensoryBridgeError::PolicyViolation { .. }));
        assert!(!bridge.has_packets());
    }

    // ── Queue inspection ─────────────────────────────────────────────────────

    #[test]
    fn has_packets_reflects_queue_state() {
        let bridge = SensoryBridge::new(HumanGuidance::new("policy"));
        assert!(!bridge.has_packets());
        bridge.packetize_text("hello");
        assert!(bridge.has_packets());
        bridge.next_packet();
        assert!(!bridge.has_packets());
    }

    // ── Runtime policy update ─────────────────────────────────────────────────

    #[test]
    fn policy_bounds_can_be_tightened_at_runtime() {
        let bridge = SensoryBridge::new(HumanGuidance::new("permissive"));

        // Accept long text under current permissive policy.
        bridge
            .packetize_text_checked("this is a moderately long input", SensoryPriority::Normal)
            .expect("permissive policy should accept");

        // Drain queue so state is clean.
        bridge.next_packet();

        // Tighten: enforce a very short max_length.
        bridge.set_active_bounds(HumanGuidance {
            policy_hint: "strict".to_string(),
            max_text_length: Some(4),
            blocked_prefixes: Vec::new(),
        });

        // Same text is now rejected.
        let err = bridge
            .packetize_text_checked("this is a moderately long input", SensoryPriority::Normal)
            .unwrap_err();
        assert!(matches!(err, SensoryBridgeError::PolicyViolation { .. }));
        assert!(!bridge.has_packets());
    }

    #[test]
    fn human_guidance_new_constructor_produces_no_restrictions() {
        let g = HumanGuidance::new("hint");
        assert_eq!(g.policy_hint, "hint");
        assert!(g.max_text_length.is_none());
        assert!(g.blocked_prefixes.is_empty());
    }
}
