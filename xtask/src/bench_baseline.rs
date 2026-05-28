//! E4.7 — Benchmark regression gate.
//!
//! Parses Criterion's `--output-format bencher` output and compares it against
//! checked-in JSON baseline files.  A measurement fails only when it exceeds
//! **both** the configured percentage threshold AND an absolute noise floor,
//! preventing sub-noise-floor measurements from failing on jitter alone.
//!
//! # Subcommands
//!
//! ```text
//! cargo xtask bench-baseline check  --crate scheduler --input bench-scheduler.txt
//! cargo xtask bench-baseline update --crate scheduler --input bench-scheduler.txt
//! ```

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// CLI types (re-exported and consumed from main.rs)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, clap::Args)]
pub struct BenchBaselineArgs {
    #[command(subcommand)]
    pub action: BenchBaselineAction,
}

#[derive(Debug, Clone, clap::Subcommand)]
pub enum BenchBaselineAction {
    /// Compare current benchmark output against the checked-in baseline.
    Check(CheckArgs),
    /// Capture a new baseline from benchmark output.
    Update(UpdateArgs),
}

#[derive(Debug, Clone, clap::Args)]
pub struct CheckArgs {
    /// Crate name used to locate `bench/baselines/<crate>.json`.
    #[arg(long)]
    pub crate_name: String,
    /// Path to the bencher-format benchmark output file.
    #[arg(long)]
    pub input: PathBuf,
    /// Root of the workspace (default: current directory).
    #[arg(long)]
    pub workspace_root: Option<PathBuf>,
}

#[derive(Debug, Clone, clap::Args)]
pub struct UpdateArgs {
    /// Crate name used to locate `bench/baselines/<crate>.json`.
    #[arg(long)]
    pub crate_name: String,
    /// Path to the bencher-format benchmark output file.
    #[arg(long)]
    pub input: PathBuf,
    /// Root of the workspace (default: current directory).
    #[arg(long)]
    pub workspace_root: Option<PathBuf>,
    /// Regression threshold percentage to record in the baseline (default 20.0).
    #[arg(long, default_value = "20.0")]
    pub regression_threshold_pct: f64,
    /// Absolute noise floor in nanoseconds (default 100).
    #[arg(long, default_value = "100")]
    pub noise_floor_ns: u64,
}

// ---------------------------------------------------------------------------
// Baseline schema
// ---------------------------------------------------------------------------

/// Top-level baseline file for one crate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrateBaseline {
    /// ISO-8601 timestamp when this baseline was captured.
    pub captured_at: String,
    /// Maximum allowed regression as a fraction of the baseline measurement
    /// (e.g. 0.20 = 20 %).  Only applied when the absolute delta is also
    /// above `noise_floor_ns`.
    pub regression_threshold_pct: f64,
    /// Absolute noise floor in nanoseconds.  A measurement is only flagged as
    /// a regression when it exceeds the baseline by **both** this amount
    /// **and** `regression_threshold_pct`.
    pub noise_floor_ns: u64,
    /// Per-benchmark baseline measurements in nanoseconds per iteration.
    pub benchmarks: HashMap<String, u64>,
}

// ---------------------------------------------------------------------------
// Bencher-format parser
// ---------------------------------------------------------------------------

/// A single parsed benchmark result.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchResult {
    pub name: String,
    pub ns_per_iter: u64,
    pub variance_ns: u64,
}

