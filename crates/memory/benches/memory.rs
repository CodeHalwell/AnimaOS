//! Performance regression benchmarks for the `memory` crate.
//!
//! Covers the hierarchical memory subsystem hot paths:
//!
//! | Group           | What is measured                                         |
//! |-----------------|----------------------------------------------------------|
//! | `arc_cache`     | ARC cache insert throughput and mixed get/insert workload|
//! | `l1_vcm`        | L1 block-occupancy queries on [`VirtualContextManager`]  |
//! | `memory_node`   | Emotionally modulated exponential decay evaluation       |
//! | `pruning`       | L1 single-pass activation-threshold pruning              |
//!
//! Run with:
//! ```
//! cargo bench -p memory
//! ```

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use memory::{ArcCache, MemoryNode, VirtualContextManager, DEFAULT_BLOCK_SIZE};
use std::hint::black_box;

// ── ArcCache benchmarks ───────────────────────────────────────────────────────

/// Measure the cost of inserting 2× capacity items into a fresh ARC cache.
///
/// This exercises the full ARC miss path including eviction from T1/T2 and
/// ghost-list management — the write-heaviest workload for the cache.
fn bench_arc_cache_sequential_inserts(c: &mut Criterion) {
    let mut group = c.benchmark_group("arc_cache");
    for capacity in [64usize, 256, 1_024] {
        group.bench_with_input(
            BenchmarkId::new("sequential_inserts", capacity),
            &capacity,
            |b, &capacity| {
                b.iter(|| {
                    let cache: ArcCache<u64, MemoryNode> = ArcCache::new(capacity);
                    // Insert 2× capacity to exercise eviction pressure.
                    for i in 0..(capacity * 2) as u64 {
                        cache.insert(i, MemoryNode::new(1.0, 0.1));
                    }
                    black_box(cache.len())
                });
            },
        );
    }
    group.finish();
}

/// Measure a realistic mixed get / insert workload against a pre-filled cache.
///
/// Pattern: read the lower half of the cache (warm hits), then write a new
/// upper half (cold inserts with eviction).  Reflects the balance between
/// L2 retrieval and L2 → L1 promotion during agent execution.
fn bench_arc_cache_mixed_workload(c: &mut Criterion) {
    let mut group = c.benchmark_group("arc_cache");
    for capacity in [64usize, 256, 1_024] {
        group.bench_with_input(
            BenchmarkId::new("mixed_workload", capacity),
            &capacity,
            |b, &capacity| {
                // Pre-fill the lower half so reads are hot.
                let cache: ArcCache<u64, MemoryNode> = ArcCache::new(capacity);
                for i in 0..(capacity / 2) as u64 {
                    cache.insert(i, MemoryNode::new(1.0, 0.1));
                }
                b.iter(|| {
                    // Warm reads — all keys are live in T1 or T2.
                    for i in 0..(capacity / 4) as u64 {
                        black_box(cache.get(&i));
                    }
                    // Cold inserts — evict LRU to make room.
                    let base = (capacity / 2) as u64;
                    for i in base..base + (capacity / 4) as u64 {
                        cache.insert(i, MemoryNode::new(0.8, 0.2));
                    }
                    black_box(cache.len())
                });
            },
        );
    }
    group.finish();
}

/// Measure read-only get throughput on a warm, fully-loaded cache.
///
/// All accesses are hits (keys in T1 or T2); the inner loop exercises the
/// `get → lock → scan_t1/t2 → promote → unlock` path.
fn bench_arc_cache_get_hits(c: &mut Criterion) {
    let mut group = c.benchmark_group("arc_cache");
    for capacity in [64usize, 256, 1_024] {
        group.bench_with_input(
            BenchmarkId::new("get_hits", capacity),
            &capacity,
            |b, &capacity| {
                let cache: ArcCache<u64, MemoryNode> = ArcCache::new(capacity);
                for i in 0..capacity as u64 {
                    cache.insert(i, MemoryNode::new(1.0, 0.05));
                }
                b.iter(|| {
                    let mut checksum = 0u64;
                    for i in 0..capacity as u64 {
                        if cache.get(&i).is_some() {
                            checksum = checksum.wrapping_add(i);
                        }
                    }
                    black_box(checksum)
                });
            },
        );
    }
    group.finish();
}

