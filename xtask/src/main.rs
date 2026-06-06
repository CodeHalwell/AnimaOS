//! AnimaOS xtask runner.
//!
//! Run with `cargo xtask <subcommand>`.
//!
//! # Available subcommands
//!
//! ## `demo` — E5.8 Kill-Shot Demonstrations
//!
//! ```
//! cargo xtask demo --kind graceful   # Demo A: graceful degradation under thermal load
//! cargo xtask demo --kind retention  # Demo B: long-horizon retention (KV-controller vs LRU)
//! cargo xtask demo --kind all        # Both demos in sequence
//! ```
//!
//! Artefacts are written to `artifacts/demos/<date>-<kind>/`.
//!
//! ## `bench-baseline` — E4.7 perf regression gate
//!
//! ```
//! cargo xtask bench-baseline check  --crate scheduler --input bench-scheduler.txt \
//!                                   --baseline bench/baselines/scheduler.json
//! cargo xtask bench-baseline update --crate scheduler --input bench-scheduler.txt \
//!                                   --output   bench/baselines/scheduler.json
//! ```
//!
//! ## `soak` — E4.7 long-running soak driver
//!
//! ```
//! cargo xtask soak --hours 720 --efi path/to/anima-microvm.efi \
//!                  --output artifacts/soak/$(date +%Y%m%d)
//! ```
//!
//! All demos are **fixture-only** — no live API calls are made.  Every demo
//! can be reproduced from a clean checkout.
//!
//! ## `bench-baseline` — E4.7 Benchmark Regression Gate
//!
//! ```
//! cargo xtask bench-baseline check  --crate-name scheduler --input bench-scheduler.txt
//! cargo xtask bench-baseline update --crate-name scheduler --input bench-scheduler.txt
//! ```
//!
//! Compares Criterion `--output-format bencher` measurements against checked-in
//! baselines under `bench/baselines/<crate>.json`.  Fails only when a measurement
//! exceeds **both** the percentage threshold and the absolute noise floor.
//!
//! ## `soak` — E4.7 MicroVM Long-Running Soak Driver
//!
//! ```
//! cargo xtask soak --efi anima-microvm.efi --iterations 5
//! cargo xtask soak --dry-run --iterations 1    # CI smoke test
//! ```
//!
//! Boots the microVM EFI image under QEMU in a loop, records per-iteration
//! outcomes, and writes a resumable checkpoint manifest.
//!
//! ## `align-eval` — E13 Alignment Evaluation Harness (S13.3)
//!
//! ```
//! cargo xtask align-eval
//! cargo xtask align-eval --threshold 0.95
//! cargo xtask align-eval --json artifacts/align-eval.json
//! ```
//!
//! Runs a labelled scenario table through the constitution check and computes
//! a value-adherence pass rate.  Exits non-zero below `--threshold`.
//!
//! ## `red-team` — E13 Red-Team Harness (S13.4)
//!
//! ```
//! cargo xtask red-team
//! cargo xtask red-team --json artifacts/redteam.json
//! ```
//!
//! Runs an adversarial probe corpus through the constitution check and asserts
//! every probe is blocked.  Any escape is a hard failure.

use anyhow::Result;
use chrono::Local;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod align_eval;
mod bench_baseline;
mod demo;
mod redteam;
mod soak;

#[derive(Parser, Debug)]
#[command(name = "xtask", about = "AnimaOS workspace automation")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run E5.8 kill-shot demonstrations and write reproducible artefact bundles.
    Demo(DemoArgs),
    /// E4.7: Compare or update benchmark baselines (regression gate).
    BenchBaseline(bench_baseline::BenchBaselineArgs),
    /// E4.7: Run the microVM soak driver (long-running boot-cycle harness).
    Soak(soak::SoakArgs),
    /// E13 S13.3: Run the alignment evaluation harness (scenario pass-rate gate).
    AlignEval(align_eval::AlignEvalArgs),
    /// E13 S13.4: Run the red-team adversarial probe harness (all-must-block gate).
    RedTeam(redteam::RedTeamArgs),
}

#[derive(clap::Args, Debug)]
struct DemoArgs {
    /// Which demo to run: `graceful`, `retention`, or `all`.
    #[arg(long, default_value = "all")]
    kind: DemoKind,

    /// Override output directory (default: `artifacts/demos/<date>-<kind>`).
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Clone, clap::ValueEnum)]
enum DemoKind {
    /// Demo A: graceful degradation under thermal load.
    Graceful,
    /// Demo B: long-horizon retention with and without the KV-cache controller.
    Retention,
    /// Run both demos in sequence.
    All,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Demo(args) => run_demo(args),
        Commands::BenchBaseline(args) => run_bench_baseline(args),
        Commands::Soak(args) => soak::run_soak(args),
        Commands::AlignEval(args) => align_eval::run_align_eval(args),
        Commands::RedTeam(args) => redteam::run_red_team(args),
    }
}

fn run_demo(args: DemoArgs) -> Result<()> {
    let date_str = Local::now().format("%Y%m%d-%H%M%S").to_string();

    match args.kind {
        DemoKind::Graceful => {
            let dir = args
                .output
                .unwrap_or_else(|| PathBuf::from(format!("artifacts/demos/{}-graceful", date_str)));
            println!("━━━ Demo A: Graceful Degradation ━━━");
            println!("  Output: {}", dir.display());
            demo::graceful::run(&dir)?;
            println!("  Done.\n");
        }
        DemoKind::Retention => {
            let dir = args.output.unwrap_or_else(|| {
                PathBuf::from(format!("artifacts/demos/{}-retention", date_str))
            });
            println!("━━━ Demo B: Long-Horizon Retention ━━━");
            println!("  Output: {}", dir.display());
            demo::retention::run(&dir)?;
            println!("  Done.\n");
        }
        DemoKind::All => {
            let graceful_dir = args
                .output
                .as_ref()
                .map(|p| p.join("graceful"))
                .unwrap_or_else(|| PathBuf::from(format!("artifacts/demos/{}-graceful", date_str)));
            let retention_dir = args
                .output
                .as_ref()
                .map(|p| p.join("retention"))
                .unwrap_or_else(|| {
                    PathBuf::from(format!("artifacts/demos/{}-retention", date_str))
                });

            println!("━━━ Demo A: Graceful Degradation ━━━");
            println!("  Output: {}", graceful_dir.display());
            demo::graceful::run(&graceful_dir)?;
            println!("  Done.\n");

            println!("━━━ Demo B: Long-Horizon Retention ━━━");
            println!("  Output: {}", retention_dir.display());
            demo::retention::run(&retention_dir)?;
            println!("  Done.\n");

            println!("All demos complete.");
            println!("  Graceful artefacts : {}", graceful_dir.display());
            println!("  Retention artefacts: {}", retention_dir.display());
        }
    }

    Ok(())
}

fn run_bench_baseline(args: bench_baseline::BenchBaselineArgs) -> Result<()> {
    match args.action {
        bench_baseline::BenchBaselineAction::Check(check_args) => {
            bench_baseline::run_check(check_args)
        }
        bench_baseline::BenchBaselineAction::Update(update_args) => {
            bench_baseline::run_update(update_args)
        }
    }
}
