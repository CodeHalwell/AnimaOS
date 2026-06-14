//! The Unsloth GPU trainer — a clearly-marked, feature-gated **skeleton**.
//!
//! This is the real, first-party adaptation backend's *seam* (S8.4.2). Actual
//! training is **external**: an Unsloth/HF-PEFT process (Python, torch, CUDA)
//! that runs the LoRA/QLoRA/HRA optimisation and the merge/quantisation pipeline
//! (S8.4.5/6), then writes an adapter + merged GGUF to disk. None of that can run
//! in CI, so this Rust type only models the seam and never trains in-process.
//!
//! ## Behaviour
//!
//! - **Without `--features live`:** [`UnslothFineTuner::fine_tune`] always returns
//!   [`FineTuneError::BackendUnavailable`] — the backend is compiled out.
//! - **With `--features live`:** it probes for the external runtime (via the
//!   `ANIMA_UNSLOTH_HOME` environment variable as the stand-in precondition) and
//!   still returns [`FineTuneError::BackendUnavailable`] when the runtime/env is
//!   absent. Wiring the actual subprocess call is left to the deployment that
//!   ships the Python side; the TODO marks exactly where it goes.

use crate::artifact::{AdapterArtifact, AdapterFormat, Provenance};
use crate::dataset::TrainingPair;
use crate::error::FineTuneError;
use crate::method::AdaptationMethod;
use crate::tuner::{FineTuneConfig, FineTuner, FineTunerCapabilities};
use serde::{Deserialize, Serialize};

/// Environment variable that must point at an installed Unsloth/PEFT runtime for
/// the `live` backend to attempt a real training run.
pub const UNSLOTH_HOME_ENV: &str = "ANIMA_UNSLOTH_HOME";

/// Optional override of the Python entrypoint path. When unset the `live`
/// backend uses `$ANIMA_UNSLOTH_HOME/finetune_entrypoint.py`.
pub const UNSLOTH_ENTRYPOINT_ENV: &str = "ANIMA_UNSLOTH_ENTRYPOINT";

/// Default entrypoint filename, resolved relative to [`UNSLOTH_HOME_ENV`].
pub const DEFAULT_ENTRYPOINT_FILE: &str = "finetune_entrypoint.py";

/// Schema version stamped into the job spec. Bumped if the Rust↔Python contract
/// changes incompatibly so the entrypoint can reject specs it cannot read.
pub const JOB_SPEC_VERSION: u32 = 1;

/// The real Unsloth-backed [`FineTuner`] (skeleton; see module docs).
#[derive(Debug, Clone, Default)]
pub struct UnslothFineTuner {
    _private: (),
}

impl UnslothFineTuner {
    /// Construct the backend handle. Construction always succeeds; availability
    /// is determined per-call so callers can probe [`FineTuner::capabilities`].
    pub fn new() -> Self {
        UnslothFineTuner { _private: () }
    }

    /// Whether the real backend is usable right now: requires the `live` feature
    /// **and** the external runtime to be present.
    pub fn is_available() -> bool {
        cfg!(feature = "live") && runtime_present()
    }

    /// The reason the backend is unavailable, or `None` if it is available.
    fn unavailable_reason() -> Option<String> {
        if !cfg!(feature = "live") {
            return Some(
                "crate built without the `live` feature; real Unsloth training is compiled out"
                    .to_string(),
            );
        }
        if !runtime_present() {
            return Some(format!(
                "external Unsloth/PEFT runtime not found (set `{UNSLOTH_HOME_ENV}` to its install path)"
            ));
        }
        None
    }
}

impl FineTuner for UnslothFineTuner {
    fn id(&self) -> &str {
        "unsloth"
    }

    fn capabilities(&self) -> FineTunerCapabilities {
        FineTunerCapabilities {
            real_training: true,
            // The full method matrix the external Unsloth/PEFT backend targets
            // (S8.4.4/.5). Listing them here documents intent even though the
            // skeleton does not execute them.
            supported_methods: vec![
                "lora".to_string(),
                "qlora".to_string(),
                "hra:hyperadapt".to_string(),
                "hra:ohora".to_string(),
                "hra:hira".to_string(),
                "hra:boha".to_string(),
                "hra:hrp".to_string(),
                "full".to_string(),
            ],
            cpu_only: false,
        }
    }

