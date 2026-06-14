//! S8.4.2 — The thin [`FineTuner`] provider abstraction + the CI-hermetic
//! [`FixtureFineTuner`].
//!
//! The report is explicit (S8.4.2) that this trait "exists purely so the
//! pipeline (dataset → train → export → eval → promote) is testable with a mock,
//! not to invite alternative trainers" — Unsloth is the default and only
//! first-party real impl. Accordingly this module ships:
//!
//! - [`FineTuneConfig`] / [`FineTuneJob`]: the standard config surface (base
//!   model, method, dataset reference, output adapter id, hyperparams).
//! - [`FineTuner`]: the trait, with [`FineTuner::fine_tune`] and an
//!   id/capabilities pair.
//! - [`FixtureFineTuner`]: a deterministic, side-effect-free implementation that
//!   derives a reproducible [`AdapterArtifact`] from a hash of its inputs — the
//!   default in tests and CI.
//!
//! The real GPU trainer lives behind a feature gate in
//! [`crate::backend::unsloth`]; constructing it is the *only* thing that changes
//! when moving from fixture to real training.

use crate::artifact::{AdapterArtifact, AdapterFormat, Provenance};
use crate::dataset::TrainingPair;
use crate::error::FineTuneError;
use crate::hash::{hex64, Fnv1a};
use crate::method::AdaptationMethod;
use serde::{Deserialize, Serialize};

/// Standard hyperparameters shared across methods (S8.4.1 config surface).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HyperParams {
    /// Maximum training steps.
    pub max_steps: u32,
    /// Learning rate.
    pub learning_rate: f32,
    /// Per-device batch size.
    pub batch_size: u32,
}

impl Default for HyperParams {
    fn default() -> Self {
        HyperParams {
            max_steps: 60,
            learning_rate: 2e-4,
            batch_size: 2,
        }
    }
}

impl HyperParams {
    fn hash_into(&self, h: &mut Fnv1a) {
        h.write_u64(self.max_steps as u64);
        // f32 bit pattern keeps the fingerprint exact and platform-stable.
        h.write_u64(self.learning_rate.to_bits() as u64);
        h.write_u64(self.batch_size as u64);
    }
}

/// The configuration for a single fine-tune (S8.4.1 / S8.4.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FineTuneConfig {
    /// Base model id + quant the adapter targets (e.g. `"qwen2.5-1.5b-q4"`).
    pub base_model: String,
    /// The adaptation method (default QLoRA; HRA selectable per S8.4.4/.5).
    pub method: AdaptationMethod,
    /// A reference to the dataset (a name/uri); the actual pairs are passed to
    /// [`FineTuner::fine_tune`]. Recorded for provenance.
    pub dataset_ref: String,
    /// Desired output adapter id. The trainer may use it verbatim or derive a
    /// content-addressed id (the fixture does the latter, seeded by this value).
    pub output_adapter_id: String,
    /// Human-readable domain/description for later task→adapter selection.
    pub description: String,
    /// Shared hyperparameters.
    pub hyperparams: HyperParams,
}

impl FineTuneConfig {
    /// A minimal config using the default method and hyperparameters.
    pub fn new(
        base_model: impl Into<String>,
        dataset_ref: impl Into<String>,
        output_adapter_id: impl Into<String>,
    ) -> Self {
        FineTuneConfig {
            base_model: base_model.into(),
            method: AdaptationMethod::default(),
            dataset_ref: dataset_ref.into(),
            output_adapter_id: output_adapter_id.into(),
            description: String::new(),
            hyperparams: HyperParams::default(),
        }
    }

    /// Builder-style override of the adaptation method.
    pub fn with_method(mut self, method: AdaptationMethod) -> Self {
        self.method = method;
        self
    }

    /// Builder-style override of the description (used for adapter selection).
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Validate config invariants independent of any backend.
    pub fn validate(&self) -> Result<(), FineTuneError> {
        let rank = match &self.method {
            AdaptationMethod::Lora { rank, .. }
            | AdaptationMethod::QLora { rank, .. }
            | AdaptationMethod::Hra { rank, .. } => Some(*rank),
            AdaptationMethod::FullFineTune => None,
        };
        if rank == Some(0) {
            return Err(FineTuneError::InvalidConfig {
                message: "adapter rank must be > 0".to_string(),
            });
        }
        if self.hyperparams.max_steps == 0 {
            return Err(FineTuneError::InvalidConfig {
                message: "max_steps must be > 0".to_string(),
            });
        }
        Ok(())
    }

