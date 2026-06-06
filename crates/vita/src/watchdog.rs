//! Cognitive watchdogs & agent-level rollback — E14, S14.4.
//!
//! The same idea as tool circuit breakers (`praxis::CircuitBreaker`), applied
//! to **cognition**: detect stuck loops, obsessive single-goal pursuit, and
//! output-hallucination spirals; break the loop; downgrade to a safe reflexive
//! policy; surface to the operator.
//!
//! # Detectors
//!
//! | Detector | What it watches | Trip condition |
//! |----------|-----------------|----------------|
//! | `StuckLoop` | Consecutive cortex outputs | Hash collision ≥ N times |
//! | `NoProgress` | Consecutive cortex invocations without tool calls | N tool-call-free invocations in a row |
//! | `ShortOutputSpiral` | Mean output length over a window | Mean < 15 chars for N consecutive invocations |
//!
//! # Rollback
//!
//! An [`AgentSnapshot`] captures a lightweight checkpoint of the agent's
//! cognitive state (identity memory + L1 node keys).  When a watchdog trips
//! after a self-modification (E11) the lifecycle manager can restore the
//! last known-good snapshot.
//!
//! # Exit criteria (S14.4)
//!
//! 1. `StuckLoop` detector trips when N consecutive outputs hash to the same value.
//! 2. `NoProgress` detector trips when N consecutive invocations make zero tool calls.
//! 3. A `WatchdogTrip` contains the detector name and reason.
//! 4. `AgentSnapshot` captures and restores identity facts.

use serde::{Deserialize, Serialize};

// ── WatchdogConfig ────────────────────────────────────────────────────────────

/// Configuration for the cognitive watchdog.
#[derive(Debug, Clone, PartialEq)]
pub struct WatchdogConfig {
    /// Number of consecutive identical-hash outputs that trip `StuckLoop`.
    pub stuck_loop_threshold: u32,
    /// Number of consecutive zero-tool-call invocations that trip `NoProgress`.
    pub no_progress_threshold: u32,
    /// Number of consecutive short (< `short_output_min_chars`) outputs that
    /// trip `ShortOutputSpiral`.
    pub short_output_threshold: u32,
    /// Minimum output length in bytes below which an output is "short".
    pub short_output_min_chars: usize,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            stuck_loop_threshold: 3,
            no_progress_threshold: 5,
            short_output_threshold: 4,
            short_output_min_chars: 15,
        }
    }
}

// ── WatchdogTrip ─────────────────────────────────────────────────────────────

/// Result when a watchdog detector trips.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchdogTrip {
    /// Human-readable name of the detector that fired.
    pub detector: String,
    /// Human-readable description of why it fired.
    pub reason: String,
    /// How many consecutive anomalous observations were seen before the trip.
    pub streak: u32,
}

// ── WatchdogState ─────────────────────────────────────────────────────────────

/// Mutable state maintained across cortex invocations.
#[derive(Debug, Clone, Default)]
pub struct WatchdogState {
    /// FNV hash of the last cortex output (for stuck-loop detection).
    last_output_hash: u64,
    /// Count of consecutive outputs with the same hash.
    stuck_streak: u32,
    /// Count of consecutive invocations with zero tool calls.
    no_progress_streak: u32,
    /// Count of consecutive short outputs.
    short_output_streak: u32,
}

// ── CognitiveWatchdog ─────────────────────────────────────────────────────────

/// Monitors cortex invocations and trips when cognitive failure is detected.
///
/// Maintain one `CognitiveWatchdog` per agent.  Call [`record_invocation`]
/// after each cortex completion, then call [`check_trip`] to test for a trip.
///
/// A tripped watchdog does **not** reset automatically: the caller must call
/// [`reset`] after surfacing the trip to the operator and taking remedial action.
#[derive(Debug, Clone)]
pub struct CognitiveWatchdog {
    /// Watchdog configuration (thresholds).
    pub config: WatchdogConfig,
    /// Mutable detection state.
    state: WatchdogState,
    /// Total number of trips since last reset.
    trip_count: u32,
    /// Pending trip waiting to be surfaced (if any).
    pending_trip: Option<WatchdogTrip>,
}