    fn fine_tune(
        &self,
        config: &FineTuneConfig,
        pairs: &[TrainingPair],
    ) -> Result<AdapterArtifact, FineTuneError> {
        // Validate eagerly so config/data errors surface even on systems where
        // the backend could run.
        config.validate()?;
        if pairs.is_empty() {
            return Err(FineTuneError::EmptyDataset);
        }
        let _ = method_is_targetable(&config.method);

        if let Some(reason) = Self::unavailable_reason() {
            return Err(FineTuneError::BackendUnavailable {
                backend: "unsloth".to_string(),
                reason,
            });
        }

        // Reachable only with `--features live` AND the runtime present.
        run_external_training(config, pairs)
    }
}

/// Whether the external runtime precondition is satisfied. Without `live` this is
/// irrelevant (the feature gate already short-circuits), so it is only meaningful
/// in a `live` build.
fn runtime_present() -> bool {
    std::env::var_os(UNSLOTH_HOME_ENV).is_some()
}

/// Methods the Unsloth backend intends to support. Always `true` for the abstract
/// matrix today; kept as a seam for a future capability check.
fn method_is_targetable(_method: &AdaptationMethod) -> bool {
    true
}

// ---------------------------------------------------------------------------
// Rust ↔ Python contract (PURE, default compile path — unit-testable in CI).
//
// The Rust side serialises a `JobSpec` to JSON and hands it to the external
// Python entrypoint (`cortex/finetune_entrypoint.py`). The entrypoint runs the
// S8.4.5 adaptation + S8.4.6 merge/quant pipeline and prints a `TrainingResult`
// JSON object on stdout. Rust parses that back into an `AdapterArtifact`.
//
// The exact same contract is documented in `cortex/finetune_entrypoint.py`.
// Keep the two in sync.
//
// Job spec (Rust → Python), JSON:
//   {
//     "version": 1,
//     "config": <FineTuneConfig as serde JSON>,
//     "pairs":  [ {"prompt": "...", "response": "..."}, ... ]
//   }
//
// Training result (Python → Rust), JSON on stdout:
//   {
//     "adapter_id":     "string",   // stable id of the produced adapter
//     "description":    "string",   // domain/description for task→adapter select
//     "format":         "lora_adapter" | "structural_transform" | "baked_gguf",
//     "merge_path":     "clean" | "hadamard",
//     "serving_tier":   "mountable_adapter" | "baked_variant",
//     "weights_digest": "string",   // digest of the adapter weights on disk
//     "adapter_path":   "string",   // filesystem path to the adapter artifacts
//     "merged_gguf_path": "string" | null,  // merged GGUF (baked variants), or null
//     "provenance": {
//       "base_model":    "string",
//       "method":        <AdaptationMethod as serde JSON>,
//       "source_job":    "string",
//       "created_at_ns": <u64>
//     },
//     "metrics": { ... }            // optional free-form metrics (ignored here)
//   }
// ---------------------------------------------------------------------------

/// The JSON job spec handed to the external Python entrypoint.
///
/// Pure data: built by [`build_job_spec`] on the default compile path so it is
/// unit-testable without the `live` feature, Python, or a GPU.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobSpec {
    /// Contract schema version (see [`JOB_SPEC_VERSION`]).
    pub version: u32,
    /// The full fine-tune configuration.
    pub config: FineTuneConfig,
    /// The training pairs to adapt on.
    pub pairs: Vec<TrainingPair>,
}

/// The provenance block echoed back by the entrypoint (mirrors [`Provenance`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultProvenance {
    /// Base model + quant the adapter was trained against.
    pub base_model: String,
    /// The adaptation method (and params) used.
    pub method: AdaptationMethod,
    /// Identifier of the job that produced this artifact.
    pub source_job: String,
    /// Wall-clock creation time (nanoseconds since the Unix epoch).
    pub created_at_ns: u64,
}

