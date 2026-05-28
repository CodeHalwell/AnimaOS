//! `xtask bench-baseline` — regression gate for Criterion `--output-format bencher` logs.
//!
//! # Why this exists
//!
//! Criterion's own history (`target/criterion/*`) is not version-controlled,
//! so PR runners have no baseline to compare against.  The `bench-baseline`
//! sub-tool persists a per-crate snapshot of measured throughput into
//! `bench/baselines/<crate>.json` and gates future runs against it.
//!
//! # Workflow
//!
//! 1. Capture (run locally on a clean machine, then commit the JSON):
//!    ```text
//!    cargo bench -p scheduler -- --output-format bencher | tee bench-scheduler.txt
//!    cargo xtask bench-baseline update \
//!        --crate scheduler \
//!        --input bench-scheduler.txt \
//!        --output bench/baselines/scheduler.json
//!    ```
//!
//! 2. Gate (run on every PR in `.github/workflows/bench.yml`):
//!    ```text
//!    cargo xtask bench-baseline check \
//!        --crate scheduler \
//!        --input bench-scheduler.txt \
//!        --baseline bench/baselines/scheduler.json
//!    ```
//!
//! The `check` sub-command exits non-zero if any measurement regressed by
//! more than `regression_threshold_pct` relative to the recorded baseline.
//! New measurements (not in the baseline) are reported but do not fail —
//! they are added by `update` after a maintainer reviews the run.

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// A single bench measurement parsed from Criterion's bencher format.
///
/// The bencher line looks like:
/// ```text
/// test task_agenda/push/100 ... bench:        4321 ns/iter (+/- 234)
/// ```
#[derive(Debug, Clone)]
pub struct BencherMeasurement {
    pub name: String,
    pub ns_per_iter: u64,
    pub noise: u64,
}

/// Persisted baseline for one crate.
#[derive(Debug, Serialize, Deserialize)]
pub struct Baseline {
    pub crate_name: String,
    pub captured_at: String,
    /// Per-bench regression threshold, expressed as a percentage of the
    /// recorded `ns_per_iter`.  A value of `20.0` means a measurement is
    /// allowed to grow by up to 20 % before the gate fails.
    pub regression_threshold_pct: f64,
    /// Noise floor: a regression only counts if the *absolute* delta also
    /// exceeds this many ns/iter.  Without this, sub-100 ns measurements
    /// (e.g. `bulk_push/8 = 0 ns/iter`) would trip the gate on any movement
    /// at all.  Defaults to 100 ns; serialised so it can be tuned per crate.
    #[serde(default = "default_noise_floor_ns")]
    pub noise_floor_ns: u64,
    pub measurements: BTreeMap<String, BaselineEntry>,
}

fn default_noise_floor_ns() -> u64 {
    100
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BaselineEntry {
    pub ns_per_iter: u64,
    pub noise: u64,
}

/// Parse the `--output-format bencher` form Criterion writes to stdout.
///
/// Any line that does not match the bencher format is silently skipped.
pub fn parse_bencher_output(input: &str) -> Vec<BencherMeasurement> {
    let mut out = Vec::new();
    for line in input.lines() {
        if let Some(m) = parse_one_bencher_line(line) {
            out.push(m);
        }
    }
    out
}

fn parse_one_bencher_line(line: &str) -> Option<BencherMeasurement> {
    // Expected: "test <name> ... bench:        <ns> ns/iter (+/- <noise>)"
    let trimmed = line.trim();
    let after_test = trimmed.strip_prefix("test ")?;
    let (name, after_name) = after_test.split_once(" ...")?;
    let after_bench = after_name.trim_start().strip_prefix("bench:")?;
    let after_bench = after_bench.trim_start();
    // `ns ns/iter (+/- noise)`
    let (ns_str, rest) = after_bench.split_once(" ns/iter")?;
    let ns_per_iter: u64 = ns_str.trim().replace(',', "").parse().ok()?;
    let noise: u64 = rest
        .trim()
        .strip_prefix("(+/-")?
        .trim()
        .strip_suffix(')')?
        .trim()
        .replace(',', "")
        .parse()
        .ok()?;
    Some(BencherMeasurement {
        name: name.trim().to_string(),
        ns_per_iter,
        noise,
    })
}

/// Write a fresh baseline derived from the given measurements.
pub fn write_baseline(
    crate_name: &str,
    measurements: &[BencherMeasurement],
    threshold_pct: f64,
    out_path: &Path,
) -> Result<()> {
    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let baseline = Baseline {
        crate_name: crate_name.to_string(),
        captured_at: Utc::now().to_rfc3339(),
        regression_threshold_pct: threshold_pct,
        noise_floor_ns: default_noise_floor_ns(),
        measurements: measurements
            .iter()
            .map(|m| {
                (
                    m.name.clone(),
                    BaselineEntry {
                        ns_per_iter: m.ns_per_iter,
                        noise: m.noise,
                    },
                )
            })
            .collect(),
    };
    let json = serde_json::to_string_pretty(&baseline)?;
    fs::write(out_path, format!("{json}\n"))
        .with_context(|| format!("write {}", out_path.display()))?;
    Ok(())
}

/// Result of comparing a measurement against its baseline entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    Ok,
    Regressed { delta_pct: i64 },
    New,
    Missing,
}

