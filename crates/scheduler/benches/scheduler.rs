//! Performance regression benchmarks for the `scheduler` crate.
//!
//! Covers the hot paths exercised on every reflex-loop iteration:
//!
//! | Group            | What is measured                                        |
//! |------------------|---------------------------------------------------------|
//! | `task_agenda`    | Push and priority-ordered select on [`TaskAgenda`]      |
//! | `mlfq`           | Starvation-boost check and bulk tier promotion           |
//! | `token_pipe`     | Credit push / refund cycle on [`BoundedTokenPipe`]      |
//!
//! Run with:
//! ```
//! cargo bench -p scheduler
//! ```

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use scheduler::{BoundedTokenPipe, IterationAwareMlfq, Task, TaskAgenda};

// ── TaskAgenda benchmarks ─────────────────────────────────────────────────────

/// Measure the cost of pushing N tasks spread across all three priority tiers.
fn bench_task_agenda_push(c: &mut Criterion) {
    let mut group = c.benchmark_group("task_agenda");
    for n in [100usize, 1_000, 10_000] {
        group.bench_with_input(BenchmarkId::new("push", n), &n, |b, &n| {
            b.iter(|| {
                let mut agenda = TaskAgenda::new();
                for i in 0..n {
                    let tier = (i % 3) as u8;
                    agenda.push(Task::new(i as u64, tier, "bench payload"));
                }
                black_box(agenda.len())
            });
        });
    }
    group.finish();
}

/// Measure priority-ordered pop (select_optimal_task) over a pre-filled agenda.
///
/// Setup cost (filling the agenda) is excluded from the timed region.
fn bench_task_agenda_select(c: &mut Criterion) {
    let mut group = c.benchmark_group("task_agenda");
    for n in [100usize, 1_000, 10_000] {
        group.bench_with_input(BenchmarkId::new("select", n), &n, |b, &n| {
            // Build a template once; clone it inside the timed region so each
            // iteration starts from a freshly-filled agenda.
            let mut template = TaskAgenda::new();
            for i in 0..n {
                let tier = (i % 3) as u8;
                template.push(Task::new(i as u64, tier, "bench payload"));
            }
            b.iter_batched(
                || template.clone(),
                |mut agenda| {
                    let mut checksum = 0u64;
                    while let Some(task) = agenda.select_optimal_task() {
                        checksum = checksum.wrapping_add(task.id);
                    }
                    black_box(checksum)
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

// ── IterationAwareMlfq benchmarks ─────────────────────────────────────────────

/// Measure the bulk-promotion path: move N medium/low tasks to the High tier.
///
/// This is the write-heavy side of starvation prevention.
fn bench_mlfq_boost_all_to_high(c: &mut Criterion) {
    let mut group = c.benchmark_group("mlfq");
    for n in [50usize, 500, 2_000] {
        group.bench_with_input(BenchmarkId::new("boost_all_to_high", n), &n, |b, &n| {
            let mut template = TaskAgenda::new();
            for i in 0..n {
                let tier = if i % 2 == 0 { 1u8 } else { 2u8 };
                template.push(Task::new(i as u64, tier, "bench"));
            }
            b.iter_batched(
                || template.clone(),
                |mut agenda| black_box(agenda.boost_all_to_high()),
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

/// Measure the common no-op path of `check_and_boost`:
/// boost_interval is set but the dispatch counter is 0, so no promotion occurs.
///
/// This path executes on every scheduler tick when the boost threshold has not
/// been reached — it must remain negligible.
fn bench_mlfq_check_and_boost_no_op(c: &mut Criterion) {
    c.bench_function("mlfq/check_and_boost_no_op", |b| {
        let mut sched = IterationAwareMlfq::with_boost_interval(100);
        let mut agenda = TaskAgenda::new();
        for i in 0..200u64 {
            agenda.push(Task::new(i, 1, "bench"));
        }
        b.iter(|| black_box(sched.check_and_boost(&mut agenda)));
    });
}

// ── BoundedTokenPipe benchmarks ───────────────────────────────────────────────

/// Measure a complete push / refund cycle at various capacities.
///
/// The inner loop pushes one credit at a time and then refunds it, exercising
/// the credit-accounting arithmetic on both producer and consumer paths.
fn bench_token_pipe_push_refund_cycle(c: &mut Criterion) {
    let mut group = c.benchmark_group("token_pipe");
    for capacity in [64u32, 512, 4_096] {
        group.bench_with_input(
            BenchmarkId::new("push_refund_cycle", capacity),
            &capacity,
            |b, &capacity| {
                b.iter(|| {
                    let mut pipe = BoundedTokenPipe::new(capacity);
                    for _ in 0..capacity {
                        pipe.push(1).ok();
                    }
                    for _ in 0..capacity {
                        pipe.refund(1).ok();
                    }
                    black_box(pipe.available_credits())
                });
            },
        );
    }
    group.finish();
}

/// Measure the hot path where credits are available: push N tokens in one call.
///
/// Simulates the producer side sending a burst of tokens when the consumer is
/// keeping up (credits are abundant).
fn bench_token_pipe_bulk_push(c: &mut Criterion) {
    let mut group = c.benchmark_group("token_pipe");
    for n in [8u32, 64, 256] {
        group.bench_with_input(BenchmarkId::new("bulk_push", n), &n, |b, &n| {
            b.iter(|| {
                let mut pipe = BoundedTokenPipe::new(n * 10);
                for _ in 0..10 {
                    pipe.push(n).ok();
                    pipe.refund(n).ok();
                }
                black_box(pipe.produced())
            });
        });
    }
    group.finish();
}

// ── Criterion wiring ──────────────────────────────────────────────────────────

criterion_group!(
    benches,
    bench_task_agenda_push,
    bench_task_agenda_select,
    bench_mlfq_boost_all_to_high,
    bench_mlfq_check_and_boost_no_op,
    bench_token_pipe_push_refund_cycle,
    bench_token_pipe_bulk_push,
);
criterion_main!(benches);
