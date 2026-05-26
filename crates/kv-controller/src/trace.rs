//! KV-cache trace capture — Story S5.4.2.
//!
//! Replay-quality logging of cortex invocations with token-level metadata
//! sufficient to reconstruct a training episode. Captured under an explicit
//! opt-in flag because trace payloads contain conversation content.
//!
//! # Provenance
//!
//! Every trace record carries a [`TraceProvenance`] tag documenting the source
//! of the data (live cortex trace, synthetic needle, or public dataset). Exit
//! criterion 3 requires all training episodes to be tagged with their source
//! and aggregate counts to be published per release. The types here support
//! that requirement.
//!
//! # Opt-in flag
//!
//! Trace capture is disabled by default. The caller must set
//! `TraceConfig::enabled = true` explicitly. This satisfies the privacy
//! requirement in S5.4.2 ("captured under an explicit opt-in flag").

#![forbid(unsafe_code)]

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::features::BlockFeatures;

// ── Provenance ─────────────────────────────────────────────────────────────────

/// Source classification for a KV trace record (exit criterion 3).
///
/// Every training episode must be tagged with one of these variants so that
/// aggregate counts can be published per release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TraceProvenance {
    /// Captured from a live cortex invocation on the hosted target.
    ///
    /// This is the highest-quality source but carries the most privacy risk;
    /// opt-in is required to capture this variant.
    LiveCortexTrace {
        /// Route ID that was active during capture.
        route_id: String,
        /// Whether the task was user-facing.
        user_facing: bool,
    },
    /// Synthetically generated (e.g., needle-insertion harness from E5.4.5).
    ///
    /// These traces are fully reproducible from a seed and carry no privacy risk.
    Synthetic {
        /// Descriptive label for the generation procedure.
        generator: String,
        /// Random seed used for generation.
        seed: u64,
    },
    /// Sourced from a published public agent-trace dataset.
    ///
    /// The dataset name and URL are recorded so provenance is fully auditable.
    PublicDataset {
        /// Human-readable name of the dataset.
        name: String,
        /// URL or DOI of the source.
        source_url: String,
    },
}

// ── Trace configuration ────────────────────────────────────────────────────────

/// Configuration controlling trace capture behaviour.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceConfig {
    /// Master switch — if `false` no traces are captured (default: `false`).
    pub enabled: bool,
    /// Maximum number of trace records buffered before being flushed.
    pub buffer_capacity: usize,
    /// Provenance tag applied to all records captured under this config.
    pub provenance: TraceProvenance,
}

impl TraceConfig {
    /// Returns a disabled configuration (no traces captured).
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            buffer_capacity: 0,
            provenance: TraceProvenance::Synthetic {
                generator: "none".into(),
                seed: 0,
            },
        }
    }

    /// Returns a configuration for live cortex trace capture.
    pub fn live(route_id: impl Into<String>, user_facing: bool, buffer_capacity: usize) -> Self {
        Self {
            enabled: true,
            buffer_capacity,
            provenance: TraceProvenance::LiveCortexTrace {
                route_id: route_id.into(),
                user_facing,
            },
        }
    }

    /// Returns a configuration for synthetic trace generation.
    pub fn synthetic(generator: impl Into<String>, seed: u64, buffer_capacity: usize) -> Self {
        Self {
            enabled: true,
            buffer_capacity,
            provenance: TraceProvenance::Synthetic {
                generator: generator.into(),
                seed,
            },
        }
    }
}

// ── Trace record ───────────────────────────────────────────────────────────────

/// A single trace event recording the gate decision for one block.
///
/// Collected across an invocation's context blocks to form a
/// [`InvocationTrace`] that can be used as a training episode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockTraceRecord {
    /// The features that drove the gating decision.
    pub features: BlockFeatures,
    /// Gate score produced by the controller (or LRU score on fallback).
    pub gate_score: f32,
    /// Whether the block was retained.
    pub retained: bool,
    /// Whether this was an LRU fallback decision.
    pub was_fallback: bool,
    /// Teacher label: `true` if the block would be retained by the full-cache
    /// teacher (used in offline training, set post-hoc).
    pub teacher_label: Option<bool>,
}

