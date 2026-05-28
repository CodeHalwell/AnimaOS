//! E4.7 — MicroVM long-running soak driver.
//!
//! Drives repeated boot cycles of the `anima-microvm` EFI image under QEMU,
//! recording per-iteration outcomes and writing a resumable checkpoint
//! manifest.  Used for the 30-day production-readiness soak.
//!
//! # Dry-run mode
//!
//! Pass `--dry-run` to exercise the harness without QEMU.  The soak
//! reports a fixed successful iteration, verifying manifest schema and
//! serialisation logic.  This is the CI smoke-test path.
//!
//! # Invocation
//!
//! ```text
//! cargo xtask soak --efi path/to/anima-microvm.efi --iterations 5
//! cargo xtask soak --dry-run --iterations 1
//! ```

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// CLI types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, clap::Args)]
pub struct SoakArgs {
    /// Path to the compiled `anima-microvm.efi` image.
    #[arg(
        long,
        default_value = "kernels/microvm/target/x86_64-unknown-uefi/release/anima-microvm.efi"
    )]
    pub efi: PathBuf,

    /// Directory for the OVMF firmware file.  The harness searches common
    /// Ubuntu paths automatically; this override is for non-standard installs.
    #[arg(long)]
    pub ovmf_dir: Option<PathBuf>,

    /// Number of boot iterations to run (default 1 for CI smoke-test).
    #[arg(long, default_value = "1")]
    pub iterations: u32,

    /// Per-iteration timeout in seconds (default 30).
    #[arg(long, default_value = "30")]
    pub timeout_secs: u64,

    /// Output directory for manifests and logs.
    #[arg(long, default_value = "artifacts/soak")]
    pub output: PathBuf,

    /// Dry-run mode: skip QEMU, report a synthetic success.
    #[arg(long, default_value = "false")]
    pub dry_run: bool,

    /// Pause between boot iterations in seconds.
    ///
    /// The documented 30-day production soak runs 8 640 iterations at a 5-minute
    /// (300 s) cycle.  CI smoke-tests use 0 (no pause).  Ignored in dry-run mode.
    #[arg(long, default_value = "0")]
    pub interval_secs: u64,
}