/// The JSON result the external entrypoint prints on stdout (see contract above).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrainingResult {
    /// Stable id of the produced adapter.
    pub adapter_id: String,
    /// Domain/description for later task→adapter selection.
    pub description: String,
    /// Serving format of the produced adapter.
    pub format: AdapterFormat,
    /// Merge path the method implied.
    pub merge_path: crate::method::MergePath,
    /// Library tier the artifact belongs to.
    pub serving_tier: crate::method::ServingTier,
    /// Digest of the adapter weights written to disk.
    pub weights_digest: String,
    /// Filesystem path to the produced adapter artifacts.
    pub adapter_path: String,
    /// Filesystem path to the merged GGUF (baked variants), or `None`.
    #[serde(default)]
    pub merged_gguf_path: Option<String>,
    /// Lineage / trust metadata.
    pub provenance: ResultProvenance,
    /// Optional free-form metrics; captured but not interpreted here.
    #[serde(default)]
    pub metrics: serde_json::Value,
}

/// Build the JSON-serialisable job spec from a config + training pairs.
///
/// PURE and side-effect free; compiled on the default (non-`live`) path so CI
/// can unit-test correct serialisation without Python or a GPU.
pub fn build_job_spec(config: &FineTuneConfig, pairs: &[TrainingPair]) -> JobSpec {
    JobSpec {
        version: JOB_SPEC_VERSION,
        config: config.clone(),
        pairs: pairs.to_vec(),
    }
}

/// Parse the external entrypoint's stdout into an [`AdapterArtifact`].
///
/// PURE: takes the captured stdout string and returns the assembled artifact or
/// a descriptive [`FineTuneError::ExternalBackend`]. Compiled on the default
/// path so it is unit-testable without the `live` feature. Only the last
/// non-empty line is parsed as JSON, so the entrypoint may emit human-readable
/// progress lines before the final result object.
pub fn parse_training_result(stdout: &str) -> Result<AdapterArtifact, FineTuneError> {
    let line = stdout
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .ok_or_else(|| FineTuneError::ExternalBackend {
            backend: "unsloth".to_string(),
            message: "entrypoint produced no output to parse as a training result".to_string(),
        })?;

    let result: TrainingResult =
        serde_json::from_str(line).map_err(|e| FineTuneError::ExternalBackend {
            backend: "unsloth".to_string(),
            message: format!("could not parse training result JSON ({e}); got: {line}"),
        })?;

    if result.adapter_id.is_empty() {
        return Err(FineTuneError::ExternalBackend {
            backend: "unsloth".to_string(),
            message: "training result is missing a non-empty `adapter_id`".to_string(),
        });
    }
    if result.adapter_path.is_empty() {
        return Err(FineTuneError::ExternalBackend {
            backend: "unsloth".to_string(),
            message: "training result is missing a non-empty `adapter_path`".to_string(),
        });
    }

    Ok(AdapterArtifact {
        adapter_id: result.adapter_id,
        description: result.description,
        format: result.format,
        merge_path: result.merge_path,
        serving_tier: result.serving_tier,
        weights_digest: result.weights_digest,
        adapter_path: Some(result.adapter_path),
        merged_gguf_path: result.merged_gguf_path,
        provenance: Provenance {
            base_model: result.provenance.base_model,
            method: result.provenance.method,
            source_job: result.provenance.source_job,
            created_at_ns: result.provenance.created_at_ns,
        },
    })
}

/// Resolve the Python entrypoint path: `$ANIMA_UNSLOTH_ENTRYPOINT` if set,
/// otherwise `$ANIMA_UNSLOTH_HOME/finetune_entrypoint.py`.
///
/// Pure given the two inputs, so the resolution rule is testable in CI.
///
/// Only invoked from the `live` `run_external_training` and from unit tests, so
/// the default non-test build (without `live`) never calls it.
#[cfg_attr(not(any(feature = "live", test)), allow(dead_code))]
fn resolve_entrypoint(
    home: Option<&str>,
    override_path: Option<&str>,
) -> Option<std::path::PathBuf> {
    if let Some(p) = override_path {
        if !p.is_empty() {
            return Some(std::path::PathBuf::from(p));
        }
    }
    let home = home?;
    if home.is_empty() {
        return None;
    }
    Some(std::path::Path::new(home).join(DEFAULT_ENTRYPOINT_FILE))
}

