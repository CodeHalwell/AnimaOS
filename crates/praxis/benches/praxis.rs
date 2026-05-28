//! Performance regression benchmarks for the `praxis` crate.
//!
//! Covers the efferent actuator hot paths:
//!
//! | Group              | What is measured                                       |
//! |--------------------|--------------------------------------------------------|
//! | `tool_registry`    | Registry lookup and synchronous tool dispatch          |
//! | `routing`          | Length-robust relative routing filter                  |
//! | `circuit_breaker`  | Record-success / record-failure in the Closed state    |
//!
//! Run with:
//! ```
//! cargo bench -p praxis
//! ```

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use praxis::{
    length_robust_filter, Bus, CircuitBreaker, ToolCandidate, ToolEnvelope, ToolRegistry,
};
use std::hint::black_box;

// ── ToolRegistry benchmarks ───────────────────────────────────────────────────

/// Measure the cost of looking up a registered tool by its string identifier.
///
/// `lookup` acquires the registry mutex, performs a `HashMap` probe, and
/// clones the `Arc<dyn ToolDriver>` — the common read path in the dispatch
/// pipeline.
fn bench_tool_registry_lookup(c: &mut Criterion) {
    let registry = ToolRegistry::new(); // pre-populated with clock, echo, text-io
    c.bench_function("tool_registry/lookup_echo", |b| {
        b.iter(|| {
            let tool = registry.lookup(black_box("echo"));
            black_box(tool.is_some())
        });
    });
}

/// Measure a miss on an unregistered identifier.
///
/// Miss path is important because the routing filter may produce candidates
/// that do not correspond to registered tools on edge-case inputs.
fn bench_tool_registry_lookup_miss(c: &mut Criterion) {
    let registry = ToolRegistry::new();
    c.bench_function("tool_registry/lookup_miss", |b| {
        b.iter(|| {
            let tool = registry.lookup(black_box("nonexistent-tool-xyz"));
            black_box(tool.is_none())
        });
    });
}

/// Measure end-to-end dispatch through the EchoTool (zero-copy passthrough).
///
/// `dispatch` locks the registry, checks the circuit-breaker state, invokes
/// the tool, and records success / failure in the breaker — the complete
/// synchronous dispatch path.
fn bench_tool_registry_dispatch_echo(c: &mut Criterion) {
    let registry = ToolRegistry::new();
    let envelope = ToolEnvelope::new(
        Bus::Mcp,
        "echo",
        b"hello world benchmark payload".to_vec(),
        0,
    );
    c.bench_function("tool_registry/dispatch_echo", |b| {
        b.iter(|| black_box(registry.dispatch(&envelope)));
    });
}

/// Measure dispatch of the ClockTool (single syscall: `SystemTime::now`).
fn bench_tool_registry_dispatch_clock(c: &mut Criterion) {
    let registry = ToolRegistry::new();
    let envelope = ToolEnvelope::new(Bus::Mcp, "clock", Vec::new(), 0);
    c.bench_function("tool_registry/dispatch_clock", |b| {
        b.iter(|| black_box(registry.dispatch(&envelope)));
    });
}

/// Measure the overhead of registering many tools and then listing them.
///
/// Used by integration tests that stress-test the registry namespace.  The
/// list call acquires the mutex and allocates a sorted `Vec<String>`.
fn bench_tool_registry_list(c: &mut Criterion) {
    let mut group = c.benchmark_group("tool_registry");
    for n in [10usize, 100, 1_000] {
        group.bench_with_input(
            BenchmarkId::new("list_after_n_registrations", n),
            &n,
            |b, &n| {
                use praxis::{ToolDriver, ToolInvocationError};

                struct NopTool(String);
                impl ToolDriver for NopTool {
                    fn id(&self) -> &'static str {
                        // SAFETY: leaking is acceptable in a short-lived benchmark binary.
                        Box::leak(self.0.clone().into_boxed_str())
                    }
                    fn schema(&self) -> &'static str {
                        "{}"
                    }
                    fn invoke(&self, _p: &[u8]) -> Result<Vec<u8>, ToolInvocationError> {
                        Ok(Vec::new())
                    }
                }

                let registry = ToolRegistry::new();
                for i in 0..n {
                    registry.register(NopTool(format!("bench-tool-{i:06}")));
                }
                b.iter(|| black_box(registry.list()));
            },
        );
    }
    group.finish();
}

