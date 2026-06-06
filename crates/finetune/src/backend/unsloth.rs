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

use crate::artifact::AdapterArtifact;
use crate::dataset::TrainingPair;
use crate::error::FineTuneError;
use crate::method::AdaptationMethod;
use crate::tuner::{FineTuneConfig, FineTuner, FineTunerCapabilities};

/// Environment variable that must point at an installed Unsloth/PEFT runtime for
/// the `live` backend to attempt a real training run.
pub const UNSLOTH_HOME_ENV: &str = "ANIMA_UNSLOTH_HOME";

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

#[cfg(feature = "live")]
fn run_external_training(
    _config: &FineTuneConfig,
    _pairs: &[TrainingPair],
) -> Result<AdapterArtifact, FineTuneError> {
    // TODO(live): spawn the external Unsloth/PEFT job here — serialise the
    // training set, invoke the Python entrypoint under `ANIMA_UNSLOTH_HOME`, run
    // the S8.4.5 adaptation + S8.4.6 merge/quant pipeline, then read back the
    // produced adapter + merged GGUF and assemble an `AdapterArtifact` with real
    // provenance. Until that integration ships, report the backend as
    // unavailable so callers transparently fall back to the fixture path.
    Err(FineTuneError::BackendUnavailable {
        backend: "unsloth".to_string(),
        reason: "external Unsloth training integration is not wired in this build".to_string(),
    })
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
}