/// Compare each measurement against the baseline; return per-bench outcomes
/// and an overall failure flag.  Failures are: any `Regressed` outcome, OR
/// any `Missing` entry (a baseline benchmark that the current run did not
/// produce).  `New` entries warn but do not fail.
pub fn check_against_baseline(
    measurements: &[BencherMeasurement],
    baseline: &Baseline,
) -> (Vec<(String, CheckOutcome)>, bool) {
    let mut outcomes: Vec<(String, CheckOutcome)> = Vec::new();
    let mut failed = false;

    // Build a quick lookup for the current run.
    let current: BTreeMap<&str, &BencherMeasurement> =
        measurements.iter().map(|m| (m.name.as_str(), m)).collect();

    // First: every measurement we did capture vs. baseline.
    for m in measurements {
        match baseline.measurements.get(&m.name) {
            None => {
                outcomes.push((m.name.clone(), CheckOutcome::New));
            }
            Some(entry) => {
                let base = entry.ns_per_iter as i128;
                let now = m.ns_per_iter as i128;
                let delta_ns = now - base;
                let delta_pct = if base == 0 {
                    if delta_ns == 0 {
                        0.0
                    } else {
                        f64::INFINITY
                    }
                } else {
                    (delta_ns as f64 / base as f64) * 100.0
                };
                // A regression must clear BOTH gates:
                //   * percentage grew beyond the configured threshold, AND
                //   * absolute ns/iter delta exceeds the noise floor.
                // This stops sub-100 ns measurements (which dominate cheap
                // hot paths like `bulk_push/8 = 0 ns/iter`) from tripping
                // the gate on a single ns of jitter.
                let pct_exceeded = delta_pct > baseline.regression_threshold_pct;
                let abs_exceeded = delta_ns > baseline.noise_floor_ns as i128;
                if pct_exceeded && abs_exceeded {
                    failed = true;
                    // Guard against baseline=0 producing f64::INFINITY.
                    // Use i64::MAX as a sentinel rendered as "(from 0 ns/iter)".
                    let delta_pct_i64 = if delta_pct.is_finite() {
                        delta_pct.round() as i64
                    } else {
                        i64::MAX
                    };
                    outcomes.push((
                        m.name.clone(),
                        CheckOutcome::Regressed {
                            delta_pct: delta_pct_i64,
                        },
                    ));
                } else {
                    outcomes.push((m.name.clone(), CheckOutcome::Ok));
                }
            }
        }
    }

    // Second: any baseline entries the current run failed to produce.
    for name in baseline.measurements.keys() {
        if !current.contains_key(name.as_str()) {
            failed = true;
            outcomes.push((name.clone(), CheckOutcome::Missing));
        }
    }

    (outcomes, failed)
}

/// Load a baseline JSON from disk.
pub fn load_baseline(path: &Path) -> Result<Baseline> {
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let parsed: Baseline = serde_json::from_str(&raw)
        .with_context(|| format!("parse {} as Baseline", path.display()))?;
    Ok(parsed)
}

/// Pretty-print check outcomes; used both by CLI and the integration test.
pub fn render_outcomes(crate_name: &str, outcomes: &[(String, CheckOutcome)]) -> String {
    use std::fmt::Write;
    let mut buf = String::new();
    let _ = writeln!(buf, "━━━ bench-baseline check — {crate_name} ━━━");
    let mut regressed = 0usize;
    let mut new = 0usize;
    let mut missing = 0usize;
    for (name, outcome) in outcomes {
        match outcome {
            CheckOutcome::Ok => {
                let _ = writeln!(buf, "  ✅  {name}");
            }
            CheckOutcome::Regressed { delta_pct } => {
                regressed += 1;
                if *delta_pct == i64::MAX {
                    let _ = writeln!(buf, "  ❌  {name}  regressed (baseline=0 ns/iter)");
                } else {
                    let _ = writeln!(buf, "  ❌  {name}  regressed +{delta_pct}%");
                }
            }
            CheckOutcome::New => {
                new += 1;
                let _ = writeln!(buf, "  🆕  {name}  not yet in baseline (warn)");
            }
            CheckOutcome::Missing => {
                missing += 1;
                let _ = writeln!(buf, "  ⚠️   {name}  in baseline but absent from this run");
            }
        }
    }
    let _ = writeln!(
        buf,
        "Summary: {} regressed, {} new, {} missing",
        regressed, new, missing
    );
    buf
}