    /// Absorb the whole config into a deterministic fingerprint.
    fn hash_into(&self, h: &mut Fnv1a) {
        h.write_str(&self.base_model);
        self.method.hash_into(h);
        h.write_str(&self.dataset_ref);
        h.write_str(&self.output_adapter_id);
        h.write_str(&self.description);
        self.hyperparams.hash_into(h);
    }
}

/// A descriptor of a submitted fine-tune run (S8.4.1).
///
/// Pairs a stable `job_id` with the config it ran. Recorded in artifact
/// [`Provenance`] so an adapter can be traced back to its run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FineTuneJob {
    /// Stable identifier for this run.
    pub job_id: String,
    /// The configuration that was (or will be) executed.
    pub config: FineTuneConfig,
}

impl FineTuneJob {
    /// Create a job from an explicit id and config.
    pub fn new(job_id: impl Into<String>, config: FineTuneConfig) -> Self {
        FineTuneJob {
            job_id: job_id.into(),
            config,
        }
    }
}

/// What a [`FineTuner`] backend can do (S8.4.2 capabilities surface).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FineTunerCapabilities {
    /// Whether this backend performs *real* training (false for the fixture).
    pub real_training: bool,
    /// Method labels (see [`AdaptationMethod::label`]) the backend supports.
    pub supported_methods: Vec<String>,
    /// Whether the backend can run without a GPU (true for the fixture).
    pub cpu_only: bool,
}

/// S8.4.2 — the thin provider abstraction for the dataset→train→export pipeline.
///
/// Implementors are constructed by the caller; everything downstream
/// ([`crate::library`], [`crate::eval`]) works against the returned
/// [`AdapterArtifact`] regardless of which trainer produced it.
pub trait FineTuner {
    /// Stable identifier of this trainer (e.g. `"fixture"`, `"unsloth"`).
    fn id(&self) -> &str;

    /// Declared capabilities, including which methods are supported.
    fn capabilities(&self) -> FineTunerCapabilities;

    /// Run a fine-tune over `pairs` per `config`, returning the resulting
    /// adapter artifact (metadata) or a [`FineTuneError`].
    fn fine_tune(
        &self,
        config: &FineTuneConfig,
        pairs: &[TrainingPair],
    ) -> Result<AdapterArtifact, FineTuneError>;

    /// Convenience: run a [`FineTuneJob`] (id + config) over `pairs`.
    ///
    /// The default forwards to [`FineTuner::fine_tune`]; the `job.job_id` is
    /// recorded by implementations in the artifact's provenance.
    fn run_job(
        &self,
        job: &FineTuneJob,
        pairs: &[TrainingPair],
    ) -> Result<AdapterArtifact, FineTuneError> {
        self.fine_tune(&job.config, pairs)
    }
}

/// A deterministic, CI-hermetic [`FineTuner`] that performs **no real training**.
///
/// It validates the config, then derives a reproducible [`AdapterArtifact`] from
/// a stable FNV-1a hash of `(config, pairs)`. No GPU, no I/O, no randomness — so
/// the same inputs always yield byte-identical output and the whole pipeline is
/// testable. The `created_at_ns` is fixed (caller-supplied, default `0`) for the
/// same reason.
#[derive(Debug, Clone, Default)]
pub struct FixtureFineTuner {
    created_at_ns: u64,
}

impl FixtureFineTuner {
    /// A fixture trainer stamping artifacts with `created_at_ns = 0`.
    pub fn new() -> Self {
        Self::default()
    }

    /// A fixture trainer that stamps a fixed creation timestamp into provenance,
    /// while remaining fully deterministic.
    pub fn with_created_at(created_at_ns: u64) -> Self {
        FixtureFineTuner { created_at_ns }
    }

    /// The deterministic content fingerprint of a run, exposed for tests and so
    /// callers can predict an adapter id without running the trainer.
    pub fn fingerprint(config: &FineTuneConfig, pairs: &[TrainingPair]) -> u64 {
        let mut h = Fnv1a::new();
        h.write_str("anima-finetune.fixture.v1");
        config.hash_into(&mut h);
        h.write_u64(pairs.len() as u64);
        for p in pairs {
            p.hash_into(&mut h);
        }
        h.finish()
    }
}

impl FineTuner for FixtureFineTuner {
    fn id(&self) -> &str {
        "fixture"
    }

    fn capabilities(&self) -> FineTunerCapabilities {
        FineTunerCapabilities {
            real_training: false,
            // The fixture accepts every method abstractly (it never trains).
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
            cpu_only: true,
        }
    }