#[cfg(feature = "live")]
fn run_external_training(
    config: &FineTuneConfig,
    pairs: &[TrainingPair],
) -> Result<AdapterArtifact, FineTuneError> {
    let unique = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let tmp_root = std::env::temp_dir().join(format!("anima-finetune-{unique}"));
    let spec_path = tmp_root.join("job_spec.json");
    let out_dir = tmp_root.join("out");

    // Run the external job. On success the produced artifacts remain under
    // `out_dir` and are referenced by `AdapterArtifact::adapter_path`; on any
    // failure the whole work dir is removed below so nothing leaks (S8.4.5/6).
    let result = run_external_job(config, pairs, &spec_path, &out_dir);

    // The job spec is a transient input — always remove it.
    let _ = std::fs::remove_file(&spec_path);
    match result {
        // Keep `out_dir`: it holds the produced adapter/GGUF that
        // `artifact.adapter_path` points at; the caller owns its lifecycle.
        Ok(artifact) => Ok(artifact),
        // Nothing usable was produced — remove the entire work dir.
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp_root);
            Err(e)
        }
    }
}

/// The fallible body of a `live` training run, factored out so the caller can
/// clean up the temporary work dir regardless of which step fails.
#[cfg(feature = "live")]
fn run_external_job(
    config: &FineTuneConfig,
    pairs: &[TrainingPair],
    spec_path: &std::path::Path,
    out_dir: &std::path::Path,
) -> Result<AdapterArtifact, FineTuneError> {
    use std::io::Write;
    use std::process::Command;

    // 1. Build + serialise the job spec to the temp file.
    let spec = build_job_spec(config, pairs);
    let spec_json =
        serde_json::to_vec_pretty(&spec).map_err(|e| FineTuneError::ExternalBackend {
            backend: "unsloth".to_string(),
            message: format!("failed to serialise job spec: {e}"),
        })?;
    std::fs::create_dir_all(out_dir).map_err(|e| FineTuneError::ExternalBackend {
        backend: "unsloth".to_string(),
        message: format!("failed to create work dir {}: {e}", out_dir.display()),
    })?;
    {
        let mut f =
            std::fs::File::create(spec_path).map_err(|e| FineTuneError::ExternalBackend {
                backend: "unsloth".to_string(),
                message: format!(
                    "failed to create job spec file {}: {e}",
                    spec_path.display()
                ),
            })?;
        f.write_all(&spec_json)
            .map_err(|e| FineTuneError::ExternalBackend {
                backend: "unsloth".to_string(),
                message: format!("failed to write job spec: {e}"),
            })?;
    }

    // 2. Resolve the entrypoint under ANIMA_UNSLOTH_HOME (or its override).
    let home = std::env::var(UNSLOTH_HOME_ENV).ok();
    let override_path = std::env::var(UNSLOTH_ENTRYPOINT_ENV).ok();
    let entrypoint = resolve_entrypoint(home.as_deref(), override_path.as_deref()).ok_or_else(
        || FineTuneError::ExternalBackend {
            backend: "unsloth".to_string(),
            message: format!(
                "could not resolve Python entrypoint (set `{UNSLOTH_HOME_ENV}` or `{UNSLOTH_ENTRYPOINT_ENV}`)"
            ),
        },
    )?;

    // 3. Spawn `python3 <entrypoint> <spec_path> <out_dir>`, capturing output.
    let output = Command::new("python3")
        .arg(&entrypoint)
        .arg(spec_path)
        .arg(out_dir)
        .output()
        .map_err(|e| FineTuneError::ExternalBackend {
            backend: "unsloth".to_string(),
            message: format!("failed to spawn `python3 {}`: {e}", entrypoint.display()),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let excerpt: String = stderr.chars().take(2000).collect();
        return Err(FineTuneError::ExternalBackend {
            backend: "unsloth".to_string(),
            message: format!(
                "entrypoint exited with {} ; stderr: {excerpt}",
                output.status
            ),
        });
    }

    // 4. Parse the printed result into a real AdapterArtifact.
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_training_result(&stdout)
}

#[cfg(not(feature = "live"))]
fn run_external_training(
    _config: &FineTuneConfig,
    _pairs: &[TrainingPair],
) -> Result<AdapterArtifact, FineTuneError> {
    // Unreachable without `live` (the unavailable-reason check returns first),
    // but provided so the function resolves in the hermetic build.
    Err(FineTuneError::BackendUnavailable {
        backend: "unsloth".to_string(),
        reason: "feature `live` not enabled".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> FineTuneConfig {
        FineTuneConfig::new("base-q4", "ds", "out")
    }

    fn pairs() -> Vec<TrainingPair> {
        vec![TrainingPair::new("q", "a")]
    }

    #[test]
    fn id_is_unsloth() {
        assert_eq!(UnslothFineTuner::new().id(), "unsloth");
    }

    #[test]
    fn capabilities_declare_real_training() {
        let caps = UnslothFineTuner::new().capabilities();
        assert!(caps.real_training);
        assert!(!caps.cpu_only);
        assert!(caps
            .supported_methods
            .contains(&"hra:hyperadapt".to_string()));
    }

    #[test]
    fn fine_tune_reports_backend_unavailable() {
        // In the default (non-live) CI build this is always unavailable.
        let t = UnslothFineTuner::new();
        match t.fine_tune(&config(), &pairs()) {
            Err(FineTuneError::BackendUnavailable { backend, .. }) => {
                assert_eq!(backend, "unsloth");
            }
            other => panic!("expected BackendUnavailable, got {other:?}"),
        }
    }

    #[test]
    fn config_and_data_errors_surface_before_availability() {
        let t = UnslothFineTuner::new();
        // Empty dataset is rejected regardless of backend availability.
        assert_eq!(
            t.fine_tune(&config(), &[]),
            Err(FineTuneError::EmptyDataset)
        );
    }

    #[cfg(not(feature = "live"))]
    #[test]
    fn not_available_without_live_feature() {
        assert!(!UnslothFineTuner::is_available());
    }

    // --- Pure contract tests (run on the default, non-live CI path) ---------

    #[test]
    fn build_job_spec_serialises_config_and_pairs() {
        let cfg = config().with_description("grade-school maths");
        let ps = vec![
            TrainingPair::new("what is 2+2?", "4"),
            TrainingPair::new("capital of France?", "Paris"),
        ];
        let spec = build_job_spec(&cfg, &ps);

        assert_eq!(spec.version, JOB_SPEC_VERSION);
        assert_eq!(spec.pairs.len(), 2);
        assert_eq!(spec.pairs[0].prompt, "what is 2+2?");
        assert_eq!(spec.pairs[1].response, "Paris");
        assert_eq!(spec.config, cfg);

        // The spec must serialise to JSON the Python entrypoint can read back.
        let json = serde_json::to_value(&spec).expect("serialise job spec");
        assert_eq!(json["version"], JOB_SPEC_VERSION);
        assert_eq!(json["pairs"][0]["prompt"], "what is 2+2?");
        assert_eq!(json["pairs"][1]["response"], "Paris");
        assert_eq!(json["config"]["base_model"], "base-q4");
        // Method is internally tagged (default QLoRA; snake_case = "q_lora").
        assert_eq!(json["config"]["method"]["kind"], "q_lora");

        // Round-trips back to an identical spec.
        let back: JobSpec = serde_json::from_value(json).expect("deserialise job spec");
        assert_eq!(back, spec);
    }

    fn sample_result_json() -> String {
        // Mirrors exactly what `cortex/finetune_entrypoint.py` prints.
        serde_json::json!({
            "adapter_id": "maths-deadbeef",
            "description": "grade-school maths",
            "format": "lora_adapter",
            "merge_path": "clean",
            "serving_tier": "mountable_adapter",
            "weights_digest": "cafef00d",
            "adapter_path": "/work/out/adapter",
            "merged_gguf_path": serde_json::Value::Null,
            "provenance": {
                "base_model": "base-q4",
                "method": {"kind": "q_lora", "rank": 16, "alpha": 32, "base_bits": 4},
                "source_job": "maths",
                "created_at_ns": 12345u64
            },
            "metrics": {"loss": 0.1}
        })
        .to_string()
    }

    #[test]
    fn parse_training_result_valid_json_builds_artifact() {
        let art = parse_training_result(&sample_result_json()).expect("parse result");
        assert_eq!(art.adapter_id, "maths-deadbeef");
        assert_eq!(art.description, "grade-school maths");
        assert_eq!(art.format, AdapterFormat::LoraAdapter);
        assert_eq!(art.merge_path, crate::method::MergePath::Clean);
        assert_eq!(
            art.serving_tier,
            crate::method::ServingTier::MountableAdapter
        );
        assert_eq!(art.weights_digest, "cafef00d");
        assert_eq!(art.provenance.base_model, "base-q4");
        assert_eq!(art.provenance.source_job, "maths");
        assert_eq!(art.provenance.created_at_ns, 12345);
        assert!(art.is_mountable());
    }

    #[test]
    fn parse_training_result_ignores_leading_progress_lines() {
        let stdout = format!(
            "loading base model...\ntraining step 1/60\nmerging adapter...\n{}\n",
            sample_result_json()
        );
        let art = parse_training_result(&stdout).expect("parse last line");
        assert_eq!(art.adapter_id, "maths-deadbeef");
    }

    #[test]
    fn parse_training_result_empty_output_is_error() {
        match parse_training_result("   \n\n") {
            Err(FineTuneError::ExternalBackend { backend, message }) => {
                assert_eq!(backend, "unsloth");
                assert!(message.contains("no output"), "got: {message}");
            }
            other => panic!("expected ExternalBackend, got {other:?}"),
        }
    }

    #[test]
    fn parse_training_result_malformed_json_is_error() {
        match parse_training_result("{not valid json") {
            Err(FineTuneError::ExternalBackend { message, .. }) => {
                assert!(message.contains("could not parse"), "got: {message}");
            }
            other => panic!("expected ExternalBackend, got {other:?}"),
        }
    }

    #[test]
    fn parse_training_result_missing_field_is_error() {
        // Drop the required `adapter_path` field entirely.
        let json = serde_json::json!({
            "adapter_id": "x",
            "description": "d",
            "format": "lora_adapter",
            "merge_path": "clean",
            "serving_tier": "mountable_adapter",
            "weights_digest": "w",
            "provenance": {
                "base_model": "b",
                "method": {"kind": "full_fine_tune"},
                "source_job": "j",
                "created_at_ns": 0u64
            }
        })
        .to_string();
        match parse_training_result(&json) {
            Err(FineTuneError::ExternalBackend { message, .. }) => {
                assert!(message.contains("could not parse"), "got: {message}");
            }
            other => panic!("expected ExternalBackend, got {other:?}"),
        }
    }

    #[test]
    fn parse_training_result_empty_adapter_id_is_error() {
        let json = serde_json::json!({
            "adapter_id": "",
            "description": "d",
            "format": "lora_adapter",
            "merge_path": "clean",
            "serving_tier": "mountable_adapter",
            "weights_digest": "w",
            "adapter_path": "/p",
            "provenance": {
                "base_model": "b",
                "method": {"kind": "full_fine_tune"},
                "source_job": "j",
                "created_at_ns": 0u64
            }
        })
        .to_string();
        match parse_training_result(&json) {
            Err(FineTuneError::ExternalBackend { message, .. }) => {
                assert!(message.contains("adapter_id"), "got: {message}");
            }
            other => panic!("expected ExternalBackend, got {other:?}"),
        }
    }

    #[test]
    fn resolve_entrypoint_prefers_override() {
        let p = resolve_entrypoint(Some("/home/unsloth"), Some("/custom/ep.py"));
        assert_eq!(p, Some(std::path::PathBuf::from("/custom/ep.py")));
    }

    #[test]
    fn resolve_entrypoint_falls_back_to_home_join() {
        let p = resolve_entrypoint(Some("/home/unsloth"), None);
        assert_eq!(
            p,
            Some(std::path::Path::new("/home/unsloth").join(DEFAULT_ENTRYPOINT_FILE))
        );
        // Empty override is treated as unset.
        let p2 = resolve_entrypoint(Some("/home/unsloth"), Some(""));
        assert_eq!(
            p2,
            Some(std::path::Path::new("/home/unsloth").join(DEFAULT_ENTRYPOINT_FILE))
        );
    }

    #[test]
    fn resolve_entrypoint_none_when_unresolvable() {
        assert_eq!(resolve_entrypoint(None, None), None);
        assert_eq!(resolve_entrypoint(Some(""), None), None);
    }
}