/// Glue used by the CLI: load both files, run the check, render, exit code.
///
/// `warn_only`: print the report but always return `Ok(())`.  Used while the
/// CI-side baselines are still being calibrated — shared GitHub runners are
/// 2-5× noisier than a local machine, so a checked-in baseline captured on a
/// developer host will flag spurious regressions on PR runs.  We keep the
/// report visible (it still surfaces real regressions for human review) but
/// don't block PR merges until baselines are captured from CI itself.
pub fn run_check(
    input_path: &Path,
    baseline_path: &Path,
    crate_name: &str,
    warn_only: bool,
) -> Result<()> {
    let raw =
        fs::read_to_string(input_path).with_context(|| format!("read {}", input_path.display()))?;
    let measurements = parse_bencher_output(&raw);
    if measurements.is_empty() {
        bail!(
            "no bencher-format measurements parsed from {} — did you pass `--output-format bencher`?",
            input_path.display()
        );
    }
    let baseline = load_baseline(baseline_path)?;
    if baseline.crate_name != crate_name {
        return Err(anyhow!(
            "baseline {} is for crate '{}', not '{}'",
            baseline_path.display(),
            baseline.crate_name,
            crate_name
        ));
    }
    let (outcomes, failed) = check_against_baseline(&measurements, &baseline);
    print!("{}", render_outcomes(crate_name, &outcomes));
    if failed {
        if warn_only {
            eprintln!(
                "bench-baseline: regression(s) detected for {crate_name} but --warn-only is set; exit 0 (threshold {:.1}%)",
                baseline.regression_threshold_pct
            );
            return Ok(());
        }
        bail!(
            "bench-baseline check FAILED for {crate_name} (threshold {:.1}%)",
            baseline.regression_threshold_pct
        );
    }
    Ok(())
}

