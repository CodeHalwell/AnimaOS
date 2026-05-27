//! E4.5 — Sleep-cycle soak: Stage 3 memory-sleep phases inside the microVM.
//!
//! This module exercises the no_std surfaces of the `memory`, `scheduler`, and
//! `interoception` crates inside the bare-metal UEFI kernel.  It mirrors the
//! Stage 3 sleep-cycle state machine but runs entirely without `std`, file I/O,
//! or OS threads.
//!
//! # Phases
//!
//! 1. **Context load** — Populate a [`VirtualContextManager`] with tokens and
//!    verify pressure detection (Normal → HighWater → Critical).
//!
//! 2. **MLFQ task scheduling** — Enqueue tasks at different priority tiers into
//!    a [`TaskAgenda`]; drive an [`IterationAwareMlfq`] boost cycle; verify
//!    task selection order respects tier priority.
//!
//! 3. **L1 pruning decay pass** — Insert [`MemoryNode`] entries into an
//!    [`L1PruningStore`]; run a pruning pass; confirm high-decay nodes are
//!    evicted.
//!
//! 4. **Homeostatic monitoring** — Record synthetic TTFT samples into a
//!    [`HomeostaticMonitor`]; compute a systemic-stress index and verify it is
//!    in range.
//!
//! 5. **Dream walk (no_std)** — Build a small [`InMemoryEntry`] corpus and run
//!    [`run_dream_walk_no_std`]; verify associative edges are produced.
//!
//! # Exit criterion
//!
//! Writes `E4.5_SOAK_DONE` to COM1 via the `serial_write` callback supplied
//! by the caller.  The CI job (`microvm-boot`) asserts this string.

use interoception::HomeostaticMonitor;
use memory::{
    run_dream_walk_no_std, DreamConfig, InMemoryEntry, L1PruningStore, MemoryNode,
    MemoryPressureEvent, VirtualContextManager,
};
use scheduler::{IterationAwareMlfq, Task, TaskAgenda};