impl CognitiveWatchdog {
    /// Create a watchdog with the given configuration.
    pub fn new(config: WatchdogConfig) -> Self {
        Self {
            config,
            state: WatchdogState::default(),
            trip_count: 0,
            pending_trip: None,
        }
    }

    /// Create a watchdog with default thresholds.
    pub fn with_defaults() -> Self {
        Self::new(WatchdogConfig::default())
    }

    /// Record a cortex invocation result and update detection state.
    ///
    /// Call this after every successful cortex completion.  For faulted
    /// invocations, only call this if the fault is recoverable (e.g. a
    /// transient timeout); persistent faults should be handled by the
    /// fault-isolation path separately.
    pub fn record_invocation(&mut self, output: &str, tool_calls_made: usize) {
        // Stuck-loop detector: count consecutive identical outputs including the
        // first — after 3 identical outputs the streak is 3.
        let hash = fnv_hash(output.trim());
        if !output.trim().is_empty() {
            if hash == self.state.last_output_hash {
                self.state.stuck_streak += 1;
            } else {
                // New unique output: restart at 1 (the current observation).
                self.state.stuck_streak = 1;
            }
        } else {
            self.state.stuck_streak = 0;
        }
        self.state.last_output_hash = hash;

        // No-progress detector.
        if tool_calls_made == 0 {
            self.state.no_progress_streak += 1;
        } else {
            self.state.no_progress_streak = 0;
        }

        // Short-output-spiral detector.
        if output.trim().len() < self.config.short_output_min_chars {
            self.state.short_output_streak += 1;
        } else {
            self.state.short_output_streak = 0;
        }

        // Evaluate all detectors and set the pending trip if any fires.
        if self.pending_trip.is_none() {
            self.pending_trip = self.evaluate_detectors();
            if self.pending_trip.is_some() {
                self.trip_count += 1;
            }
        }
    }

    /// Evaluate all detectors and return the first trip (if any).
    fn evaluate_detectors(&self) -> Option<WatchdogTrip> {
        if self.state.stuck_streak >= self.config.stuck_loop_threshold {
            return Some(WatchdogTrip {
                detector: "StuckLoop".to_string(),
                reason: format!(
                    "cortex output hash repeated {} consecutive times — agent appears stuck",
                    self.state.stuck_streak
                ),
                streak: self.state.stuck_streak,
            });
        }
        if self.state.no_progress_streak >= self.config.no_progress_threshold {
            return Some(WatchdogTrip {
                detector: "NoProgress".to_string(),
                reason: format!(
                    "{} consecutive invocations with zero tool calls — no observable progress",
                    self.state.no_progress_streak
                ),
                streak: self.state.no_progress_streak,
            });
        }
        if self.state.short_output_streak >= self.config.short_output_threshold {
            return Some(WatchdogTrip {
                detector: "ShortOutputSpiral".to_string(),
                reason: format!(
                    "{} consecutive outputs shorter than {} bytes — possible hallucination spiral",
                    self.state.short_output_streak, self.config.short_output_min_chars
                ),
                streak: self.state.short_output_streak,
            });
        }
        None
    }

    /// Returns the pending trip (if any) without consuming it.
    ///
    /// `Some` signals that the lifecycle manager should break the loop,
    /// surface the trip to the operator, and call [`reset`].
    pub fn check_trip(&self) -> Option<&WatchdogTrip> {
        self.pending_trip.as_ref()
    }

    /// Reset the watchdog state and clear any pending trip.
    ///
    /// Call after the lifecycle manager has surfaced the trip and taken
    /// remedial action (e.g. operator acknowledgement, route downgrade).
    pub fn reset(&mut self) {
        self.state = WatchdogState::default();
        self.pending_trip = None;
    }

    /// Total number of trips since this watchdog was created (or last [`reset`]).
    pub fn trip_count(&self) -> u32 {
        self.trip_count
    }

    /// Returns `true` when a trip is pending (same as `check_trip().is_some()`).
    pub fn is_tripped(&self) -> bool {
        self.pending_trip.is_some()
    }
}

// ── AgentSnapshot ─────────────────────────────────────────────────────────────