/// Glue used by the CLI: parse the bencher log and write a fresh baseline.
pub fn run_update(
    input_path: &Path,
    output_path: &Path,
    crate_name: &str,
    threshold_pct: f64,
) -> Result<()> {
    let raw =
        fs::read_to_string(input_path).with_context(|| format!("read {}", input_path.display()))?;
    let measurements = parse_bencher_output(&raw);
    if measurements.is_empty() {
        bail!(
            "no bencher-format measurements parsed from {} — did you pass `--output-format bencher`?",
            input_path.display()
        );
    }
    write_baseline(crate_name, &measurements, threshold_pct, output_path)?;
    println!(
        "Wrote {} measurements to {} (threshold {:.1}%)",
        measurements.len(),
        output_path.display(),
        threshold_pct
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_well_formed_bencher_lines() {
        let input = "\
test task_agenda/push/100 ... bench:        4321 ns/iter (+/- 234)
test mlfq/boost/1000      ... bench:       12345 ns/iter (+/- 1,200)
unrelated garbage line that should be ignored
";
        let m = parse_bencher_output(input);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].name, "task_agenda/push/100");
        assert_eq!(m[0].ns_per_iter, 4321);
        assert_eq!(m[0].noise, 234);
        assert_eq!(m[1].name, "mlfq/boost/1000");
        assert_eq!(m[1].ns_per_iter, 12345);
        assert_eq!(m[1].noise, 1200);
    }

    #[test]
    fn flags_regressions_beyond_threshold() {
        let measurements = vec![
            BencherMeasurement {
                name: "a".into(),
                ns_per_iter: 130,
                noise: 5,
            }, // +30% vs 100
            BencherMeasurement {
                name: "b".into(),
                ns_per_iter: 105,
                noise: 5,
            }, // +5% vs 100
        ];
        let mut baseline = Baseline {
            crate_name: "demo".into(),
            captured_at: "now".into(),
            regression_threshold_pct: 20.0,
            noise_floor_ns: 0,
            measurements: BTreeMap::new(),
        };
        baseline.measurements.insert(
            "a".into(),
            BaselineEntry {
                ns_per_iter: 100,
                noise: 5,
            },
        );
        baseline.measurements.insert(
            "b".into(),
            BaselineEntry {
                ns_per_iter: 100,
                noise: 5,
            },
        );
        let (outcomes, failed) = check_against_baseline(&measurements, &baseline);
        assert!(failed, "30% regression should fail the gate");
        let a_outcome = &outcomes.iter().find(|(n, _)| n == "a").unwrap().1;
        assert!(matches!(a_outcome, CheckOutcome::Regressed { .. }));
        let b_outcome = &outcomes.iter().find(|(n, _)| n == "b").unwrap().1;
        assert_eq!(b_outcome, &CheckOutcome::Ok);
    }

    #[test]
    fn flags_missing_baseline_entries() {
        let measurements = vec![BencherMeasurement {
            name: "a".into(),
            ns_per_iter: 100,
            noise: 5,
        }];
        let mut baseline = Baseline {
            crate_name: "demo".into(),
            captured_at: "now".into(),
            regression_threshold_pct: 20.0,
            noise_floor_ns: 0,
            measurements: BTreeMap::new(),
        };
        baseline.measurements.insert(
            "a".into(),
            BaselineEntry {
                ns_per_iter: 100,
                noise: 5,
            },
        );
        baseline.measurements.insert(
            "b".into(),
            BaselineEntry {
                ns_per_iter: 200,
                noise: 10,
            },
        );
        let (outcomes, failed) = check_against_baseline(&measurements, &baseline);
        assert!(failed, "missing baseline entry should fail the gate");
        let b_outcome = &outcomes.iter().find(|(n, _)| n == "b").unwrap().1;
        assert_eq!(b_outcome, &CheckOutcome::Missing);
    }

    #[test]
    fn noise_floor_suppresses_small_absolute_regressions() {
        // Baseline = 1 ns, measurement = 2 ns → +100% but only +1 ns absolute.
        // With noise_floor_ns = 100, the gate should not fire.
        let measurements = vec![BencherMeasurement {
            name: "tiny".into(),
            ns_per_iter: 2,
            noise: 0,
        }];
        let mut baseline = Baseline {
            crate_name: "demo".into(),
            captured_at: "now".into(),
            regression_threshold_pct: 20.0,
            noise_floor_ns: 100,
            measurements: BTreeMap::new(),
        };
        baseline.measurements.insert(
            "tiny".into(),
            BaselineEntry {
                ns_per_iter: 1,
                noise: 0,
            },
        );
        let (outcomes, failed) = check_against_baseline(&measurements, &baseline);
        assert!(
            !failed,
            "1→2 ns should be absorbed by the 100 ns noise floor"
        );
        assert_eq!(outcomes[0].1, CheckOutcome::Ok);
    }

    #[test]
    fn zero_baseline_regression_renders_without_panic() {
        // Baseline = 0 ns/iter, current = 500 ns/iter.
        // delta_pct would be f64::INFINITY; guard must prevent cast to i64::MAX
        // and render a human-readable message instead of a nonsensical value.
        let measurements = vec![BencherMeasurement {
            name: "zero_base".into(),
            ns_per_iter: 500,
            noise: 0,
        }];
        let mut baseline = Baseline {
            crate_name: "demo".into(),
            captured_at: "now".into(),
            regression_threshold_pct: 20.0,
            noise_floor_ns: 100, // 500 ns > 100 ns noise floor → should fail
            measurements: BTreeMap::new(),
        };
        baseline.measurements.insert(
            "zero_base".into(),
            BaselineEntry {
                ns_per_iter: 0,
                noise: 0,
            },
        );
        let (outcomes, failed) = check_against_baseline(&measurements, &baseline);
        assert!(failed, "regression from 0 baseline should fail the gate");
        let outcome = &outcomes.iter().find(|(n, _)| n == "zero_base").unwrap().1;
        assert!(
            matches!(outcome, CheckOutcome::Regressed { delta_pct } if *delta_pct == i64::MAX),
            "zero-baseline regression sentinel should be i64::MAX"
        );
        // Ensure the renderer does not produce the raw i64::MAX value.
        let rendered = render_outcomes("demo", &outcomes);
        assert!(
            !rendered.contains(&i64::MAX.to_string()),
            "renderer must not print raw i64::MAX"
        );
        assert!(
            rendered.contains("baseline=0 ns/iter"),
            "renderer should note zero baseline"
        );
    }

    #[test]
    fn new_measurements_warn_but_do_not_fail() {
        let measurements = vec![
            BencherMeasurement {
                name: "existing".into(),
                ns_per_iter: 100,
                noise: 5,
            },
            BencherMeasurement {
                name: "fresh".into(),
                ns_per_iter: 999,
                noise: 5,
            },
        ];
        let mut baseline = Baseline {
            crate_name: "demo".into(),
            captured_at: "now".into(),
            regression_threshold_pct: 20.0,
            noise_floor_ns: 0,
            measurements: BTreeMap::new(),
        };
        baseline.measurements.insert(
            "existing".into(),
            BaselineEntry {
                ns_per_iter: 100,
                noise: 5,
            },
        );
        let (outcomes, failed) = check_against_baseline(&measurements, &baseline);
        assert!(!failed, "a new bench should warn, not fail");
        let fresh_outcome = &outcomes.iter().find(|(n, _)| n == "fresh").unwrap().1;
        assert_eq!(fresh_outcome, &CheckOutcome::New);
    }
}