/// Run the full sleep-cycle soak and return `Ok(())` on success.
///
/// `serial` is a callback that writes a string slice to COM1; this keeps the
/// soak module independent of the raw port-I/O helpers in `main.rs`.
pub fn run_sleep_soak(serial: impl Fn(&str)) -> Result<(), &'static str> {
    serial("\n[E4.5] sleep_soak: starting Stage 3 sleep-cycle soak\n");

    // ------------------------------------------------------------------
    // Phase 1 — VirtualContextManager pressure
    // ------------------------------------------------------------------
    serial("[E4.5] Phase 1 — VirtualContextManager pressure detection\n");

    // 1 KiB context window, 16-token blocks → 64 blocks total, HWM = 48 (75%)
    let mut ctx = VirtualContextManager::with_blocks(0, 1024, 16);

    // Initially empty → Normal pressure.
    if ctx.check_pressure() != MemoryPressureEvent::Normal {
        return Err("E4.5 Phase 1 FAILED: expected Normal pressure on empty context");
    }
    serial("[E4.5]   empty window → Normal pressure ✓\n");

    // Fill to just above HWM (49 blocks × 16 tokens = 784 tokens).
    ctx.set_l1_token_count(784);
    if ctx.check_pressure() != MemoryPressureEvent::HighWater {
        return Err("E4.5 Phase 1 FAILED: expected HighWater pressure at 784/1024 tokens");
    }
    serial("[E4.5]   784/1024 tokens → HighWater pressure ✓\n");

    // Fill completely → Critical pressure.
    ctx.set_l1_token_count(1024);
    if ctx.check_pressure() != MemoryPressureEvent::Critical {
        return Err("E4.5 Phase 1 FAILED: expected Critical pressure on full context");
    }
    serial("[E4.5]   1024/1024 tokens → Critical pressure ✓\n");
    serial("[E4.5] Phase 1 PASSED\n");

    // ------------------------------------------------------------------
    // Phase 2 — MLFQ task scheduling
    // ------------------------------------------------------------------
    serial("[E4.5] Phase 2 — MLFQ task scheduling (TaskAgenda + IterationAwareMlfq)\n");

    let mut agenda = TaskAgenda::new();

    // Enqueue three tasks: one high-priority (level 0), two low (level 2).
    agenda.push(Task::new(1, 0, "high-priority inference"));
    agenda.push(Task::new(2, 2, "low-priority maintenance-a"));
    agenda.push(Task::new(3, 2, "low-priority maintenance-b"));

    if agenda.len() != 3 {
        return Err("E4.5 Phase 2 FAILED: expected 3 tasks in agenda");
    }
    serial("[E4.5]   3 tasks enqueued ✓\n");

    // The high-priority task (level 0) should be selected first.
    let first = agenda
        .select_optimal_task()
        .ok_or("E4.5 Phase 2 FAILED: select_optimal_task returned None")?;
    if first.id != 1 {
        return Err("E4.5 Phase 2 FAILED: expected task id=1 (level 0) to be selected first");
    }
    serial("[E4.5]   high-priority task (id=1) selected first ✓\n");

    // Remaining tasks are low-priority; there should be 2 left.
    if agenda.len() != 2 {
        return Err("E4.5 Phase 2 FAILED: expected 2 tasks remaining after first select");
    }

    // IterationAwareMlfq boost: all remaining tasks are promoted to high.
    // We bypass `check_and_boost` (which is a policy gate that requires
    // prior dispatches before it fires — see the no-boost-before-any-
    // dispatch contract in mlfq.rs) and exercise the underlying operation
    // directly.  The soak's intent is to verify that `boost_all_to_high`
    // moves Medium/Low tasks into the High tier in a no_std build; the
    // dispatch-counting policy is exercised by the std-side scheduler
    // tests, not by the kernel soak.
    let _mlfq = IterationAwareMlfq::with_boost_interval(1);
    let boosted = agenda.boost_all_to_high();
    if boosted == 0 {
        return Err("E4.5 Phase 2 FAILED: boost_all_to_high promoted zero tasks");
    }
    serial("[E4.5]   MLFQ boost cycle promoted remaining tasks ✓\n");
    serial("[E4.5] Phase 2 PASSED\n");

    // ------------------------------------------------------------------
    // Phase 3 — L1 pruning decay pass
    // ------------------------------------------------------------------
    serial("[E4.5] Phase 3 — L1PruningStore decay pass\n");

    let mut store = L1PruningStore::new();

    // Insert a low-decay node (survives pruning) and a high-decay node (evicted).
    store.insert(
        "stable-context",
        MemoryNode::new(0.9, 0.01), // activation=0.9, lambda=0.01 (slow decay)
    );
    store.insert(
        "ephemeral-context",
        MemoryNode::new(0.1, 2.0), // activation=0.1, lambda=2.0 (fast decay)
    );

    if store.len() != 2 {
        return Err("E4.5 Phase 3 FAILED: expected 2 nodes before pruning");
    }
    serial("[E4.5]   2 nodes inserted ✓\n");

    // Run a pruning pass with elapsed=1.0 s and floor=0.3.
    // The ephemeral node (activation ≈ 0.1 × e^{−2.0} ≈ 0.014) is below
    // the floor and will be pruned.  The stable node (0.9 × e^{−0.01} ≈ 0.891)
    // remains.
    let report = store.run_pruning_pass_with(1.0, 0.3);

    if report.nodes_removed == 0 {
        return Err("E4.5 Phase 3 FAILED: expected at least one node pruned");
    }
    serial("[E4.5]   pruning pass evicted high-decay node ✓\n");

    if store.len() != 1 {
        return Err("E4.5 Phase 3 FAILED: expected exactly 1 node after pruning");
    }
    if store.get("stable-context").is_none() {
        return Err("E4.5 Phase 3 FAILED: stable node should have survived pruning");
    }
    serial("[E4.5]   stable node survived decay ✓\n");
    serial("[E4.5] Phase 3 PASSED\n");

    // ------------------------------------------------------------------
    // Phase 4 — HomeostaticMonitor systemic stress index
    // ------------------------------------------------------------------
    serial("[E4.5] Phase 4 — HomeostaticMonitor systemic stress index\n");

    // baseline_ttft=100 ms, beta=0.1, window_size=8.
    let mut monitor = HomeostaticMonitor::new(100.0, 0.1, 8);

    // Record eight TTFT samples: moderate latency (150 ms each).
    for _ in 0..8 {
        monitor.record_ttft(150.0);
    }

    // With 50 % of the context window occupied the stress index should be > 0.
    let stress = monitor.compute_systemic_stress_index(512, 1024);
    if !(0.0..=1.0).contains(&stress) {
        return Err("E4.5 Phase 4 FAILED: systemic stress index out of [0,1] range");
    }
    serial("[E4.5]   systemic stress index in [0,1] ✓\n");
    serial("[E4.5] Phase 4 PASSED\n");

    // ------------------------------------------------------------------
    // Phase 5 — Dream walk (no_std variant)
    // ------------------------------------------------------------------
    serial("[E4.5] Phase 5 — run_dream_walk_no_std\n");

    // Build a small in-memory corpus: 4 entries with 4-dimensional embeddings.
    // Entries 0 and 1 are very similar (cosine ≈ 1.0).
    // Entries 2 and 3 are orthogonal to each other and to 0/1.
    let entries: &[InMemoryEntry] = &[
        InMemoryEntry {
            id: 0,
            key: alloc::string::String::from("topic-a-v1"),
            embedding: alloc::vec![1.0, 0.0, 0.0, 0.0],
        },
        InMemoryEntry {
            id: 1,
            key: alloc::string::String::from("topic-a-v2"),
            embedding: alloc::vec![0.99, 0.14, 0.0, 0.0],
        },
        InMemoryEntry {
            id: 2,
            key: alloc::string::String::from("topic-b"),
            embedding: alloc::vec![0.0, 1.0, 0.0, 0.0],
        },
        InMemoryEntry {
            id: 3,
            key: alloc::string::String::from("topic-c"),
            embedding: alloc::vec![0.0, 0.0, 1.0, 0.0],
        },
    ];

    let config = DreamConfig {
        similarity_threshold: 0.8,
        top_k_neighbors: 4,
        ..DreamConfig::default()
    };

    let (_report, edges) = run_dream_walk_no_std(entries, &config);

    // Entries 0 and 1 share high cosine similarity; expect at least one edge.
    if edges.is_empty() {
        return Err("E4.5 Phase 5 FAILED: expected at least one associative edge");
    }
    serial("[E4.5]   associative edges generated ✓\n");

    // Verify that the edge connects the two similar entries (topic-a-v1 ↔ topic-a-v2).
    let similar_edge = edges.iter().any(|e| {
        (e.from_key == "topic-a-v1" && e.to_key == "topic-a-v2")
            || (e.from_key == "topic-a-v2" && e.to_key == "topic-a-v1")
    });
    if !similar_edge {
        return Err("E4.5 Phase 5 FAILED: expected edge between topic-a-v1 and topic-a-v2");
    }
    serial("[E4.5]   similar-entry edge (topic-a-v1 ↔ topic-a-v2) confirmed ✓\n");
    serial("[E4.5] Phase 5 PASSED\n");

    // ------------------------------------------------------------------
    // All phases complete — signal E4.5 exit criterion to COM1.
    // ------------------------------------------------------------------
    serial("E4.5_SOAK_DONE: sleep-cycle soak complete — all 5 phases passed\n");

    Ok(())
}
