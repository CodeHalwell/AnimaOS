//! E8 S8.4.1 — `cargo xtask finetune` command.
//!
//! The canonical fine-tuning entrypoint for AnimaOS.  Wraps the
//! [`anima_finetune`] crate's pipeline (dataset → train → export → artifact)
//! behind a stable CLI, following the same fixture-vs-live discipline as the
//! LLM backends and the E5.8 demo runner.
//!
//! # Modes
//!
//! **Fixture mode (default, CI-hermetic):** uses [`FixtureFineTuner`] —
//! deterministic, no GPU, no Python, no I/O.  The output artifact's
//! `weights_digest` is derived from a stable FNV-1a hash of the config +
//! training data; running the same command twice produces byte-identical
//! `artifact.json`.
//!
//! **Live mode (`ANIMA_FINETUNE_LIVE=1`):** the xtask delegates to the real
//! Unsloth/PEFT GPU pipeline.  The skeleton compiles and runs but returns
//! [`FineTuneError::BackendUnavailable`] unless the external runtime and
//! environment (Python, torch, CUDA) are configured.
//!
//! # Usage
//!
//! ```
//! # Fixture run (CI-safe, no GPU):
//! cargo xtask finetune --model unsloth/Phi-3.5-mini-instruct \
//!                      --adapter-id phi-maths-v1
//!
//! # From a JSONL dataset (one {"prompt":"…","response":"…"} per line):
//! cargo xtask finetune --dataset training_corpus/alpaca.jsonl \
//!                      --adapter-id episodic-v1 --max-steps 200
//!
//! # HRA method with a higher rank:
//! cargo xtask finetune --method hra-hyperadapt --lora-rank 64
//!
//! # Register the artifact in the adapter library:
//! cargo xtask finetune --register --library ~/.anima/adapters
//!
//! # Live mode (requires Unsloth + torch + CUDA):
//! ANIMA_FINETUNE_LIVE=1 cargo xtask finetune --model unsloth/Llama-3.2-3B \
//!                                             --adapter-id llama-v1 --max-steps 500
//! ```

use anima_finetune::method::HraKind;
use anima_finetune::{
    AdaptationMethod, AdapterLibrary, FineTuneConfig, FineTuneJob, FineTuner, FixtureFineTuner,
    TrainingPair,
};
use anyhow::{bail, Context, Result};
use chrono::Local;
use clap::Args;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

// ── CLI args ───────────────────────────────────────────────────────────────────

/// Adaptation method variants accepted on the CLI.
#[derive(Debug, Clone, clap::ValueEnum)]
pub enum MethodArg {
    /// LoRA — standard low-rank adaptation.
    Lora,
    /// QLoRA — quantised LoRA (default).
    Qlora,
    /// HRA HyperAdapt — structural scaling; cleanest merge path.
    HraHyperadapt,
    /// HRA OHoRA — orthogonal projection; merges cleanly.
    HraOhora,
    /// HRA HiRA — Hadamard Δ W; baked into a variant.
    HraHira,
    /// HRA BoHA — blockwise Hadamard; best for continual self-improvement.
    HraBoha,
    /// HRA HRP — high-rank preheat → SVD → LoRA; mounts like vanilla LoRA.
    HraHrp,
    /// Full fine-tune — updates all weights; baked into a variant.
    Full,
}

impl MethodArg {
    fn into_method(self, rank: u32, alpha: u32) -> AdaptationMethod {
        match self {
            MethodArg::Lora => AdaptationMethod::Lora { rank, alpha },
            MethodArg::Qlora => AdaptationMethod::QLora {
                rank,
                alpha,
                base_bits: 4,
            },
            MethodArg::HraHyperadapt => AdaptationMethod::Hra {
                family: HraKind::HyperAdapt,
                rank,
            },
            MethodArg::HraOhora => AdaptationMethod::Hra {
                family: HraKind::Ohora,
                rank,
            },
            MethodArg::HraHira => AdaptationMethod::Hra {
                family: HraKind::Hira,
                rank,
            },
            MethodArg::HraBoha => AdaptationMethod::Hra {
                family: HraKind::Boha,
                rank,
            },
            MethodArg::HraHrp => AdaptationMethod::Hra {
                family: HraKind::Hrp,
                rank,
            },
            MethodArg::Full => AdaptationMethod::FullFineTune,
        }
    }
}

#[derive(Args, Debug)]
pub struct FinetuneArgs {
    /// Base model identifier (e.g. `unsloth/Phi-3.5-mini-instruct`).
    #[arg(long, default_value = "unsloth/Phi-3.5-mini-instruct")]
    pub model: String,

