//! E4.5 — Stage-3 sleep cycle running inside the microVM kernel.
//!
//! This module satisfies Epic E4.5's exit criterion:
//!
//! > The Stage 3 sleep-cycle soak passes inside the microVM target.
//!
//! It wires the no_std + alloc ports of every higher crate
//! (`anima-self`, `interoception`, `memory`, `praxis`, `scheduler`,
//! `senses`, `vita`) into the UEFI kernel and runs one complete
//! [`vita::LifecycleManager::run_sleep_cycle`] pass:
//!
//! 1. **MemoryPruning** — `L1PruningStore::run_pruning_pass_with` against
//!    a pre-populated L1 with three episodic nodes.
//! 2. **GenerativeReplay** — `memory::run_replay_validation` against the
//!    in-memory L3 archive.
//! 3. **DreamExploration** — `memory::run_dream_walk` over the same L3.
//! 4. **PolicyCompilation** — `memory::compile_traces_to_pairs` over the
//!    audit-log trace (file output is automatically a no-op without `std`).
//!
//! Output (written to COM1):
//!
//! ```text
//! [E4.5] populating L1 / L3 …
//! [E4.5] sleep cycle: pruning=N replay=Y dream=E compilation=P
//! E4.5_SLEEP_DONE: …
//! ```
//!
//! All persistence paths are in-memory because the microVM has no
//! filesystem; `L3Archive::in_memory` is the only constructor reachable
//! without the `std` feature, and `CompilationConfig::output_dir` is
//! present but unused — the file-output branch is `#[cfg(feature = "std")]`
//! gated inside `memory::compile_traces_to_pairs`.

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use core::fmt::Write;

use memory::{
    archival::archive_memory_node, decay::EmotionalContext, ArchivedItem, AssociativeEdge,
    CompilationConfig, DreamConfig, L3Archive, MemoryNode, Provenance, ReplayConfig, SourceTier,
    TrainingFormat, VirtualContextManager,
};
use scheduler::MockLlmBackend;
use senses::{HumanGuidance, SensoryBridge};
use vita::{LifecycleConfig, LifecycleManager};

use crate::serial_write;

/// Build and run one complete Stage-3 sleep cycle inside the microVM kernel.
pub fn run_microvm_sleep_cycle() {
    serial_write("[E4.5] populating L1 + L3 …\n");

    // ── L3 archive: 3 entries with distinct embeddings ───────────────
    let mut l3 = L3Archive::in_memory(4, 64);
    for (id, key, emb) in [
        (1u64, "alpha", [1.0f32, 0.0, 0.0, 0.0]),
        (2u64, "beta", [0.0f32, 1.0, 0.0, 0.0]),
        (3u64, "gamma", [0.0f32, 0.0, 1.0, 0.0]),
    ] {
        let item = ArchivedItem {
            id,
            embedding: emb.to_vec(),
            payload: vec![0u8; 20], // matches replay's expected encoding
        };
        let prov = Provenance::at_ns(SourceTier::L1, key, 1_000_000_000u64 + id);
        let _ = l3.demote(item, prov);
    }

    // ── LifecycleManager wired with mock backend ───────────────────────
    let backend = Arc::new(MockLlmBackend::new());
    let mut mgr = LifecycleManager::new(
        "microvm-agent",
        SensoryBridge::new(HumanGuidance::default()),
        VirtualContextManager::with_capacity(0, 8192),
        LifecycleConfig { max_context: 8192 },
        HumanGuidance::default(),
        backend,
        Some(1),
    );

    // Pre-populate L1 with three episodic nodes that the pruning phase
    // can evict deterministically (lambda is large so they decay quickly).
    for key in ["mem-1", "mem-2", "mem-3"] {
        let mut node = MemoryNode::new(0.9, 0.5);
        node.emotion = EmotionalContext {
            arousal: 0.3,
            surprise: 0.2,
        };
        mgr.l1_memory.insert(key, node);
    }
    mgr.pruning_elapsed = 10.0; // force deep decay on this pass

    // Install configs for every phase so all four routines have work.
    mgr.l3_archive = Some(l3);
    mgr.replay_config = ReplayConfig::default();
    mgr.dream_config = DreamConfig::default();
    mgr.compilation_config = Some(CompilationConfig {
        // unused under no_std — file output is gated behind `std`.
        output_dir: String::from("training_corpus"),
        formats: vec![TrainingFormat::Alpaca],
        append: false,
    });

    // ── Run one full sleep cycle ──────────────────────────────────────
    let report = mgr.run_sleep_cycle();

    let pruned = report
        .outcomes
        .first()
        .and_then(|o| o.pruning.as_ref())
        .map(|p| p.nodes_removed)
        .unwrap_or(0);
    let replay = report
        .outcomes
        .get(1)
        .and_then(|o| o.replay.as_ref())
        .map(|r| r.queries_run)
        .unwrap_or(0);
    let dream = report
        .outcomes
        .get(2)
        .and_then(|o| o.dream.as_ref())
        .map(|d| d.candidates_found)
        .unwrap_or(0);
    let compiled = report
        .outcomes
        .get(3)
        .and_then(|o| o.compilation.as_ref())
        .map(|c| c.pairs_compiled)
        .unwrap_or(0);

    let mut line = heapless_line();
    let _ = writeln!(
        &mut line,
        "[E4.5] sleep cycle: pruning={pruned} replay={replay} dream={dream} compilation={compiled}",
    );
    serial_write(&line);

    // Quick sanity probes that exercise types from every higher crate so
    // a regression in any of them (no_std build drift) fails loudly here.
    let _ = AssociativeEdge {
        from_key: String::from("a"),
        to_key: String::from("b"),
        similarity: 1.0,
    };
    let _ = archive_memory_node(42, "probe", &MemoryNode::new(0.5, 0.1));
    serial_write("[E4.5] no_std type smoke checks ok\n");

    // The four phases must each emit a SleepPhaseStarted + SleepPhaseCompleted
    // pair into the audit log; assert that here so a soak run inside the
    // microVM panics loudly on regression.
    let started = mgr
        .audit
        .entries()
        .iter()
        .filter(|e| matches!(e, vita::AuditEntry::SleepPhaseStarted { .. }))
        .count();
    let completed = mgr
        .audit
        .entries()
        .iter()
        .filter(|e| matches!(e, vita::AuditEntry::SleepPhaseCompleted { .. }))
        .count();
    assert!(
        started >= 4,
        "expected at least 4 SleepPhaseStarted entries"
    );
    assert!(
        completed >= 4,
        "expected at least 4 SleepPhaseCompleted entries"
    );

    let mut line = heapless_line();
    let _ = writeln!(
        &mut line,
        "[E4.5] audit entries: started={started} completed={completed}",
    );
    serial_write(&line);

    // Discard `_unused` to silence dead-code lints when consumers don't
    // route the format-only payload anywhere.
    let _ = format!("{:?}", report.outcomes.first().map(|o| o.routine));
}

/// 256-byte heap-backed line buffer for the formatted COM1 messages above.
///
/// The kernel's BumpAllocator is fine for short-lived `String`s, but using a
/// fixed-cap `String::with_capacity` keeps allocations predictable and reads
/// well in the serial log when the buffer is reused across iterations.
fn heapless_line() -> String {
    String::with_capacity(256)
}
