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
//! All demos are **fixture-only** — no live API calls are made.  Every demo
//! can be reproduced from a clean checkout.

use anyhow::Result;
use chrono::Local;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod demo;

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