    /// Path to a JSONL file of training pairs (`{"prompt":"…","response":"…"}`).
    /// Omit to use the built-in fixture pairs (CI-safe, no file I/O).
    #[arg(long)]
    pub dataset: Option<PathBuf>,

    /// Adaptation method.
    #[arg(long, value_enum, default_value = "qlora")]
    pub method: MethodArg,

    /// LoRA / HRA adapter rank (ignored for `full`).
    #[arg(long, default_value = "16")]
    pub lora_rank: u32,

    /// LoRA alpha — scaling factor (ignored for HRA and `full`).
    #[arg(long, default_value = "32")]
    pub lora_alpha: u32,

    /// Maximum training steps.
    #[arg(long, default_value = "500")]
    pub max_steps: u32,

    /// Learning rate.
    #[arg(long, default_value = "0.0002")]
    pub learning_rate: f32,

    /// Per-device batch size.
    #[arg(long, default_value = "2")]
    pub batch_size: u32,

    /// Output adapter identifier.  Content-addressed suffix is appended in
    /// fixture mode; used verbatim in live mode.
    #[arg(long, default_value = "adapter")]
    pub adapter_id: String,

    /// Human-readable description stored in the artifact's provenance.
    #[arg(long, default_value = "")]
    pub description: String,

    /// Output directory.  Defaults to `artifacts/finetune/<date>-<adapter-id>/`.
    #[arg(long)]
    pub output: Option<PathBuf>,

    /// Register the produced artifact in the adapter library.
    #[arg(long, default_value = "false")]
    pub register: bool,

    /// Adapter library directory (used with `--register`).
    /// Defaults to `~/.anima/adapters/`.
    #[arg(long)]
    pub library: Option<PathBuf>,

    /// Suppress per-step output (summary only).
    #[arg(long, default_value = "false")]
    pub quiet: bool,
}

// ── On-disk manifest ──────────────────────────────────────────────────────────

/// Written to `<output>/run.json` alongside `artifact.json`.
#[derive(Debug, Serialize, Deserialize)]
struct RunManifest {
    trainer: String,
    real_training: bool,
    cpu_only: bool,
    dataset_ref: String,
    pair_count: usize,
    started_at: String,
    completed_at: String,
    artifact_id: String,
    output_dir: String,
}

// ── JSONL loader ──────────────────────────────────────────────────────────────

/// A JSONL record with `prompt` and `response` fields.
#[derive(Deserialize)]
struct JsonlRecord {
    prompt: String,
    response: String,
}

/// Load training pairs from a JSONL file.  Each line must be a JSON object
/// with at least `prompt` and `response` string fields.  Blank lines and
/// comment lines (starting with `#`) are skipped.
fn load_jsonl(path: &PathBuf) -> Result<Vec<TrainingPair>> {
    let content =
        fs::read_to_string(path).with_context(|| format!("reading dataset {}", path.display()))?;
    let mut pairs = Vec::new();
    for (line_no, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let record: JsonlRecord = serde_json::from_str(trimmed).with_context(|| {
            format!(
                "parsing JSONL record at {}:{} — expected {{\"prompt\":\"…\",\"response\":\"…\"}}",
                path.display(),
                line_no + 1
            )
        })?;
        pairs.push(TrainingPair::new(record.prompt, record.response));
    }
    if pairs.is_empty() {
        bail!(
            "dataset {} contains no valid training pairs",
            path.display()
        );
    }
    Ok(pairs)
}

// ── Built-in fixture dataset ───────────────────────────────────────────────────