    fn fine_tune(
        &self,
        config: &FineTuneConfig,
        pairs: &[TrainingPair],
    ) -> Result<AdapterArtifact, FineTuneError> {
        config.validate()?;
        if pairs.is_empty() {
            return Err(FineTuneError::EmptyDataset);
        }

        let fp = Self::fingerprint(config, pairs);
        let adapter_id = format!("{}-{}", config.output_adapter_id, hex64(fp));
        // A second, salted digest stands in for the (externally produced) weights.
        let weights_digest = hex64(fp ^ 0x5bd1_e995_5bd1_e995);

        Ok(AdapterArtifact {
            adapter_id,
            description: config.description.clone(),
            format: AdapterFormat::for_method(&config.method),
            merge_path: config.method.merge_path(),
            serving_tier: config.method.serving_tier(),
            weights_digest,
            // The fixture tuner produces no on-disk artifacts.
            adapter_path: None,
            merged_gguf_path: None,
            provenance: Provenance {
                base_model: config.base_model.clone(),
                method: config.method.clone(),
                // No explicit job id here; run_job records the real one. Use the
                // configured output id as the traceable source for direct calls.
                source_job: config.output_adapter_id.clone(),
                created_at_ns: self.created_at_ns,
            },
        })
    }

    fn run_job(
        &self,
        job: &FineTuneJob,
        pairs: &[TrainingPair],
    ) -> Result<AdapterArtifact, FineTuneError> {
        let mut artifact = self.fine_tune(&job.config, pairs)?;
        artifact.provenance.source_job = job.job_id.clone();
        Ok(artifact)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::method::HraKind;

    fn pairs() -> Vec<TrainingPair> {
        vec![TrainingPair::new("q1", "a1"), TrainingPair::new("q2", "a2")]
    }

    fn config() -> FineTuneConfig {
        FineTuneConfig::new("base-q4", "episodic://2026-06", "maths")
            .with_description("grade-school maths")
    }

    #[test]
    fn fixture_reports_non_real_cpu_only() {
        let caps = FixtureFineTuner::new().capabilities();
        assert!(!caps.real_training);
        assert!(caps.cpu_only);
        assert!(caps.supported_methods.contains(&"qlora".to_string()));
    }

    #[test]
    fn fixture_is_deterministic() {
        let t = FixtureFineTuner::new();
        let a = t.fine_tune(&config(), &pairs()).unwrap();
        let b = t.fine_tune(&config(), &pairs()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn different_inputs_yield_different_ids() {
        let t = FixtureFineTuner::new();
        let a = t.fine_tune(&config(), &pairs()).unwrap();

        let mut other = pairs();
        other.push(TrainingPair::new("q3", "a3"));
        let b = t.fine_tune(&config(), &other).unwrap();

        assert_ne!(a.adapter_id, b.adapter_id);
        assert_ne!(a.weights_digest, b.weights_digest);
    }

    #[test]
    fn method_choice_drives_format_and_tier() {
        let t = FixtureFineTuner::new();
        let hira = config().with_method(AdaptationMethod::Hra {
            family: HraKind::Hira,
            rank: 128,
        });
        let art = t.fine_tune(&hira, &pairs()).unwrap();
        assert_eq!(art.format, AdapterFormat::BakedGguf);
        assert!(!art.is_mountable());
    }

    #[test]
    fn empty_dataset_is_rejected() {
        let t = FixtureFineTuner::new();
        assert_eq!(
            t.fine_tune(&config(), &[]),
            Err(FineTuneError::EmptyDataset)
        );
    }

    #[test]
    fn zero_rank_config_is_rejected() {
        let t = FixtureFineTuner::new();
        let bad = config().with_method(AdaptationMethod::Lora { rank: 0, alpha: 8 });
        match t.fine_tune(&bad, &pairs()) {
            Err(FineTuneError::InvalidConfig { .. }) => {}
            other => panic!("expected InvalidConfig, got {other:?}"),
        }
    }

    #[test]
    fn run_job_records_job_id_in_provenance() {
        let t = FixtureFineTuner::new();
        let job = FineTuneJob::new("job-42", config());
        let art = t.run_job(&job, &pairs()).unwrap();
        assert_eq!(art.provenance.source_job, "job-42");
    }

    #[test]
    fn fingerprint_helper_matches_artifact_id_suffix() {
        let fp = FixtureFineTuner::fingerprint(&config(), &pairs());
        let art = FixtureFineTuner::new()
            .fine_tune(&config(), &pairs())
            .unwrap();
        assert!(art.adapter_id.ends_with(&hex64(fp)));
    }

    #[test]
    fn config_serde_round_trip() {
        let c = config().with_method(AdaptationMethod::Hra {
            family: HraKind::HyperAdapt,
            rank: 64,
        });
        let json = serde_json::to_string(&c).unwrap();
        let back: FineTuneConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }
}