// ── VirtualContextManager (L1) benchmarks ────────────────────────────────────

/// Measure the `occupied_blocks()` query — called on every scheduler tick.
///
/// This is pure arithmetic (ceiling division) with no allocation; it should
/// compile to a handful of instructions.
fn bench_l1_vcm_occupied_blocks(c: &mut Criterion) {
    let mut group = c.benchmark_group("l1_vcm");
    // Representative token counts: sparse, typical live agent, near-full.
    for tokens in [0u32, 2_048, 4_000, 8_192] {
        group.bench_with_input(
            BenchmarkId::new("occupied_blocks", tokens),
            &tokens,
            |b, &tokens| {
                let vcm = VirtualContextManager::with_blocks(tokens, 8_192, DEFAULT_BLOCK_SIZE);
                b.iter(|| black_box(vcm.occupied_blocks()));
            },
        );
    }
    group.finish();
}

/// Measure repeated `add_tokens` calls, which update the L1 token count and
/// enforce the `max_context` ceiling.
fn bench_l1_vcm_add_tokens(c: &mut Criterion) {
    c.bench_function("l1_vcm/add_tokens", |b| {
        b.iter(|| {
            let mut vcm = VirtualContextManager::with_blocks(0, 8_192, DEFAULT_BLOCK_SIZE);
            for _ in 0..512 {
                vcm.add_tokens(black_box(16));
            }
            black_box(vcm.get_l1_token_count())
        });
    });
}

// ── MemoryNode decay benchmarks ───────────────────────────────────────────────

/// Measure the emotionally modulated exponential decay formula.
///
/// `S(t) = max(floor, S₀ · e^{−λ·t} · (1 + α·arousal + σ·surprise))`
///
/// This function is evaluated for every node in the L1 pruning pass and for
/// L2 → L3 embedding; its throughput directly bounds pruning rate.
fn bench_memory_node_activation_at(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_node");
    for t in [0.0f32, 1.0, 10.0, 100.0] {
        group.bench_with_input(
            BenchmarkId::new("activation_at", format!("t={t}")),
            &t,
            |b, &t| {
                let node = MemoryNode::new(1.0, 0.05);
                b.iter(|| black_box(node.activation_at(black_box(t))));
            },
        );
    }
    group.finish();
}

/// Measure bulk activation evaluation for a batch of nodes at a fixed time.
///
/// Simulates the inner loop of the L1 pruning pass, which iterates over all
/// live nodes and evaluates their activation before comparing with the floor.
fn bench_memory_node_activation_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_node");
    for n in [64usize, 512, 4_096] {
        group.bench_with_input(BenchmarkId::new("activation_batch", n), &n, |b, &n| {
            let nodes: Vec<MemoryNode> = (0..n)
                .map(|i| MemoryNode::new(1.0, 0.01 + (i as f32) * 0.001))
                .collect();
            b.iter(|| {
                let sum: f32 = nodes
                    .iter()
                    .map(|nd| nd.activation_at(black_box(5.0)))
                    .sum();
                black_box(sum)
            });
        });
    }
    group.finish();
}

// ── Criterion wiring ──────────────────────────────────────────────────────────

criterion_group!(
    benches,
    bench_arc_cache_sequential_inserts,
    bench_arc_cache_mixed_workload,
    bench_arc_cache_get_hits,
    bench_l1_vcm_occupied_blocks,
    bench_l1_vcm_add_tokens,
    bench_memory_node_activation_at,
    bench_memory_node_activation_batch,
);
criterion_main!(benches);