// ── Routing filter benchmarks ─────────────────────────────────────────────────

/// Measure `length_robust_filter` on a small candidate set (10 tools).
///
/// This is the typical online path: the language model emits a short ranked
/// list and the filter keeps only the top-scoring candidates.
fn bench_routing_filter_small(c: &mut Criterion) {
    let candidates: Vec<ToolCandidate> = (0..10u32)
        .map(|i| ToolCandidate {
            id: format!("tool-{i:03}"),
            score: 1.0 - i as f32 * 0.05,
        })
        .collect();
    c.bench_function("routing/filter_10_candidates", |b| {
        b.iter(|| black_box(length_robust_filter(&candidates, 0.85)));
    });
}

/// Measure `length_robust_filter` across a range of candidate-list sizes.
///
/// Larger lists are produced during offline tool discovery or when the tool
/// namespace is large; the filter should remain linear in allocations.
fn bench_routing_filter_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("routing");
    for n in [50usize, 200, 1_000] {
        group.bench_with_input(BenchmarkId::new("filter_candidates", n), &n, |b, &n| {
            let candidates: Vec<ToolCandidate> = (0..n as u32)
                .map(|i| ToolCandidate {
                    id: format!("tool-{i:06}"),
                    // Linearly decreasing scores so ~15 % pass the 0.85 threshold.
                    score: 1.0 - (i as f32 / n as f32) * 0.5,
                })
                .collect();
            b.iter(|| black_box(length_robust_filter(&candidates, 0.85)));
        });
    }
    group.finish();
}

/// Measure the degenerate case where all candidates have identical scores.
///
/// All items pass the filter since threshold = max × τ ≤ every score when
/// scores are equal.  This exercises the full-length output-Vec allocation.
fn bench_routing_filter_all_equal(c: &mut Criterion) {
    let mut group = c.benchmark_group("routing");
    for n in [50usize, 200, 1_000] {
        group.bench_with_input(BenchmarkId::new("filter_all_equal", n), &n, |b, &n| {
            let candidates: Vec<ToolCandidate> = (0..n as u32)
                .map(|i| ToolCandidate {
                    id: format!("tool-{i:06}"),
                    score: 0.9, // all equal
                })
                .collect();
            b.iter(|| black_box(length_robust_filter(&candidates, 0.85)));
        });
    }
    group.finish();
}

// ── CircuitBreaker benchmarks ─────────────────────────────────────────────────

/// Measure `record_success` in the Closed state — the expected steady-state
/// path for every successful tool dispatch.
fn bench_circuit_breaker_record_success(c: &mut Criterion) {
    c.bench_function("circuit_breaker/record_success_closed", |b| {
        let mut breaker = CircuitBreaker::new();
        b.iter(|| {
            breaker.record_success();
            black_box(breaker.failure_count)
        });
    });
}

/// Measure `record_failure` up to (but not crossing) the trip threshold.
///
/// Keeps the breaker in the Closed state so we measure the accounting
/// overhead without the state-transition cost.
fn bench_circuit_breaker_record_failure_below_threshold(c: &mut Criterion) {
    use praxis::BreakerState;

    // Default open threshold in the registry is 5; use 10 here so 4 failures
    // never trip the breaker during the benchmark.
    c.bench_function("circuit_breaker/record_failure_below_threshold", |b| {
        b.iter(|| {
            let mut breaker = CircuitBreaker::new();
            for _ in 0..4 {
                breaker.record_failure(10);
            }
            black_box(breaker.state == BreakerState::Closed)
        });
    });
}

// ── Criterion wiring ──────────────────────────────────────────────────────────

criterion_group!(
    benches,
    bench_tool_registry_lookup,
    bench_tool_registry_lookup_miss,
    bench_tool_registry_dispatch_echo,
    bench_tool_registry_dispatch_clock,
    bench_tool_registry_list,
    bench_routing_filter_small,
    bench_routing_filter_scaling,
    bench_routing_filter_all_equal,
    bench_circuit_breaker_record_success,
    bench_circuit_breaker_record_failure_below_threshold,
);
criterion_main!(benches);