/// Parse Criterion's `--output-format bencher` output.
///
/// Expected line format:
/// ```text
/// test <name> ... bench: <n> ns/iter (+/- <n>)
/// ```
/// Lines that do not match this pattern are silently skipped.
pub fn parse_bencher_output(input: &str) -> Vec<BenchResult> {
    let mut results = Vec::new();

    for line in input.lines() {
        let line = line.trim();
        // Must start with "test " and contain "bench:"
        if !line.starts_with("test ") || !line.contains("bench:") {
            continue;
        }

        // Extract the test name: between "test " and " ... bench:"
        let after_test = &line["test ".len()..];
        let bench_pos = match after_test.find("... bench:") {
            Some(p) => p,
            None => continue,
        };
        let name = after_test[..bench_pos].trim().to_string();

        // Extract "N ns/iter (+/- M)" from after "bench:"
        let after_bench = &after_test[bench_pos + "... bench:".len()..]
            .trim()
            .to_string();
        // after_bench looks like: "1234 ns/iter (+/- 56)"
        let ns_part = after_bench.split("ns/iter").next().unwrap_or("").trim();
        let ns_per_iter: u64 = match ns_part.replace([',', '_'], "").parse() {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Extract variance from "(+/- N)"
        let variance_ns = if let Some(start) = after_bench.find("(+/-") {
            let var_str = &after_bench[start + 4..];
            let var_str = var_str.trim_start().trim_end_matches(')').trim();
            var_str.replace([',', '_'], "").parse().unwrap_or(0)
        } else {
            0
        };

        results.push(BenchResult {
            name,
            ns_per_iter,
            variance_ns,
        });
    }

    results
}

// ---------------------------------------------------------------------------
// Regression check
// ---------------------------------------------------------------------------

/// Result of comparing a single benchmark against its baseline.
#[derive(Debug)]
pub struct ComparisonResult {
    pub name: String,
    pub baseline_ns: u64,
    pub current_ns: u64,
    pub regression_pct: f64,
    pub is_regression: bool,
    /// The measurement is considered noise if the absolute delta < noise_floor_ns.
    pub is_noise: bool,
}

/// Compare a set of results against a baseline.
///
/// Returns `(comparisons, regressions)` where `regressions` is the count of
/// benchmarks that failed both the percentage and noise-floor gates.
pub fn check_against_baseline(
    results: &[BenchResult],
    baseline: &CrateBaseline,
) -> (Vec<ComparisonResult>, usize) {
    let mut comparisons = Vec::new();
    let mut regression_count = 0;

    for result in results {
        if let Some(&baseline_ns) = baseline.benchmarks.get(&result.name) {
            let current_ns = result.ns_per_iter;
            let delta = current_ns.saturating_sub(baseline_ns) as f64;
            let regression_pct = if baseline_ns > 0 {
                delta / baseline_ns as f64 * 100.0
            } else {
                0.0
            };

            let exceeds_pct = regression_pct > baseline.regression_threshold_pct;
            let exceeds_noise = delta as u64 > baseline.noise_floor_ns;
            let is_regression = exceeds_pct && exceeds_noise;
            let is_noise = !exceeds_noise;

            if is_regression {
                regression_count += 1;
            }

            comparisons.push(ComparisonResult {
                name: result.name.clone(),
                baseline_ns,
                current_ns,
                regression_pct,
                is_regression,
                is_noise,
            });
        }
        // Benchmarks not in the baseline are ignored (new benchmarks don't fail).
    }

    (comparisons, regression_count)
}

// ---------------------------------------------------------------------------
// Entrypoints
// ---------------------------------------------------------------------------

fn baseline_path(workspace_root: &Path, crate_name: &str) -> PathBuf {
    workspace_root
        .join("bench")
        .join("baselines")
        .join(format!("{crate_name}.json"))
}

fn workspace_root(opt: Option<PathBuf>) -> PathBuf {
    opt.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

pub fn run_check(args: CheckArgs) -> Result<()> {
    let root = workspace_root(args.workspace_root);
    let path = baseline_path(&root, &args.crate_name);

    let baseline_json = fs::read_to_string(&path)
        .with_context(|| format!("reading baseline {}", path.display()))?;
    let baseline: CrateBaseline =
        serde_json::from_str(&baseline_json).context("parsing baseline JSON")?;

    let input = fs::read_to_string(&args.input)
        .with_context(|| format!("reading input {}", args.input.display()))?;
    let results = parse_bencher_output(&input);

    if results.is_empty() {
        anyhow::bail!(
            "No benchmark results parsed from {} — verify that `cargo bench` was \
             invoked with `--output-format bencher` and produced output.",
            args.input.display()
        );
    }

    let (comparisons, regression_count) = check_against_baseline(&results, &baseline);

    println!("Benchmark regression check: {}", args.crate_name);
    println!(
        "  Baseline: {}  |  threshold: {:.0}%  |  noise floor: {} ns",
        baseline.captured_at, baseline.regression_threshold_pct, baseline.noise_floor_ns
    );
    println!();

    for c in &comparisons {
        let status = if c.is_regression {
            "❌ REGRESSION"
        } else if c.is_noise {
            "✅ noise"
        } else if c.regression_pct > 0.0 {
            "✅ within threshold"
        } else {
            "✅ improved"
        };
        println!(
            "  {:60}  {:>8} ns  (baseline {:>8} ns, {:+.1}%)  {}",
            c.name, c.current_ns, c.baseline_ns, c.regression_pct, status
        );
    }

    println!();
    if regression_count > 0 {
        bail!(
            "{} regression(s) detected for crate '{}'. \
             Run `cargo xtask bench-baseline update --crate {}` to accept new baselines.",
            regression_count,
            args.crate_name,
            args.crate_name,
        );
    } else {
        println!("✅ No regressions detected for '{}'.", args.crate_name);
    }

    Ok(())
}

pub fn run_update(args: UpdateArgs) -> Result<()> {
    let root = workspace_root(args.workspace_root);
    let path = baseline_path(&root, &args.crate_name);

    let input = fs::read_to_string(&args.input)
        .with_context(|| format!("reading input {}", args.input.display()))?;
    let results = parse_bencher_output(&input);

    if results.is_empty() {
        bail!(
            "No benchmark results parsed from {} — cannot update baseline.",
            args.input.display()
        );
    }

    let benchmarks: HashMap<String, u64> = results
        .into_iter()
        .map(|r| (r.name, r.ns_per_iter))
        .collect();

    let baseline = CrateBaseline {
        captured_at: chrono::Utc::now().to_rfc3339(),
        regression_threshold_pct: args.regression_threshold_pct,
        noise_floor_ns: args.noise_floor_ns,
        benchmarks,
    };

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(&baseline).context("serialising baseline")?;
    fs::write(&path, json).with_context(|| format!("writing baseline {}", path.display()))?;

    println!(
        "✅ Baseline updated for '{}' ({} benchmarks) → {}",
        args.crate_name,
        baseline.benchmarks.len(),
        path.display()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_BENCHER_OUTPUT: &str = r#"
test task_agenda/push/100    ... bench:       1_234 ns/iter (+/-  56)
test task_agenda/push/1000   ... bench:      12_345 ns/iter (+/- 500)
test token_pipe/push_refund_cycle/64 ... bench:         89 ns/iter (+/-  10)
test arc_cache/sequential_inserts/64 ... bench:      5_678 ns/iter (+/- 200)
# comment line — should be ignored
some random line without bench output
"#;

    #[test]
    fn parses_bencher_output_correctly() {
        let results = parse_bencher_output(SAMPLE_BENCHER_OUTPUT);
        assert_eq!(results.len(), 4);
        assert_eq!(results[0].name, "task_agenda/push/100");
        assert_eq!(results[0].ns_per_iter, 1234);
        assert_eq!(results[0].variance_ns, 56);
        assert_eq!(results[1].name, "task_agenda/push/1000");
        assert_eq!(results[1].ns_per_iter, 12345);
    }

    #[test]
    fn empty_input_yields_no_results() {
        assert!(parse_bencher_output("").is_empty());
        assert!(parse_bencher_output("# just a comment\n").is_empty());
    }

    #[test]
    fn regression_detected_when_both_gates_exceeded() {
        let mut benchmarks = HashMap::new();
        benchmarks.insert("bench_a".to_string(), 1000u64);

        let baseline = CrateBaseline {
            captured_at: "2026-01-01T00:00:00Z".to_string(),
            regression_threshold_pct: 20.0,
            noise_floor_ns: 100,
            benchmarks,
        };

        // 1300 ns is +30% above 1000 ns and +300 ns above noise floor → regression
        let results = vec![BenchResult {
            name: "bench_a".to_string(),
            ns_per_iter: 1300,
            variance_ns: 50,
        }];

        let (comparisons, regression_count) = check_against_baseline(&results, &baseline);
        assert_eq!(regression_count, 1);
        assert!(comparisons[0].is_regression);
    }

    #[test]
    fn noise_floor_gate_prevents_false_positive() {
        let mut benchmarks = HashMap::new();
        benchmarks.insert("bench_tiny".to_string(), 50u64);

        let baseline = CrateBaseline {
            captured_at: "2026-01-01T00:00:00Z".to_string(),
            regression_threshold_pct: 20.0,
            noise_floor_ns: 100,
            benchmarks,
        };

        // 80 ns is +60% above 50 ns but only +30 ns above baseline → within noise floor
        let results = vec![BenchResult {
            name: "bench_tiny".to_string(),
            ns_per_iter: 80,
            variance_ns: 5,
        }];

        let (comparisons, regression_count) = check_against_baseline(&results, &baseline);
        assert_eq!(regression_count, 0);
        assert!(comparisons[0].is_noise);
    }

    #[test]
    fn pct_threshold_gate_prevents_large_absolute_but_small_relative_change() {
        let mut benchmarks = HashMap::new();
        benchmarks.insert("bench_slow".to_string(), 1_000_000u64);

        let baseline = CrateBaseline {
            captured_at: "2026-01-01T00:00:00Z".to_string(),
            regression_threshold_pct: 20.0,
            noise_floor_ns: 100,
            benchmarks,
        };

        // 1_010_000 ns is only +1% above 1_000_000 ns, well under 20% threshold
        let results = vec![BenchResult {
            name: "bench_slow".to_string(),
            ns_per_iter: 1_010_000,
            variance_ns: 1000,
        }];

        let (comparisons, regression_count) = check_against_baseline(&results, &baseline);
        assert_eq!(regression_count, 0);
        assert!(!comparisons[0].is_regression);
    }

    #[test]
    fn benchmark_not_in_baseline_is_ignored() {
        let baseline = CrateBaseline {
            captured_at: "2026-01-01T00:00:00Z".to_string(),
            regression_threshold_pct: 20.0,
            noise_floor_ns: 100,
            benchmarks: HashMap::new(),
        };

        let results = vec![BenchResult {
            name: "new_bench".to_string(),
            ns_per_iter: 9999,
            variance_ns: 100,
        }];

        let (comparisons, regression_count) = check_against_baseline(&results, &baseline);
        assert_eq!(regression_count, 0);
        assert!(comparisons.is_empty());
    }

    #[test]
    fn improvement_is_not_flagged_as_regression() {
        let mut benchmarks = HashMap::new();
        benchmarks.insert("bench_b".to_string(), 1000u64);

        let baseline = CrateBaseline {
            captured_at: "2026-01-01T00:00:00Z".to_string(),
            regression_threshold_pct: 20.0,
            noise_floor_ns: 100,
            benchmarks,
        };

        let results = vec![BenchResult {
            name: "bench_b".to_string(),
            ns_per_iter: 500, // 50% faster
            variance_ns: 20,
        }];

        let (_, regression_count) = check_against_baseline(&results, &baseline);
        assert_eq!(regression_count, 0);
    }
}
