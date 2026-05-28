//! `xtask soak` — Epic E4.7 long-running soak driver.
//!
//! # What this is
//!
//! A wrapper around `qemu-system-x86_64` that boots the production microVM
//! EFI image in a loop for a configurable duration and records:
//!
//! * Per-iteration boot-to-soak-complete latency (ms).
//! * Whether each iteration completed cleanly (`E4.5_SOAK_DONE` observed),
//!   timed out, or exited prematurely (`ANIMA_PANIC` without `_SOAK_DONE`).
//! * A JSONL log flushed every iteration so a 30-day run can be resumed /
//!   inspected if the host reboots.  The `manifest.json` is written once at
//!   run completion (or on graceful interrupt) to avoid massive per-iteration
//!   I/O write amplification.
//!
//! # Why it lives in xtask, not in a CI workflow
//!
//! The 30-day soak (E4.7 exit criterion 2) is operator-driven: it runs on a
//! dedicated host, not on a GitHub-hosted runner.  Putting the logic in
//! `xtask` keeps the schedule + parsing + checkpointing reproducible from
//! `cargo xtask soak` on any machine that has QEMU + OVMF installed.  The
//! `.github/workflows/soak.yml` workflow exists as a *smoke test* of this
//! harness — it runs a 5-minute soak on every dispatch to make sure the
//! driver still works.
//!
//! # Modes
//!
//! * **Live** — `--efi <path>` provided.  Each iteration spawns QEMU and
//!   verifies the COM1 markers.
//! * **Dry-run** — `--efi` omitted.  The driver emits the schedule and
//!   creates an empty manifest.  Useful for CI smoke-testing the parsing
//!   logic without depending on QEMU+OVMF on the runner.
//!
//! # Checkpoint / resume
//!
//! On startup, if `<output>/iterations.jsonl` already exists the driver
//! continues appending from the last recorded index.  The original
//! `started_at` timestamp is preserved from the existing `manifest.json`.
//! The manifest is written only at the end (or on clean shutdown) to avoid
//! rewriting a growing JSON blob every iteration.
//!
//! # Manifest schema
//!
//! See `SoakManifest` below — it is a stable JSON file written to
//! `<output>/manifest.json`.  Each iteration appends a row to
//! `<output>/iterations.jsonl` so the manifest can be reconstructed if it
//! is corrupted or truncated.

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub struct SoakConfig {
    pub hours: f64,
    pub efi: Option<PathBuf>,
    pub output: PathBuf,
    pub iteration_timeout_s: u64,
    pub ovmf: Option<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SoakManifest {
    pub started_at: String,
    pub finished_at: Option<String>,
    pub planned_hours: f64,
    pub mode: SoakMode,
    pub efi_path: Option<String>,
    pub ovmf_path: Option<String>,
    pub iteration_timeout_s: u64,
    pub iterations: Vec<IterationRecord>,
    pub summary: Option<SoakSummary>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum SoakMode {
    Live,
    DryRun,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct IterationRecord {
    pub index: u64,
    pub started_at: String,
    pub boot_ms: Option<u64>,
    pub outcome: IterationOutcome,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum IterationOutcome {
    Ok,
    Timeout,
    UnscheduledExit,
    DryRunSkipped,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SoakSummary {
    pub iterations: u64,
    pub ok: u64,
    pub timeouts: u64,
    pub unscheduled_exits: u64,
    pub mean_boot_ms: Option<f64>,
    pub p95_boot_ms: Option<u64>,
}

pub fn run(cfg: SoakConfig) -> Result<()> {
    fs::create_dir_all(&cfg.output).with_context(|| format!("mkdir {}", cfg.output.display()))?;

    let manifest_path = cfg.output.join("manifest.json");
    let iterations_log_path = cfg.output.join("iterations.jsonl");

    let mode = if cfg.efi.is_some() {
        SoakMode::Live
    } else {
        SoakMode::DryRun
    };

    // Load existing manifest header (preserves `started_at` on resume) or
    // create a fresh one.  Iterations are always sourced from the JSONL log,
    // not from the manifest — the manifest is the run header, not the record.
    let mut manifest = load_or_create_manifest(&manifest_path, &cfg, mode);
    manifest.finished_at = None;
    manifest.iterations.clear();
    manifest.summary = None;

    let mut iterations_log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&iterations_log_path)
        .with_context(|| format!("open {}", iterations_log_path.display()))?;

    // Derive the resume index from existing JSONL content.
    let mut index = derive_resume_index(&iterations_log_path);
    if index > 0 {
        println!(
            "soak: resuming from iteration {} (prior entries in {})",
            index,
            iterations_log_path.display()
        );
    }

    let total_budget = Duration::from_secs_f64(manifest.planned_hours * 3600.0);
    let started = Instant::now();

    println!(
        "soak: planned={:.2} h  mode={:?}  output={}",
        manifest.planned_hours,
        mode,
        cfg.output.display()
    );

    if mode == SoakMode::DryRun {
        // Emit one record so the manifest is non-empty and CI assertions can
        // verify the harness ran end-to-end without QEMU.
        let rec = IterationRecord {
            index,
            started_at: Utc::now().to_rfc3339(),
            boot_ms: None,
            outcome: IterationOutcome::DryRunSkipped,
        };
        append_iteration(&mut iterations_log, &rec)?;
        manifest.iterations.push(rec);
        manifest.finished_at = Some(Utc::now().to_rfc3339());
        manifest.summary = Some(summarise(&manifest.iterations));
        write_manifest(&manifest_path, &manifest)?;
        println!(
            "soak: dry-run complete — wrote stub manifest at {}",
            manifest_path.display()
        );
        return Ok(());
    }

    let efi = cfg
        .efi
        .clone()
        .expect("live mode requires --efi (checked above)");
    let ovmf = resolve_ovmf(cfg.ovmf.as_deref())?;
    let esp = prepare_esp(&cfg.output, &efi)?;
    println!("soak: ESP prepared at {}", esp.display());

    while started.elapsed() < total_budget {
        let rec = run_one_iteration(index, &cfg, &ovmf, &esp)?;
        append_iteration(&mut iterations_log, &rec)?;
        println!(
            "soak: iter {} → {:?} ({} ms)",
            rec.index,
            rec.outcome,
            rec.boot_ms
                .map(|v| v.to_string())
                .unwrap_or_else(|| "—".into())
        );
        // Delete serial log for successful iterations to prevent inode
        // exhaustion on long (30-day) runs.  Failed iteration logs are
        // retained for post-mortem debugging.
        if rec.outcome == IterationOutcome::Ok {
            let serial_log = cfg.output.join(format!("serial-{:06}.txt", rec.index));
            let _ = fs::remove_file(serial_log);
        }
        index += 1;
    }

    // Reconstruct all iteration records from the JSONL log for the final
    // manifest.  This avoids holding an O(n) Vec in RAM during the run —
    // the JSONL is the single durable source of truth.
    drop(iterations_log);
    manifest.iterations = read_all_iterations(&iterations_log_path)?;
    manifest.finished_at = Some(Utc::now().to_rfc3339());
    manifest.summary = Some(summarise(&manifest.iterations));
    write_manifest(&manifest_path, &manifest)?;
    println!(
        "soak: complete — {} iterations  manifest={}",
        manifest.iterations.len(),
        manifest_path.display()
    );
    Ok(())
}

/// Load the existing manifest header for a resumed run, or create a fresh one.
fn load_or_create_manifest(path: &Path, cfg: &SoakConfig, mode: SoakMode) -> SoakManifest {
    if path.exists() {
        if let Ok(raw) = fs::read_to_string(path) {
            if let Ok(m) = serde_json::from_str::<SoakManifest>(&raw) {
                return m;
            }
        }
    }
    SoakManifest {
        started_at: Utc::now().to_rfc3339(),
        finished_at: None,
        planned_hours: cfg.hours,
        mode,
        efi_path: cfg.efi.as_ref().map(|p| p.display().to_string()),
        ovmf_path: cfg.ovmf.as_ref().map(|p| p.display().to_string()),
        iteration_timeout_s: cfg.iteration_timeout_s,
        iterations: Vec::new(),
        summary: None,
    }
}

/// Derive the index to start from by reading the existing JSONL log.
fn derive_resume_index(jsonl_path: &Path) -> u64 {
    if !jsonl_path.exists() {
        return 0;
    }
    let content = fs::read_to_string(jsonl_path).unwrap_or_default();
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<IterationRecord>(l).ok())
        .map(|r| r.index)
        .max()
        .map(|i| i + 1)
        .unwrap_or(0)
}

/// Reconstruct all iteration records from the JSONL log.
fn read_all_iterations(jsonl_path: &Path) -> Result<Vec<IterationRecord>> {
    if !jsonl_path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(jsonl_path)
        .with_context(|| format!("read {}", jsonl_path.display()))?;
    let records = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<IterationRecord>(l).ok())
        .collect();
    Ok(records)
}

fn run_one_iteration(
    index: u64,
    cfg: &SoakConfig,
    ovmf: &Path,
    esp: &Path,
) -> Result<IterationRecord> {
    let started_at = Utc::now().to_rfc3339();
    let serial_log = cfg.output.join(format!("serial-{:06}.txt", index));
    // Clear the per-iteration log so grep below only sees this run.
    let _ = fs::write(&serial_log, b"");

    let start = Instant::now();
    let mut child = Command::new("qemu-system-x86_64")
        .args([
            "-cpu",
            "qemu64,+rdrand",
            "-drive",
            &format!("if=pflash,format=raw,readonly=on,file={}", ovmf.display()),
            "-drive",
            &format!("format=raw,file=fat:rw:{}", esp.display()),
            "-serial",
            &format!("file:{}", serial_log.display()),
            "-display",
            "none",
            "-m",
            "512M",
            "-no-reboot",
        ])
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .context("spawn qemu-system-x86_64")?;

    // Poll for E4.5_SOAK_DONE up to iteration_timeout_s.
    let timeout = Duration::from_secs(cfg.iteration_timeout_s);
    let mut boot_ms: Option<u64> = None;
    // Track whether QEMU exited before the timeout elapsed.  An early exit
    // without the soak marker is an UnscheduledExit (firmware error, triple
    // fault, crash) rather than a Timeout.
    let mut qemu_exited_early = false;
    while start.elapsed() < timeout {
        if let Ok(s) = fs::read_to_string(&serial_log) {
            if s.contains("E4.5_SOAK_DONE") {
                boot_ms = Some(start.elapsed().as_millis() as u64);
                break;
            }
        }
        if let Ok(Some(_status)) = child.try_wait() {
            qemu_exited_early = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Reap the QEMU process whatever happened.
    let _ = child.kill();
    let _ = child.wait();

    let outcome = match boot_ms {
        Some(_) => IterationOutcome::Ok,
        // QEMU exited before the soak marker appeared — crash, panic, or
        // firmware error.  Classify as UnscheduledExit regardless of whether
        // ANIMA_PANIC was seen.
        None if qemu_exited_early => IterationOutcome::UnscheduledExit,
        // True timeout: elapsed >= iteration_timeout_s.  Check whether the
        // image managed to panic before the timer expired.
        None => {
            let log = fs::read_to_string(&serial_log).unwrap_or_default();
            if log.contains("ANIMA_PANIC") {
                IterationOutcome::UnscheduledExit
            } else {
                IterationOutcome::Timeout
            }
        }
    };

    Ok(IterationRecord {
        index,
        started_at,
        boot_ms,
        outcome,
    })
}

fn prepare_esp(output: &Path, efi: &Path) -> Result<PathBuf> {
    let esp = output.join("esp");
    let boot_dir = esp.join("EFI/BOOT");
    fs::create_dir_all(&boot_dir).with_context(|| format!("mkdir {}", boot_dir.display()))?;
    let dst = boot_dir.join("BOOTX64.EFI");
    fs::copy(efi, &dst).with_context(|| format!("copy {} → {}", efi.display(), dst.display()))?;
    Ok(esp)
}

fn resolve_ovmf(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        if p.exists() {
            return Ok(p.to_path_buf());
        }
        return Err(anyhow!("OVMF_CODE.fd not found at {}", p.display()));
    }
    for candidate in [
        "/usr/share/OVMF/OVMF_CODE.fd",
        "/usr/share/OVMF/OVMF_CODE_4M.fd",
        "/usr/share/ovmf/OVMF.fd",
    ] {
        if Path::new(candidate).exists() {
            return Ok(PathBuf::from(candidate));
        }
    }
    Err(anyhow!(
        "could not locate OVMF_CODE.fd; pass --ovmf <path> or install the `ovmf` package"
    ))
}

fn write_manifest(path: &Path, manifest: &SoakManifest) -> Result<()> {
    let raw = serde_json::to_string_pretty(manifest)?;
    fs::write(path, format!("{raw}\n")).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn append_iteration(log: &mut std::fs::File, rec: &IterationRecord) -> Result<()> {
    let line = serde_json::to_string(rec)?;
    writeln!(log, "{line}").context("append iteration line")?;
    log.flush().context("flush iteration log")?;
    Ok(())
}

pub fn summarise(iters: &[IterationRecord]) -> SoakSummary {
    let mut ok = 0u64;
    let mut timeouts = 0u64;
    let mut unscheduled = 0u64;
    let mut samples: Vec<u64> = Vec::new();
    for r in iters {
        match r.outcome {
            IterationOutcome::Ok => ok += 1,
            IterationOutcome::Timeout => timeouts += 1,
            IterationOutcome::UnscheduledExit => unscheduled += 1,
            IterationOutcome::DryRunSkipped => {}
        }
        if let Some(ms) = r.boot_ms {
            samples.push(ms);
        }
    }
    samples.sort_unstable();
    let mean = if samples.is_empty() {
        None
    } else {
        Some(samples.iter().sum::<u64>() as f64 / samples.len() as f64)
    };
    let p95 = if samples.is_empty() {
        None
    } else {
        // Round up so very small samples (1–2 items) yield the worst value.
        let idx = ((samples.len() as f64) * 0.95).ceil() as usize - 1;
        Some(samples[idx.min(samples.len() - 1)])
    };
    SoakSummary {
        iterations: iters.len() as u64,
        ok,
        timeouts,
        unscheduled_exits: unscheduled,
        mean_boot_ms: mean,
        p95_boot_ms: p95,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(idx: u64, outcome: IterationOutcome, boot_ms: Option<u64>) -> IterationRecord {
        IterationRecord {
            index: idx,
            started_at: "t".into(),
            boot_ms,
            outcome,
        }
    }

    #[test]
    fn summary_counts_categories_and_drops_dry_run_from_p95() {
        let iters = vec![
            mk(0, IterationOutcome::Ok, Some(800)),
            mk(1, IterationOutcome::Ok, Some(1200)),
            mk(2, IterationOutcome::Timeout, None),
            mk(3, IterationOutcome::UnscheduledExit, None),
            mk(4, IterationOutcome::DryRunSkipped, None),
        ];
        let s = summarise(&iters);
        assert_eq!(s.iterations, 5);
        assert_eq!(s.ok, 2);
        assert_eq!(s.timeouts, 1);
        assert_eq!(s.unscheduled_exits, 1);
        assert_eq!(s.mean_boot_ms, Some(1000.0));
        assert_eq!(s.p95_boot_ms, Some(1200));
    }

    #[test]
    fn dry_run_mode_writes_a_manifest_without_qemu() {
        // Use the system temp dir directly so the test is hermetic.
        let dir = std::env::temp_dir().join(format!("xtask-soak-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let cfg = SoakConfig {
            hours: 0.0001, // tiny so the live-mode loop would exit immediately anyway
            efi: None,
            output: dir.clone(),
            iteration_timeout_s: 1,
            ovmf: None,
        };
        run(cfg).expect("dry-run soak should succeed");
        let manifest_path = dir.join("manifest.json");
        let raw = fs::read_to_string(&manifest_path).unwrap();
        let m: SoakManifest = serde_json::from_str(&raw).unwrap();
        assert_eq!(m.mode, SoakMode::DryRun);
        assert!(m.summary.is_some());
        assert_eq!(m.iterations.len(), 1);
        assert_eq!(m.iterations[0].outcome, IterationOutcome::DryRunSkipped);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resume_reads_prior_iterations_from_jsonl() {
        let dir = std::env::temp_dir().join(format!(
            "xtask-soak-resume-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Write two prior iteration records to the JSONL log.
        let jsonl_path = dir.join("iterations.jsonl");
        let rec0 = mk(0, IterationOutcome::Ok, Some(500));
        let rec1 = mk(1, IterationOutcome::Timeout, None);
        {
            let mut f = fs::File::create(&jsonl_path).unwrap();
            writeln!(f, "{}", serde_json::to_string(&rec0).unwrap()).unwrap();
            writeln!(f, "{}", serde_json::to_string(&rec1).unwrap()).unwrap();
        }

        // derive_resume_index should return 2 (last index was 1, so next is 2).
        let idx = derive_resume_index(&jsonl_path);
        assert_eq!(idx, 2, "resume index should follow last recorded index");

        // read_all_iterations should round-trip both records.
        let records = read_all_iterations(&jsonl_path).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].index, 0);
        assert_eq!(records[0].outcome, IterationOutcome::Ok);
        assert_eq!(records[1].index, 1);
        assert_eq!(records[1].outcome, IterationOutcome::Timeout);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ovmf_resolution_fails_when_explicit_path_missing() {
        let bogus = PathBuf::from("/nonexistent/OVMF_CODE.fd");
        let err = resolve_ovmf(Some(&bogus)).unwrap_err();
        assert!(format!("{err}").contains("OVMF_CODE.fd"));
    }
}