// ---------------------------------------------------------------------------
// Outcome types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum IterationOutcome {
    /// All expected marker strings were found in the serial output.
    Ok,
    /// QEMU did not produce the expected marker within the timeout.
    Timeout,
    /// QEMU exited with a non-zero or unexpected status (no timeout).
    UnscheduledExit { exit_code: i32 },
    /// Dry-run synthetic result.
    DryRun,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IterationRecord {
    pub iteration: u32,
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub boot_latency_ms: u64,
    pub outcome: IterationOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoakManifest {
    pub started_at: DateTime<Utc>,
    pub last_updated: DateTime<Utc>,
    pub total_iterations: u32,
    pub completed_iterations: u32,
    pub successful_iterations: u32,
    pub timeout_iterations: u32,
    pub unscheduled_exit_iterations: u32,
    pub mean_boot_latency_ms: f64,
    pub p95_boot_latency_ms: f64,
    pub iterations: Vec<IterationRecord>,
}

// ---------------------------------------------------------------------------
// QEMU invocation
// ---------------------------------------------------------------------------

/// Search common Ubuntu paths for OVMF firmware.
fn find_ovmf(ovmf_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = ovmf_dir {
        let path = dir.join("OVMF_CODE.fd");
        if path.exists() {
            return Some(path);
        }
        // Try versioned variants
        for name in &["OVMF_CODE_4M.fd", "OVMF.fd"] {
            let p = dir.join(name);
            if p.exists() {
                return Some(p);
            }
        }
    }

    // Auto-detect from common install paths.
    let candidates = [
        "/usr/share/OVMF/OVMF_CODE.fd",
        "/usr/share/OVMF/OVMF_CODE_4M.fd",
        "/usr/share/ovmf/OVMF.fd",
        "/usr/share/edk2/ovmf/OVMF_CODE.fd",
    ];
    for c in &candidates {
        let p = PathBuf::from(c);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Required serial output markers (E4.1 through E4.4 exit criteria).
const REQUIRED_MARKERS: &[&str] = &[
    "E4.2_TASK_DONE",
    "E4.3_TCP_DONE",
    "E4.4_TLS_DONE",
    "ANIMA_PANIC",
];

fn run_qemu_iteration(
    efi: &Path,
    ovmf_code: &Path,
    timeout_secs: u64,
    iteration: u32,
    log_dir: &Path,
    esp_dir: &Path,
) -> Result<(IterationOutcome, u64)> {
    // Reuse the shared ESP directory — copy the EFI image to the fixed location.
    // The directory was created once before the loop; only the image is refreshed.
    let efi_boot_dir = esp_dir.join("EFI").join("BOOT");
    fs::copy(efi, efi_boot_dir.join("BOOTX64.EFI")).context("copying EFI image")?;

    let serial_log = log_dir.join(format!("serial-{iteration}.txt"));

    let start = Instant::now();
    let deadline = start + Duration::from_secs(timeout_secs);

    // Spawn QEMU without the `timeout` wrapper so we can kill it ourselves the
    // instant the required serial markers appear, recording accurate boot latency.
    let mut child = Command::new("qemu-system-x86_64")
        .arg("-cpu")
        .arg("qemu64,+rdrand")
        .arg("-drive")
        .arg(format!(
            "if=pflash,format=raw,readonly=on,file={}",
            ovmf_code.display()
        ))
        .arg("-drive")
        .arg(format!("format=raw,file=fat:rw:{}", esp_dir.display()))
        .arg("-serial")
        .arg(format!("file:{}", serial_log.display()))
        .arg("-display")
        .arg("none")
        .arg("-m")
        .arg("512M")
        .arg("-no-reboot")
        .spawn()
        .context("spawning qemu-system-x86_64")?;

    // Poll the serial log for required markers.  Boot latency is recorded the
    // moment all markers are found — not after QEMU exits (which may be the
    // full timeout duration after the panic handler has already run).
    let mut boot_latency_ms: Option<u64> = None;
    let mut unscheduled_exit: Option<i32> = None;
    while Instant::now() < deadline {
        if let Ok(content) = fs::read_to_string(&serial_log) {
            if REQUIRED_MARKERS.iter().all(|m| content.contains(m)) {
                boot_latency_ms = Some(start.elapsed().as_millis() as u64);
                break;
            }
        }
        // Check whether QEMU exited early (crash or clean shutdown before
        // all markers were emitted).
        match child.try_wait() {
            Ok(Some(status)) => {
                unscheduled_exit = Some(status.code().unwrap_or(-1));
                break;
            }
            Ok(None) => {}
            Err(_) => {}
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // Kill QEMU (it stays alive in the panic handler) and reap the child.
    let _ = child.kill();
    let _ = child.wait();

    let outcome = if let Some(ms) = boot_latency_ms {
        return Ok((IterationOutcome::Ok, ms));
    } else if let Some(exit_code) = unscheduled_exit {
        // One final check: the markers may have appeared just before the exit.
        let content = fs::read_to_string(&serial_log).unwrap_or_default();
        if REQUIRED_MARKERS.iter().all(|m| content.contains(m)) {
            return Ok((IterationOutcome::Ok, start.elapsed().as_millis() as u64));
        }
        IterationOutcome::UnscheduledExit { exit_code }
    } else {
        // One final check after the deadline in case markers appeared just before
        // we stopped polling.
        let content = fs::read_to_string(&serial_log).unwrap_or_default();
        if REQUIRED_MARKERS.iter().all(|m| content.contains(m)) {
            return Ok((IterationOutcome::Ok, start.elapsed().as_millis() as u64));
        }
        IterationOutcome::Timeout
    };

    Ok((outcome, start.elapsed().as_millis() as u64))
}

// ---------------------------------------------------------------------------
// Statistics helpers
// ---------------------------------------------------------------------------

fn compute_stats(latencies: &[u64]) -> (f64, f64) {
    if latencies.is_empty() {
        return (0.0, 0.0);
    }
    let mean = latencies.iter().sum::<u64>() as f64 / latencies.len() as f64;
    let mut sorted = latencies.to_vec();
    sorted.sort_unstable();
    let p95_idx = (sorted.len() as f64 * 0.95) as usize;
    let p95 = sorted[p95_idx.min(sorted.len() - 1)] as f64;
    (mean, p95)
}

// ---------------------------------------------------------------------------
// Manifest persistence
// ---------------------------------------------------------------------------

fn save_manifest(manifest: &SoakManifest, output_dir: &Path) -> Result<()> {
    fs::create_dir_all(output_dir).context("creating soak output directory")?;
    let path = output_dir.join("manifest.json");
    let tmp = output_dir.join("manifest.json.tmp");
    let json = serde_json::to_string_pretty(manifest).context("serialising soak manifest")?;
    fs::write(&tmp, &json).context("writing manifest to tmp")?;
    // `fs::rename` is atomic on Unix but fails on Windows when the destination
    // already exists.  Use a remove-then-rename fallback for portability.
    if let Err(e) = fs::rename(&tmp, &path) {
        // On Windows the destination must not exist for rename to succeed.
        let _ = fs::remove_file(&path);
        fs::rename(&tmp, &path).with_context(|| format!("renaming manifest: {e}"))?;
    }

    // Append to JSONL log for durability.
    if let Some(last) = manifest.iterations.last() {
        let jsonl_path = output_dir.join("iterations.jsonl");
        let line = serde_json::to_string(last).context("serialising iteration record")?;
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&jsonl_path)
            .context("opening JSONL log")?;
        writeln!(f, "{}", line).context("writing JSONL line")?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Entrypoint
// ---------------------------------------------------------------------------

pub fn run_soak(args: SoakArgs) -> Result<()> {
    let output = args.output.clone();
    fs::create_dir_all(&output).context("creating output directory")?;

    let log_dir = output.join("logs");
    fs::create_dir_all(&log_dir).context("creating log directory")?;

    // Resume from an existing manifest when present so the soak can be
    // interrupted and restarted without losing previous progress.
    let manifest_path = output.join("manifest.json");
    let mut manifest = if manifest_path.exists() {
        let json = fs::read_to_string(&manifest_path).context("reading existing manifest")?;
        let mut m: SoakManifest =
            serde_json::from_str(&json).context("parsing existing manifest")?;
        // Allow callers to extend a completed run by raising --iterations.
        m.total_iterations = m.total_iterations.max(args.iterations);
        println!(
            "Resuming soak from iteration {}/{}.",
            m.completed_iterations, m.total_iterations
        );
        m
    } else {
        let now = Utc::now();
        SoakManifest {
            started_at: now,
            last_updated: now,
            total_iterations: args.iterations,
            completed_iterations: 0,
            successful_iterations: 0,
            timeout_iterations: 0,
            unscheduled_exit_iterations: 0,
            mean_boot_latency_ms: 0.0,
            p95_boot_latency_ms: 0.0,
            iterations: Vec::new(),
        }
    };

    if args.dry_run {
        println!("🔵 Dry-run mode: skipping QEMU, generating synthetic soak result.");
        let start_i = manifest.completed_iterations + 1;
        let mut latencies: Vec<u64> = manifest
            .iterations
            .iter()
            .filter(|r| matches!(r.outcome, IterationOutcome::Ok | IterationOutcome::DryRun))
            .map(|r| r.boot_latency_ms)
            .collect();
        // Append each JSONL record individually (correctness: one line per
        // iteration), but open the file once before the loop rather than
        // per-iteration to avoid O(N) open/close syscalls for large iteration
        // counts.  Stats computation and manifest.json write are deferred to
        // after the loop (O(N log N) once vs O(N² log N) if called each iteration).
        let jsonl_path = output.join("iterations.jsonl");
        let mut jsonl_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&jsonl_path)
            .context("opening JSONL log")?;
        for i in start_i..=manifest.total_iterations {
            let record = IterationRecord {
                iteration: i,
                started_at: Utc::now(),
                completed_at: Utc::now(),
                boot_latency_ms: 50,
                outcome: IterationOutcome::DryRun,
            };
            latencies.push(record.boot_latency_ms);
            // Append this iteration's record to the JSONL log immediately so
            // the file stays consistent even if the process is interrupted.
            let line = serde_json::to_string(&record).context("serialising dry-run iteration")?;
            use std::io::Write as _;
            writeln!(jsonl_file, "{}", line).context("writing JSONL line")?;
            manifest.iterations.push(record);
            manifest.completed_iterations += 1;
            manifest.successful_iterations += 1;
        }
        manifest.last_updated = Utc::now();
        let (mean, p95) = compute_stats(&latencies);
        manifest.mean_boot_latency_ms = mean;
        manifest.p95_boot_latency_ms = p95;
        // Write manifest.json once at the end (deferred from per-iteration writes).
        let manifest_path = output.join("manifest.json");
        let tmp_path = output.join("manifest.json.tmp");
        let json = serde_json::to_string_pretty(&manifest).context("serialising manifest")?;
        fs::write(&tmp_path, &json).context("writing manifest to tmp")?;
        if let Err(e) = fs::rename(&tmp_path, &manifest_path) {
            let _ = fs::remove_file(&manifest_path);
            fs::rename(&tmp_path, &manifest_path)
                .with_context(|| format!("renaming manifest: {e}"))?;
        }
        print_summary(&manifest);
        return Ok(());
    }

    // Find OVMF firmware.
    let ovmf_code = find_ovmf(args.ovmf_dir.as_deref())
        .context("Could not locate OVMF_CODE.fd — install ovmf or pass --ovmf-dir")?;
    println!("Using OVMF: {}", ovmf_code.display());
    println!("EFI image : {}", args.efi.display());
    println!("Iterations: {}", manifest.total_iterations);
    println!("Timeout   : {} s", args.timeout_secs);
    println!("Output    : {}", output.display());
    println!();

    // Create the shared ESP directory once; all iterations reuse it.
    let esp_dir = log_dir.join("esp-shared");
    let efi_boot_dir = esp_dir.join("EFI").join("BOOT");
    fs::create_dir_all(&efi_boot_dir).context("creating shared EFI/BOOT directory")?;

    let mut latencies: Vec<u64> = manifest
        .iterations
        .iter()
        .filter(|r| matches!(r.outcome, IterationOutcome::Ok | IterationOutcome::DryRun))
        .map(|r| r.boot_latency_ms)
        .collect();
    let mut all_ok = manifest.timeout_iterations == 0 && manifest.unscheduled_exit_iterations == 0;

    let start_i = manifest.completed_iterations + 1;
    for i in start_i..=manifest.total_iterations {
        let started_at = Utc::now();
        let start = Instant::now();

        println!("Iteration {i}/{} ...", manifest.total_iterations);

        let (outcome, boot_latency_ms) = run_qemu_iteration(
            &args.efi,
            &ovmf_code,
            args.timeout_secs,
            i,
            &log_dir,
            &esp_dir,
        )?;

        let completed_at = Utc::now();
        let status_str = match &outcome {
            IterationOutcome::Ok => "✅ OK",
            IterationOutcome::Timeout => "⚠  Timeout",
            IterationOutcome::UnscheduledExit { exit_code } => {
                eprintln!("  ⚠  Unscheduled exit (code {exit_code})");
                "❌ UnscheduledExit"
            }
            IterationOutcome::DryRun => "🔵 DryRun",
        };
        println!(
            "  {status_str}  boot_latency={boot_latency_ms} ms  elapsed={:.1}s",
            start.elapsed().as_secs_f32()
        );

        match &outcome {
            IterationOutcome::Ok | IterationOutcome::DryRun => {
                manifest.successful_iterations += 1;
                latencies.push(boot_latency_ms);
            }
            IterationOutcome::Timeout => {
                manifest.timeout_iterations += 1;
                all_ok = false;
            }
            IterationOutcome::UnscheduledExit { .. } => {
                manifest.unscheduled_exit_iterations += 1;
                all_ok = false;
            }
        }

        manifest.iterations.push(IterationRecord {
            iteration: i,
            started_at,
            completed_at,
            boot_latency_ms,
            outcome,
        });
        manifest.completed_iterations += 1;

        let (mean, p95) = compute_stats(&latencies);
        manifest.mean_boot_latency_ms = mean;
        manifest.p95_boot_latency_ms = p95;
        manifest.last_updated = Utc::now();

        save_manifest(&manifest, &output)?;

        // Inter-iteration pause.  Full 30-day soak uses 300 s; CI smoke-tests
        // use 0.  Hard-coded 200 ms is replaced by the caller-supplied value.
        if args.interval_secs > 0 {
            std::thread::sleep(Duration::from_secs(args.interval_secs));
        }
    }

    println!();
    print_summary(&manifest);

    if !all_ok {
        anyhow::bail!(
            "Soak completed with {} timeout(s) and {} unscheduled exit(s).",
            manifest.timeout_iterations,
            manifest.unscheduled_exit_iterations,
        );
    }

    Ok(())
}

fn print_summary(manifest: &SoakManifest) {
    println!("━━━ Soak Summary ━━━");
    println!(
        "  Iterations  : {}/{}",
        manifest.completed_iterations, manifest.total_iterations
    );
    println!("  Successful  : {}", manifest.successful_iterations);
    println!("  Timeouts    : {}", manifest.timeout_iterations);
    println!("  Unscheduled : {}", manifest.unscheduled_exit_iterations);
    println!(
        "  Boot latency: mean={:.1} ms  p95={:.1} ms",
        manifest.mean_boot_latency_ms, manifest.p95_boot_latency_ms
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn compute_stats_empty_returns_zeros() {
        let (mean, p95) = compute_stats(&[]);
        assert_eq!(mean, 0.0);
        assert_eq!(p95, 0.0);
    }

    #[test]
    fn compute_stats_single_element() {
        let (mean, p95) = compute_stats(&[100]);
        assert_eq!(mean, 100.0);
        assert_eq!(p95, 100.0);
    }

    #[test]
    fn compute_stats_multiple_elements() {
        let latencies = vec![100u64, 200, 150, 300, 250];
        let (mean, _p95) = compute_stats(&latencies);
        // mean = (100+200+150+300+250)/5 = 200
        assert!((mean - 200.0).abs() < 0.1);
    }

    #[test]
    fn dry_run_produces_successful_iterations() {
        let tmp = TempDir::new().unwrap();
        let args = SoakArgs {
            efi: PathBuf::from("/nonexistent.efi"),
            ovmf_dir: None,
            iterations: 3,
            timeout_secs: 30,
            output: tmp.path().to_path_buf(),
            dry_run: true,
            interval_secs: 0,
        };
        run_soak(args).expect("dry-run should succeed");

        let manifest_path = tmp.path().join("manifest.json");
        assert!(manifest_path.exists(), "manifest.json should be written");

        let json = fs::read_to_string(&manifest_path).unwrap();
        let manifest: SoakManifest = serde_json::from_str(&json).unwrap();

        assert_eq!(manifest.completed_iterations, 3);
        assert_eq!(manifest.successful_iterations, 3);
        assert_eq!(manifest.timeout_iterations, 0);
        assert_eq!(manifest.unscheduled_exit_iterations, 0);
        assert_eq!(manifest.iterations.len(), 3);
        assert!(
            manifest
                .iterations
                .iter()
                .all(|r| r.outcome == IterationOutcome::DryRun),
            "all iterations should be DryRun"
        );
    }

    #[test]
    fn dry_run_writes_jsonl_log() {
        let tmp = TempDir::new().unwrap();
        let args = SoakArgs {
            efi: PathBuf::from("/nonexistent.efi"),
            ovmf_dir: None,
            iterations: 2,
            timeout_secs: 30,
            output: tmp.path().to_path_buf(),
            dry_run: true,
            interval_secs: 0,
        };
        run_soak(args).unwrap();

        let jsonl_path = tmp.path().join("iterations.jsonl");
        assert!(jsonl_path.exists(), "iterations.jsonl should be written");
        let lines: Vec<_> = fs::read_to_string(&jsonl_path)
            .unwrap()
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect();
        assert_eq!(lines.len(), 2, "one JSONL line per iteration");

        // Each line must be valid JSON with an 'iteration' field.
        for (i, line) in lines.iter().enumerate() {
            let val: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(val["iteration"].as_u64().unwrap(), (i + 1) as u64);
        }
    }

    #[test]
    fn manifest_schema_round_trips() {
        let manifest = SoakManifest {
            started_at: Utc::now(),
            last_updated: Utc::now(),
            total_iterations: 5,
            completed_iterations: 3,
            successful_iterations: 3,
            timeout_iterations: 0,
            unscheduled_exit_iterations: 0,
            mean_boot_latency_ms: 42.5,
            p95_boot_latency_ms: 80.0,
            iterations: vec![IterationRecord {
                iteration: 1,
                started_at: Utc::now(),
                completed_at: Utc::now(),
                boot_latency_ms: 42,
                outcome: IterationOutcome::Ok,
            }],
        };

        let json = serde_json::to_string_pretty(&manifest).unwrap();
        let recovered: SoakManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.total_iterations, 5);
        assert_eq!(recovered.successful_iterations, 3);
        assert_eq!(recovered.iterations[0].outcome, IterationOutcome::Ok);
    }
}
