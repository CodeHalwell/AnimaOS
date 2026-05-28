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

use anyhow::Result;
use chrono::Local;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod bench_baseline;
mod demo;
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
    /// E4.7 — Gate a benchmark run against a checked-in baseline, or refresh it.
    BenchBaseline(BenchBaselineArgs),
    /// E4.7 — Drive a long-running microVM soak in QEMU with checkpointing.
    Soak(SoakArgs),
}

#[derive(clap::Args, Debug)]
struct BenchBaselineArgs {
    #[command(subcommand)]
    op: BenchBaselineOp,
}

#[derive(Subcommand, Debug)]
enum BenchBaselineOp {
    /// Compare the current bencher log against the baseline; exit non-zero on regression.
    Check {
        /// Crate name (must match the baseline file's `crate_name` field).
        #[arg(long = "crate", value_name = "NAME")]
        crate_name: String,
        /// Path to a `--output-format bencher` log produced by `cargo bench`.
        #[arg(long)]
        input: PathBuf,
        /// Path to the checked-in baseline JSON.
        #[arg(long)]
        baseline: PathBuf,
        /// Print the report but exit 0 even on regression.  Used while CI-side
        /// baselines are still being calibrated against shared-runner jitter
        /// (the checked-in baselines are captured on the maintainer's host).
        #[arg(long)]
        warn_only: bool,
    },
    /// Overwrite the baseline with the measurements from the current bencher log.
    Update {
        /// Crate name; written into the baseline's `crate_name` field.
        #[arg(long = "crate", value_name = "NAME")]
        crate_name: String,
        /// Path to a `--output-format bencher` log produced by `cargo bench`.
        #[arg(long)]
        input: PathBuf,
        /// Path of the baseline JSON to write.
        #[arg(long)]
        output: PathBuf,
        /// Per-bench regression threshold (percent).  Default: 20 %.
        #[arg(long, default_value_t = 20.0)]
        threshold_pct: f64,
    },
}

#[derive(clap::Args, Debug)]
struct SoakArgs {
    /// Total runtime budget in hours.  Use `720` for the E4.7 30-day soak.
    #[arg(long, default_value_t = 1.0)]
    hours: f64,

    /// Path to the release EFI image.  If absent the soak runs in `dry-run`
    /// mode and only emits the schedule — useful for CI smoke tests.
    #[arg(long)]
    efi: Option<PathBuf>,

    /// Where to write checkpoints, the run manifest, and per-iteration logs.
    #[arg(long, default_value = "artifacts/soak")]
    output: PathBuf,

    /// Maximum seconds to allow a single boot+soak iteration before classifying
    /// the outcome as `Timeout`.  An early QEMU exit (crash / panic) is always
    /// classified as `UnscheduledExit` regardless of this value.  Default: 60 s.
    #[arg(long, default_value_t = 60)]
    iteration_timeout_s: u64,

    /// Path to OVMF_CODE.fd.  If absent we look in the standard apt locations.
    #[arg(long)]
    ovmf: Option<PathBuf>,
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
        Commands::BenchBaseline(args) => match args.op {
            BenchBaselineOp::Check {
                crate_name,
                input,
                baseline,
                warn_only,
            } => bench_baseline::run_check(&input, &baseline, &crate_name, warn_only),
            BenchBaselineOp::Update {
                crate_name,
                input,
                output,
                threshold_pct,
            } => bench_baseline::run_update(&input, &output, &crate_name, threshold_pct),
        },
        Commands::Soak(args) => soak::run(soak::SoakConfig {
            hours: args.hours,
            efi: args.efi,
            output: args.output,
            iteration_timeout_s: args.iteration_timeout_s,
            ovmf: args.ovmf,
        }),
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