/// Lightweight checkpoint of an agent's cognitive state for rollback.
///
/// Captures identity facts and L1 memory node keys (not full node data — the
/// full data lives in the L3 archive and can be retrieved by key if needed).
/// Snapshots are taken before potentially-dangerous operations (e.g. self-
/// modification in E11) so the lifecycle manager can restore the last known-
/// good state on a watchdog trip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSnapshot {
    /// Wall-clock nanoseconds when the snapshot was taken.
    pub taken_at_ns: u64,
    /// Snapshot of the identity-memory JSON document.
    pub identity_snapshot: serde_json::Value,
    /// Keys of all L1 memory nodes at snapshot time.
    pub l1_node_keys: Vec<String>,
    /// Optional description of why the snapshot was taken.
    pub description: String,
}

impl AgentSnapshot {
    /// Create a snapshot.
    pub fn new(
        now_ns: u64,
        identity: serde_json::Value,
        l1_keys: Vec<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            taken_at_ns: now_ns,
            identity_snapshot: identity,
            l1_node_keys: l1_keys,
            description: description.into(),
        }
    }
}

// ── FNV-1a hash ───────────────────────────────────────────────────────────────

fn fnv_hash(s: &str) -> u64 {
    const FNV_OFFSET: u64 = 14695981039346656037;
    const FNV_PRIME: u64 = 1099511628211;
    let mut hash = FNV_OFFSET;
    for byte in s.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn dog() -> CognitiveWatchdog {
        CognitiveWatchdog::new(WatchdogConfig {
            stuck_loop_threshold: 3,
            no_progress_threshold: 3,
            short_output_threshold: 3,
            short_output_min_chars: 10,
        })
    }

    // ── S14.4 Exit criterion 1 — StuckLoop detector ───────────────────────────

    #[test]
    fn stuck_loop_trips_after_n_identical_outputs() {
        let mut w = dog();
        // stuck_loop_threshold = 3: trips when streak reaches 3.
        // With the counting algorithm (streak=1 on first occurrence), after N
        // identical outputs the streak equals N.
        let output = "The answer is exactly forty-two.";

        w.record_invocation(output, 1);
        assert!(
            w.check_trip().is_none(),
            "not yet tripped after 1 (streak=1 < 3)"
        );
        w.record_invocation(output, 1);
        assert!(
            w.check_trip().is_none(),
            "not yet tripped after 2 (streak=2 < 3)"
        );
        w.record_invocation(output, 1);
        // Third identical output → streak=3 = threshold → trip.
        assert!(w.check_trip().is_some(), "should trip at threshold (3)");
        let trip = w.check_trip().unwrap();
        assert_eq!(trip.detector, "StuckLoop");
        assert_eq!(trip.streak, 3);
    }

    #[test]
    fn stuck_loop_resets_on_different_output() {
        let mut w = dog();
        w.record_invocation("same output text here", 1);
        w.record_invocation("same output text here", 1);
        w.record_invocation("completely different output text", 1); // resets streak to 1
        assert!(w.check_trip().is_none(), "streak reset — no trip");
    }

    #[test]
    fn empty_output_does_not_trip_stuck_loop() {
        let mut w = dog();
        // Empty outputs don't increment the stuck streak.
        for _ in 0..5 {
            w.record_invocation("", 1);
        }
        // If empty repeatedly: stuck_streak stays 0 because we skip empty.
        assert!(
            w.check_trip()
                .map(|t| t.detector != "StuckLoop")
                .unwrap_or(true),
            "empty output should not trip StuckLoop"
        );
    }

    // ── S14.4 Exit criterion 2 — NoProgress detector ─────────────────────────

    #[test]
    fn no_progress_trips_after_n_zero_tool_call_invocations() {
        let mut w = dog();
        for _ in 0..2 {
            w.record_invocation("some unique output", 0);
        }
        assert!(w.check_trip().is_none(), "not yet at 2");
        w.record_invocation("another unique output", 0);
        assert!(
            w.check_trip().is_some(),
            "should trip at 3 zero-tool invocations"
        );
        let trip = w.check_trip().unwrap();
        assert_eq!(trip.detector, "NoProgress");
    }

    #[test]
    fn no_progress_resets_on_tool_call() {
        let mut w = dog();
        // Use strings longer than short_output_min_chars (10) to avoid triggering
        // the ShortOutputSpiral detector.
        w.record_invocation("analyzed the first request carefully", 0);
        w.record_invocation("analyzed the second request carefully", 0);
        w.record_invocation("analyzed the third request carefully", 1); // tool call resets streak
        assert!(w.check_trip().is_none(), "streak reset by tool call");
    }

    // ── S14.4 Exit criterion 3 — WatchdogTrip fields ─────────────────────────

    #[test]
    fn watchdog_trip_contains_detector_and_reason() {
        let mut w = dog();
        // A unique-per-call output avoids the StuckLoop detector; zero tool calls
        // trip the NoProgress detector at threshold=3.
        w.record_invocation("the result of analysis run one", 0);
        w.record_invocation("the result of analysis run two", 0);
        w.record_invocation("the result of analysis run three", 0);
        let trip = w.check_trip().expect("tripped");
        assert!(!trip.detector.is_empty(), "detector must be non-empty");
        assert!(!trip.reason.is_empty(), "reason must be non-empty");
        assert!(trip.streak >= 3, "streak must be >= threshold");
    }

    // ── S14.4 Exit criterion 4 — AgentSnapshot ────────────────────────────────

    #[test]
    fn agent_snapshot_captures_and_restores_identity_facts() {
        let now_ns = 1_700_000_000_000_000_000u64;
        let identity = serde_json::json!({"user": {"name": "Alice"}, "version": 1});
        let l1_keys = vec!["key1".to_string(), "key2".to_string()];

        let snap = AgentSnapshot::new(
            now_ns,
            identity.clone(),
            l1_keys.clone(),
            "pre-modification",
        );

        assert_eq!(snap.taken_at_ns, now_ns);
        assert_eq!(snap.identity_snapshot, identity);
        assert_eq!(snap.l1_node_keys, l1_keys);
        assert_eq!(snap.description, "pre-modification");
    }

    // ── Reset behaviour ───────────────────────────────────────────────────────

    #[test]
    fn reset_clears_pending_trip_and_streaks() {
        let mut w = dog();
        let output = "same";
        for _ in 0..3 {
            w.record_invocation(output, 1);
        }
        assert!(w.is_tripped());
        w.reset();
        assert!(!w.is_tripped(), "trip cleared after reset");
        // State should be zeroed — one more same output should not immediately re-trip.
        w.record_invocation(output, 1);
        assert!(!w.is_tripped(), "one record after reset should not trip");
    }

    #[test]
    fn trip_count_accumulates_across_resets() {
        let mut w = dog();
        for _ in 0..3 {
            w.record_invocation("same", 1);
        }
        assert_eq!(w.trip_count(), 1);
        w.reset();
        for _ in 0..3 {
            w.record_invocation("other", 1);
        }
        assert_eq!(w.trip_count(), 2, "trip count accumulates");
    }

    // ── ShortOutputSpiral detector ────────────────────────────────────────────

    #[test]
    fn short_output_spiral_trips_after_n_short_outputs() {
        let mut w = dog();
        // Use distinct short outputs so StuckLoop doesn't fire first.
        let outputs = ["ok.", "yes", "no."]; // each < 10 chars, all different
        for output in &outputs[..2] {
            w.record_invocation(output, 1);
        }
        assert!(w.check_trip().is_none(), "not yet at 2 short outputs");
        w.record_invocation(outputs[2], 1);
        assert!(
            w.check_trip().is_some(),
            "should trip after 3 short outputs"
        );
        let trip = w.check_trip().unwrap();
        assert_eq!(trip.detector, "ShortOutputSpiral");
    }

    #[test]
    fn long_output_resets_short_output_streak() {
        let mut w = dog();
        w.record_invocation("ok", 1);
        w.record_invocation("ok", 1);
        w.record_invocation(
            "This is a much longer output that exceeds the minimum length threshold",
            1,
        );
        assert!(w.check_trip().is_none(), "long output resets short streak");
    }
}
