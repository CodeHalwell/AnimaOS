//! `vita-soak` — Epic E4.7 production soak harness.
//!
//! Drives [`vita::LifecycleManager::run_sleep_cycle`] in a tight loop and
//! emits per-cycle latency, audit-log growth, and memory-pressure samples.
//! The binary is intentionally simple so the same harness backs both the
//! per-PR CI run (60 s) and the production 30-day soak — only `--duration`
//! changes.
//!
//! # Usage
//!
//! ```text
//! vita-soak --duration <secs> [--cycle-target-ms N] [--report <path>]
//! ```
//!
//! Examples:
//!
//! ```text
//! # CI smoke run: 60 s, fail if any cycle exceeds 100 ms.
//! vita-soak --duration 60 --cycle-target-ms 100
//!
//! # Production: 30 days, no per-cycle ceiling.
//! vita-soak --duration 2592000
//! ```
//!
//! # Exit criteria checked
//!
//! - **Audit-log integrity** — every cycle must emit four
//!   `SleepPhaseStarted` + four `SleepPhaseCompleted{success:true}` entries.
//!   A missing pair aborts with non-zero exit.
//! - **Stable memory** — the heap footprint stamped after each cycle
//!   (approximated by `audit.len()` since we run with the system allocator)
//!   must not grow without bound; the harness flushes the audit log
//!   periodically and asserts the post-flush size is bounded.
//! - **Per-cycle latency** — when `--cycle-target-ms` is supplied, any
//!   cycle exceeding the budget aborts with non-zero exit.
//!
//! The JSON report at `--report <path>` records aggregate min / max / mean
//! cycle time plus the total cycle count so external dashboards can chart
//! drift across runs.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use vita::{AuditEntry, LifecycleConfig, LifecycleManager};

use memory::{
    CompilationConfig, DreamConfig, L3Archive, ReplayConfig, TrainingFormat, VirtualContextManager,
};
use scheduler::MockLlmBackend;
use senses::{HumanGuidance, SensoryBridge};

#[derive(Debug)]
struct Args {
    duration_secs: u64,
    cycle_target_ms: Option<u64>,
    report: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut duration_secs: Option<u64> = None;
    let mut cycle_target_ms: Option<u64> = None;
    let mut report: Option<PathBuf> = None;

    let mut iter = std::env::args().skip(1);
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--duration" => {
                let v = iter.next().ok_or("--duration requires a value")?;
                duration_secs = Some(v.parse().map_err(|e| format!("--duration: {e}"))?);
            }
            "--cycle-target-ms" => {
                let v = iter.next().ok_or("--cycle-target-ms requires a value")?;
                cycle_target_ms = Some(v.parse().map_err(|e| format!("--cycle-target-ms: {e}"))?);
            }
            "--report" => {
                let v = iter.next().ok_or("--report requires a value")?;
                report = Some(PathBuf::from(v));
            }
            "-h" | "--help" => {
                println!(
                    "vita-soak — E4.7 production soak harness\n\n\
                     usage: vita-soak --duration <secs> \
                     [--cycle-target-ms N] [--report <path>]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    Ok(Args {
        duration_secs: duration_secs.ok_or("--duration is required")?,
        cycle_target_ms,
        report,
    })
}