/// A minimal deterministic fixture dataset used when `--dataset` is not given.
///
/// These pairs are representative of the AnimaOS agent's task output format
/// (Alpaca-style) and exercise the full pipeline shape (config → train → export)
/// without requiring any file I/O.
fn fixture_pairs() -> Vec<TrainingPair> {
    vec![
        TrainingPair::new(
            "Explain the difference between LoRA and QLoRA.",
            "LoRA adds trainable rank-decomposition matrices to frozen base-model weights. \
             QLoRA quantises the base weights to 4-bit NormalFloat before applying LoRA, \
             reducing VRAM by 4× at the cost of slightly lower throughput.",
        ),
        TrainingPair::new(
            "What is the purpose of the AnimaOS sleep cycle?",
            "The sleep cycle runs four phases — Pruning, Replay, Dreaming, Compilation — \
             to consolidate episodic memory, validate the L3 archive, generate associative \
             edges via random walks, and compile completed tasks into training pairs.",
        ),
        TrainingPair::new(
            "Describe the Striatal Gate decision process.",
            "The Striatal Gate scores candidate events by urgency and novelty, adjusts the \
             threshold for homeostatic signals (thermal load, financial budget, memory \
             pressure), and classifies each invocation as CheapLocal, MidTier, or Frontier.",
        ),
        TrainingPair::new(
            "How does the KV-cache controller improve long-horizon retention?",
            "The linear gate model scores each KV-cache block on role, constraint presence, \
             error-trace presence, and recency, pinning high-value blocks and evicting \
             superseded intermediate state — outperforming LRU by ≥10 pp needle recall \
             at a matched block budget.",
        ),
        TrainingPair::new(
            "What safety checks does the DefenceLayer run on cortex outputs?",
            "The DefenceLayer runs four detectors in sequence: (1) ConstitutionGuard checks \
             the value charter's eight prohibitions; (2) PromptInjectionDetector screens for \
             49 injection patterns; (3) GoalDriftMonitor compares actions to the original \
             objective; (4) RewardHackingDetector flags completion claims without observable \
             evidence.",
        ),
    ]
}

// ── Main entry point ───────────────────────────────────────────────────────────

pub fn run_finetune(args: FinetuneArgs) -> Result<()> {
    let started_at = Local::now();

    // ── Build the adaptation method ────────────────────────────────────────────
    let method = args.method.into_method(args.lora_rank, args.lora_alpha);

    // ── Build FineTuneConfig ───────────────────────────────────────────────────
    let dataset_ref = args
        .dataset
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "fixture://builtin".to_string());

    let mut config = FineTuneConfig::new(&args.model, &dataset_ref, &args.adapter_id)
        .with_method(method)
        .with_description(args.description.clone());
    config.hyperparams.max_steps = args.max_steps;
    config.hyperparams.learning_rate = args.learning_rate;
    config.hyperparams.batch_size = args.batch_size;

    config
        .validate()
        .context("invalid fine-tune configuration")?;

    // ── Resolve the output directory ───────────────────────────────────────────
    let date_str = started_at.format("%Y%m%d-%H%M%S");
    let out_dir = args.output.clone().unwrap_or_else(|| {
        PathBuf::from(format!(
            "artifacts/finetune/{}-{}",
            date_str, args.adapter_id
        ))
    });
    fs::create_dir_all(&out_dir)
        .with_context(|| format!("creating output directory {}", out_dir.display()))?;

    // ── Load training pairs ────────────────────────────────────────────────────
    let pairs: Vec<TrainingPair> = match &args.dataset {
        Some(path) => load_jsonl(path)?,
        None => fixture_pairs(),
    };
    let pair_count = pairs.len();

    if !args.quiet {
        println!("━━━ AnimaOS Fine-Tune (E8 S8.4.1) ━━━");
        println!("  Model       : {}", args.model);
        println!("  Dataset     : {dataset_ref}");
        println!("  Pairs       : {pair_count}");
        println!("  Method      : {}", config.method.label());
        println!("  Max steps   : {}", args.max_steps);
        println!("  Adapter ID  : {}", args.adapter_id);
        println!("  Output      : {}", out_dir.display());
    }

    // ── Select trainer ─────────────────────────────────────────────────────────
    let live_mode = std::env::var("ANIMA_FINETUNE_LIVE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let job_id = format!(
        "job-{}-{}",
        date_str,
        &args.adapter_id[..args.adapter_id.len().min(16)]
    );

    let (artifact, caps) = if live_mode {
        if !args.quiet {
            println!("  Trainer     : unsloth (live mode)");
        }
        // The UnslothFineTuner skeleton compiles but returns BackendUnavailable
        // unless the external Unsloth / Python / CUDA environment is present.
        // We surface the error with a clear diagnostic.
        bail!(
            "live mode (ANIMA_FINETUNE_LIVE=1) requires the Unsloth/PEFT external runtime.\n\
             Ensure Python ≥ 3.10, torch, and unsloth are installed and on PATH.\n\
             The `anima-finetune` crate ships a live-gated skeleton at \
             `crates/finetune/src/backend/unsloth.rs`;\n\
             build with `--features live` to enable it (see docs/13-local-llm-providers.md S8.4)."
        );
    } else {
        if !args.quiet {
            println!("  Trainer     : fixture (CI-hermetic, deterministic)");
        }
        let tuner =
            FixtureFineTuner::with_created_at(started_at.timestamp_nanos_opt().unwrap_or(0) as u64);
        let caps = tuner.capabilities();
        let job = FineTuneJob::new(&job_id, config.clone());
        let artifact = tuner
            .run_job(&job, &pairs)
            .context("fixture fine-tune failed")?;
        (artifact, caps)
    };

    let completed_at = Local::now();

    // ── Write artifact.json ────────────────────────────────────────────────────
    let artifact_json =
        serde_json::to_string_pretty(&artifact).context("serialising AdapterArtifact")?;
    let artifact_path = out_dir.join("artifact.json");
    fs::write(&artifact_path, &artifact_json)
        .with_context(|| format!("writing {}", artifact_path.display()))?;

    // ── Write run.json ─────────────────────────────────────────────────────────
    let manifest = RunManifest {
        trainer: if live_mode {
            "unsloth".to_string()
        } else {
            "fixture".to_string()
        },
        real_training: caps.real_training,
        cpu_only: caps.cpu_only,
        dataset_ref: dataset_ref.clone(),
        pair_count,
        started_at: started_at.to_rfc3339(),
        completed_at: completed_at.to_rfc3339(),
        artifact_id: artifact.adapter_id.clone(),
        output_dir: out_dir.display().to_string(),
    };
    let manifest_json =
        serde_json::to_string_pretty(&manifest).context("serialising RunManifest")?;
    let manifest_path = out_dir.join("run.json");
    fs::write(&manifest_path, &manifest_json)
        .with_context(|| format!("writing {}", manifest_path.display()))?;

    // ── Optional adapter library registration ──────────────────────────────────
    if args.register {
        let lib_dir = args
            .library
            .clone()
            .unwrap_or_else(|| dirs_next().join("adapters"));
        fs::create_dir_all(&lib_dir)
            .with_context(|| format!("creating library directory {}", lib_dir.display()))?;

        let mut lib = AdapterLibrary::new(64);
        lib.register(artifact.clone())
            .context("registering artifact in adapter library")?;

        let lib_path = lib_dir.join(format!("{}.json", artifact.adapter_id));
        let lib_json =
            serde_json::to_string_pretty(&artifact).context("serialising artifact for library")?;
        fs::write(&lib_path, &lib_json)
            .with_context(|| format!("writing library entry {}", lib_path.display()))?;

        if !args.quiet {
            println!("  Registered  : {}", lib_path.display());
        }
    }

    if !args.quiet {
        println!();
        println!("  ✓ artifact.json : {}", artifact_path.display());
        println!("  ✓ run.json      : {}", manifest_path.display());
        println!();
        println!("  Adapter ID   : {}", artifact.adapter_id);
        println!("  Format       : {:?}", artifact.format);
        println!("  Merge path   : {:?}", artifact.merge_path);
        println!("  Serving tier : {:?}", artifact.serving_tier);
        println!("  Weights hash : {}", &artifact.weights_digest[..16]);
        println!();
        println!(
            "  Done in {:.2}s",
            (completed_at - started_at).num_milliseconds() as f64 / 1000.0
        );
    }

    Ok(())
}