/// Complete trace for one cortex invocation — the unit of a training episode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvocationTrace {
    /// Unique identifier for this invocation (matches cortex task ID).
    pub invocation_id: String,
    /// Unix timestamp (seconds) when the invocation started.
    pub started_at_secs: u64,
    /// The route that was active when this trace was captured.
    pub route_id: String,
    /// Block-level trace records, one per block in the working context.
    pub blocks: Vec<BlockTraceRecord>,
    /// Data provenance (exit criterion 3).
    pub provenance: TraceProvenance,
    /// Number of turns executed in this invocation.
    pub turns: u32,
    /// Whether the invocation completed successfully.
    pub completed: bool,
}

impl InvocationTrace {
    /// Number of needle blocks (user constraints) in this trace.
    pub fn needle_count(&self) -> usize {
        self.blocks
            .iter()
            .filter(|b| b.features.is_user_constraint)
            .count()
    }

    /// Number of needle blocks that were retained.
    pub fn needles_retained(&self) -> usize {
        self.blocks
            .iter()
            .filter(|b| b.features.is_user_constraint && b.retained)
            .count()
    }

    /// Needle recall: proportion of needle blocks that were retained.
    pub fn needle_recall(&self) -> f32 {
        let total = self.needle_count();
        if total == 0 {
            1.0
        } else {
            self.needles_retained() as f32 / total as f32
        }
    }
}

// ── Trace capture buffer ───────────────────────────────────────────────────────

/// Accumulates block-level trace records during a single cortex invocation.
///
/// Dropped when the invocation ends if `config.enabled = false`.
pub struct TraceCapture {
    config: TraceConfig,
    records: Vec<BlockTraceRecord>,
    invocation_id: String,
    route_id: String,
    started_at_secs: u64,
}

impl TraceCapture {
    /// Creates a new capture session for `invocation_id`.
    ///
    /// If `config.enabled = false` the buffer is always empty and
    /// [`flush`](Self::flush) returns `None`.
    pub fn new(
        config: TraceConfig,
        invocation_id: impl Into<String>,
        route_id: impl Into<String>,
    ) -> Self {
        let started_at_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            config,
            records: Vec::new(),
            invocation_id: invocation_id.into(),
            route_id: route_id.into(),
            started_at_secs,
        }
    }

    /// Records a block gate decision if capture is enabled.
    pub fn record(
        &mut self,
        features: BlockFeatures,
        gate_score: f32,
        retained: bool,
        was_fallback: bool,
    ) {
        if !self.config.enabled {
            return;
        }
        if self.records.len() >= self.config.buffer_capacity.max(1) {
            // Buffer full — drop oldest record to make room.
            self.records.remove(0);
        }
        self.records.push(BlockTraceRecord {
            features,
            gate_score,
            retained,
            was_fallback,
            teacher_label: None,
        });
    }

    /// Finalises the capture session and returns an [`InvocationTrace`].
    ///
    /// Returns `None` if capture was disabled or no records were accumulated.
    pub fn flush(self, turns: u32, completed: bool) -> Option<InvocationTrace> {
        if !self.config.enabled || self.records.is_empty() {
            return None;
        }
        Some(InvocationTrace {
            invocation_id: self.invocation_id,
            started_at_secs: self.started_at_secs,
            route_id: self.route_id,
            blocks: self.records,
            provenance: self.config.provenance,
            turns,
            completed,
        })
    }

    /// Returns the number of records currently buffered.
    pub fn buffered(&self) -> usize {
        self.records.len()
    }
}

// ── Aggregate provenance statistics ───────────────────────────────────────────

/// Per-source count of training episodes (exit criterion 3).
///
/// Published in the release notes to satisfy the provenance audit requirement.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceCounts {
    /// Traces captured from live cortex invocations.
    pub live_traces: usize,
    /// Synthetically generated episodes.
    pub synthetic: usize,
    /// Episodes sourced from public datasets.
    pub public_dataset: usize,
}