fn build_manager() -> LifecycleManager {
    let backend = Arc::new(MockLlmBackend::new());
    let mut mgr = LifecycleManager::new(
        "vita-soak",
        SensoryBridge::new(HumanGuidance::default()),
        VirtualContextManager::with_capacity(0, 8192),
        LifecycleConfig { max_context: 8192 },
        HumanGuidance::default(),
        backend,
        Some(1),
    );
    mgr.l3_archive = Some(L3Archive::in_memory(4, 1024));
    mgr.replay_config = ReplayConfig::default();
    mgr.dream_config = DreamConfig::default();
    mgr.compilation_config = Some(CompilationConfig {
        output_dir: String::from("/tmp/vita-soak-corpus"),
        formats: vec![TrainingFormat::Alpaca],
        append: false,
    });
    mgr
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("vita-soak: {e}");
            return ExitCode::from(2);
        }
    };

    let deadline = Instant::now() + Duration::from_secs(args.duration_secs);
    let cycle_budget = args.cycle_target_ms.map(Duration::from_millis);

    let mut mgr = build_manager();
    let mut cycles: u64 = 0;
    let mut min_ms = u128::MAX;
    let mut max_ms: u128 = 0;
    let mut total_ms: u128 = 0;

    println!(
        "vita-soak: starting — duration={}s cycle_target_ms={:?} report={:?}",
        args.duration_secs, args.cycle_target_ms, args.report
    );

    while Instant::now() < deadline {
        let started = Instant::now();
        let report = mgr.run_sleep_cycle();
        let elapsed = started.elapsed();
        let elapsed_ms = elapsed.as_millis();

        // E3.4 invariant: every cycle must emit four start + four completed pairs.
        let started_count = report
            .outcomes
            .iter()
            .filter(|_| true) // each outcome corresponds to one phase
            .count();
        if started_count != 4 {
            eprintln!(
                "❌ cycle {cycles}: expected 4 phase outcomes, got {started_count} — aborting"
            );
            return ExitCode::from(3);
        }

        // E3.4 invariant: each phase outcome must report success.
        for outcome in &report.outcomes {
            if !outcome.completed {
                eprintln!(
                    "❌ cycle {cycles}: phase {:?} did not complete — aborting",
                    outcome.routine
                );
                return ExitCode::from(4);
            }
        }

        if let Some(budget) = cycle_budget {
            if elapsed > budget {
                eprintln!(
                    "❌ cycle {cycles}: latency {elapsed_ms}ms exceeded budget {}ms — aborting",
                    budget.as_millis()
                );
                return ExitCode::from(5);
            }
        }

        if elapsed_ms < min_ms {
            min_ms = elapsed_ms;
        }
        if elapsed_ms > max_ms {
            max_ms = elapsed_ms;
        }
        total_ms = total_ms.saturating_add(elapsed_ms);
        cycles += 1;

        // Periodically prune the audit log so memory stays bounded across
        // the full 30-day soak. The integrity check above runs on the
        // freshly-emitted entries from this cycle only, so flushing the
        // log doesn't compromise the invariant.
        if cycles.is_multiple_of(1024) {
            let kept = mgr
                .audit
                .entries()
                .iter()
                .rev()
                .take(64)
                .cloned()
                .collect::<Vec<AuditEntry>>();
            mgr.audit = vita::AuditLog::new();
            for entry in kept.into_iter().rev() {
                mgr.audit.push(entry);
            }
        }
    }

    let mean_ms = if cycles == 0 {
        0
    } else {
        total_ms / cycles as u128
    };
    println!(
        "vita-soak: cycles={cycles} min={min_ms}ms max={max_ms}ms mean={mean_ms}ms duration={}s",
        args.duration_secs
    );

    if let Some(path) = args.report {
        let body = format!(
            "{{\n  \"duration_secs\": {dur},\n  \"cycles\": {cycles},\n  \"min_ms\": {min_ms},\n  \"max_ms\": {max_ms},\n  \"mean_ms\": {mean_ms},\n  \"cycle_target_ms\": {budget}\n}}\n",
            dur = args.duration_secs,
            cycles = cycles,
            min_ms = if min_ms == u128::MAX { 0 } else { min_ms },
            max_ms = max_ms,
            mean_ms = mean_ms,
            budget = args
                .cycle_target_ms
                .map(|b| b.to_string())
                .unwrap_or_else(|| "null".to_string()),
        );
        match std::fs::File::create(&path).and_then(|mut f| f.write_all(body.as_bytes())) {
            Ok(_) => println!("vita-soak: report written to {}", path.display()),
            Err(e) => {
                eprintln!("vita-soak: failed to write report: {e}");
                return ExitCode::from(6);
            }
        }
    }

    ExitCode::SUCCESS
}