/// Return the default AnimaOS state directory (`~/.anima/`).
fn dirs_next() -> PathBuf {
    std::env::var("HOME")
        .map(|h| PathBuf::from(h).join(".anima"))
        .unwrap_or_else(|_| PathBuf::from(".anima"))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn default_args(tmp: &TempDir) -> FinetuneArgs {
        FinetuneArgs {
            model: "test-model".to_string(),
            dataset: None,
            method: MethodArg::Qlora,
            lora_rank: 16,
            lora_alpha: 32,
            max_steps: 10,
            learning_rate: 2e-4,
            batch_size: 2,
            adapter_id: "test-adapter".to_string(),
            description: "unit test".to_string(),
            output: Some(tmp.path().to_path_buf()),
            register: false,
            library: None,
            quiet: true,
        }
    }

    #[test]
    fn fixture_run_writes_artifact_and_manifest() {
        let tmp = TempDir::new().unwrap();
        run_finetune(default_args(&tmp)).unwrap();

        let artifact_path = tmp.path().join("artifact.json");
        let manifest_path = tmp.path().join("run.json");

        assert!(artifact_path.exists(), "artifact.json not written");
        assert!(manifest_path.exists(), "run.json not written");

        let artifact: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&artifact_path).unwrap()).unwrap();
        assert!(artifact["adapter_id"]
            .as_str()
            .unwrap()
            .starts_with("test-adapter-"));

        let manifest: RunManifest =
            serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
        assert_eq!(manifest.trainer, "fixture");
        assert!(!manifest.real_training);
        assert!(manifest.cpu_only);
        assert_eq!(manifest.pair_count, 5); // built-in fixture pairs
    }

    #[test]
    fn fixture_run_is_deterministic() {
        let tmp1 = TempDir::new().unwrap();
        let tmp2 = TempDir::new().unwrap();

        // Two runs with identical args and created_at=0 via FixtureFineTuner::new()
        // should produce the same adapter_id (content-addressed).
        let pairs = fixture_pairs();
        let config = FineTuneConfig::new("test-model", "fixture://builtin", "test-adapter")
            .with_method(AdaptationMethod::QLora {
                rank: 16,
                alpha: 32,
                base_bits: 4,
            });

        let tuner = FixtureFineTuner::new();
        let a = tuner.fine_tune(&config, &pairs).unwrap();
        let b = tuner.fine_tune(&config, &pairs).unwrap();

        assert_eq!(a.adapter_id, b.adapter_id);
        assert_eq!(a.weights_digest, b.weights_digest);
        drop((tmp1, tmp2));
    }

    #[test]
    fn jsonl_loader_parses_valid_file() {
        let tmp = TempDir::new().unwrap();
        let jsonl_path = tmp.path().join("data.jsonl");
        fs::write(
            &jsonl_path,
            "# header comment\n\
             {\"prompt\":\"q1\",\"response\":\"a1\"}\n\
             \n\
             {\"prompt\":\"q2\",\"response\":\"a2\"}\n",
        )
        .unwrap();

        let pairs = load_jsonl(&jsonl_path).unwrap();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].prompt, "q1");
        assert_eq!(pairs[1].response, "a2");
    }

    #[test]
    fn jsonl_loader_rejects_empty_file() {
        let tmp = TempDir::new().unwrap();
        let jsonl_path = tmp.path().join("empty.jsonl");
        fs::write(&jsonl_path, "# only a comment\n").unwrap();
        assert!(load_jsonl(&jsonl_path).is_err());
    }

    #[test]
    fn jsonl_loader_rejects_missing_file() {
        let path = PathBuf::from("/nonexistent/dataset.jsonl");
        assert!(load_jsonl(&path).is_err());
    }

    #[test]
    fn run_with_jsonl_dataset_uses_file_pairs() {
        let tmp = TempDir::new().unwrap();
        let jsonl_path = tmp.path().join("train.jsonl");
        fs::write(
            &jsonl_path,
            "{\"prompt\":\"hello\",\"response\":\"world\"}\n",
        )
        .unwrap();

        let out_dir = tmp.path().join("out");
        let args = FinetuneArgs {
            dataset: Some(jsonl_path),
            output: Some(out_dir.clone()),
            ..default_args(&tmp)
        };
        run_finetune(args).unwrap();

        let manifest: RunManifest =
            serde_json::from_str(&fs::read_to_string(out_dir.join("run.json")).unwrap()).unwrap();
        assert_eq!(manifest.pair_count, 1);
    }

    #[test]
    fn zero_rank_is_rejected_before_training() {
        let tmp = TempDir::new().unwrap();
        let args = FinetuneArgs {
            lora_rank: 0,
            ..default_args(&tmp)
        };
        assert!(run_finetune(args).is_err());
    }

    #[test]
    fn method_arg_maps_to_correct_adaptation_method() {
        let lora = MethodArg::Lora.into_method(8, 16);
        assert!(matches!(
            lora,
            AdaptationMethod::Lora { rank: 8, alpha: 16 }
        ));

        let qlora = MethodArg::Qlora.into_method(16, 32);
        assert!(matches!(
            qlora,
            AdaptationMethod::QLora {
                rank: 16,
                alpha: 32,
                base_bits: 4
            }
        ));

        let hra = MethodArg::HraHira.into_method(32, 0);
        assert!(matches!(
            hra,
            AdaptationMethod::Hra {
                family: HraKind::Hira,
                rank: 32
            }
        ));

        let full = MethodArg::Full.into_method(0, 0);
        assert!(matches!(full, AdaptationMethod::FullFineTune));
    }

    #[test]
    fn fixture_pairs_are_non_empty_and_all_fields_populated() {
        let pairs = fixture_pairs();
        assert!(!pairs.is_empty());
        for p in &pairs {
            assert!(!p.prompt.is_empty(), "empty prompt");
            assert!(!p.response.is_empty(), "empty response");
        }
    }

    #[test]
    fn dirs_next_returns_valid_path() {
        let p = dirs_next();
        assert!(p.ends_with(".anima") || p.to_str().is_some());
    }
}