impl ProvenanceCounts {
    /// Tallies provenance counts across a collection of traces.
    pub fn from_traces(traces: &[InvocationTrace]) -> Self {
        let mut counts = Self::default();
        for t in traces {
            match &t.provenance {
                TraceProvenance::LiveCortexTrace { .. } => counts.live_traces += 1,
                TraceProvenance::Synthetic { .. } => counts.synthetic += 1,
                TraceProvenance::PublicDataset { .. } => counts.public_dataset += 1,
            }
        }
        counts
    }

    /// Total number of training episodes across all sources.
    pub fn total(&self) -> usize {
        self.live_traces + self.synthetic + self.public_dataset
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::{BlockFeatures, BlockRole};

    fn make_features(idx: usize, total: usize, is_needle: bool) -> BlockFeatures {
        BlockFeatures::new(idx, total, BlockRole::User, is_needle, false, false, 0.0)
    }

    #[test]
    fn trace_capture_records_nothing_when_disabled() {
        let config = TraceConfig::disabled();
        let mut cap = TraceCapture::new(config, "inv-1", "cheap-local");
        cap.record(make_features(0, 1, false), 0.8, true, false);
        assert_eq!(cap.buffered(), 0);
        assert!(cap.flush(1, true).is_none());
    }

    #[test]
    fn trace_capture_records_blocks_when_enabled() {
        let config = TraceConfig::synthetic("test-gen", 42, 100);
        let mut cap = TraceCapture::new(config, "inv-1", "mid-tier");
        cap.record(make_features(0, 3, true), 0.9, true, false);
        cap.record(make_features(1, 3, false), 0.4, false, false);
        cap.record(make_features(2, 3, false), 0.7, true, false);
        assert_eq!(cap.buffered(), 3);
        let trace = cap.flush(2, true).unwrap();
        assert_eq!(trace.blocks.len(), 3);
        assert_eq!(trace.turns, 2);
        assert!(trace.completed);
    }

    #[test]
    fn invocation_trace_needle_recall_all_retained() {
        let config = TraceConfig::synthetic("test", 0, 100);
        let mut cap = TraceCapture::new(config, "inv-needle", "frontier");
        cap.record(make_features(0, 3, true), 0.9, true, false); // needle retained
        cap.record(make_features(1, 3, true), 0.8, true, false); // needle retained
        cap.record(make_features(2, 3, false), 0.3, false, false);
        let trace = cap.flush(1, true).unwrap();
        assert!((trace.needle_recall() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn invocation_trace_needle_recall_partial() {
        let config = TraceConfig::synthetic("test", 0, 100);
        let mut cap = TraceCapture::new(config, "inv-partial", "frontier");
        cap.record(make_features(0, 4, true), 0.9, true, false); // retained
        cap.record(make_features(1, 4, true), 0.2, false, false); // evicted
        cap.record(make_features(2, 4, true), 0.8, true, false); // retained
        cap.record(make_features(3, 4, true), 0.1, false, false); // evicted
        let trace = cap.flush(1, true).unwrap();
        assert!((trace.needle_recall() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn provenance_counts_tally_correctly() {
        let live = InvocationTrace {
            invocation_id: "a".into(),
            started_at_secs: 0,
            route_id: "r".into(),
            blocks: vec![],
            provenance: TraceProvenance::LiveCortexTrace {
                route_id: "r".into(),
                user_facing: true,
            },
            turns: 0,
            completed: true,
        };
        let synth = InvocationTrace {
            invocation_id: "b".into(),
            started_at_secs: 0,
            route_id: "r".into(),
            blocks: vec![],
            provenance: TraceProvenance::Synthetic {
                generator: "g".into(),
                seed: 0,
            },
            turns: 0,
            completed: true,
        };
        let counts = ProvenanceCounts::from_traces(&[live, synth]);
        assert_eq!(counts.live_traces, 1);
        assert_eq!(counts.synthetic, 1);
        assert_eq!(counts.public_dataset, 0);
        assert_eq!(counts.total(), 2);
    }
}
