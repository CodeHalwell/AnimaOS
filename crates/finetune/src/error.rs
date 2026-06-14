//! Error types for the adaptation engine.
//!
//! [`FineTuneError`] is the single error returned across the [`crate::tuner`]
//! pipeline. The variant most relevant to the fixture-vs-real boundary is
//! [`FineTuneError::BackendUnavailable`]: the feature-gated real backend
//! ([`crate::backend::unsloth`]) returns it whenever the external Unsloth/PEFT
//! runtime or its environment is absent, which — by design — is *always* in CI.

use std::fmt;

/// Errors produced while configuring or running a [`crate::tuner::FineTuner`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FineTuneError {
    /// The real (GPU) training backend is not available.
    ///
    /// Returned by [`crate::backend::unsloth::UnslothFineTuner`] when the crate
    /// is built without the `live` feature, or when it is built with `live` but
    /// the external Unsloth/PEFT runtime / required environment is missing.
    /// The `reason` explains which precondition failed.
    BackendUnavailable {
        /// Identifier of the backend that could not run (e.g. `"unsloth"`).
        backend: String,
        /// Human-readable explanation of the missing precondition.
        reason: String,
    },
    /// The training set was empty, so no adapter could be produced.
    EmptyDataset,
    /// A configuration value was invalid (e.g. zero LoRA rank).
    InvalidConfig {
        /// What was wrong with the configuration.
        message: String,
    },
    /// The chosen [`crate::method::AdaptationMethod`] is not supported by this
    /// trainer (see [`crate::tuner::FineTunerCapabilities`]).
    UnsupportedMethod {
        /// Name of the rejected method.
        method: String,
    },
    /// The external training process (the out-of-process Unsloth/PEFT
    /// entrypoint, S8.4.5/6) failed: it could not be spawned, exited non-zero,
    /// or produced output the Rust side could not interpret.
    ///
    /// Returned by [`crate::backend::unsloth::UnslothFineTuner`] in `live`
    /// builds when the runtime *is* present but the run did not complete
    /// successfully. The `message` carries the captured cause (exit status +
    /// stderr excerpt, a spawn error, or a result-parse failure).
    ExternalBackend {
        /// Identifier of the backend whose external process failed.
        backend: String,
        /// Human-readable explanation (spawn/exit/parse failure detail).
        message: String,
    },
}

impl fmt::Display for FineTuneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FineTuneError::BackendUnavailable { backend, reason } => {
                write!(f, "fine-tune backend `{backend}` unavailable: {reason}")
            }
            FineTuneError::EmptyDataset => {
                write!(f, "cannot fine-tune on an empty training set")
            }
            FineTuneError::InvalidConfig { message } => {
                write!(f, "invalid fine-tune config: {message}")
            }
            FineTuneError::UnsupportedMethod { method } => {
                write!(
                    f,
                    "adaptation method `{method}` not supported by this trainer"
                )
            }
            FineTuneError::ExternalBackend { backend, message } => {
                write!(
                    f,
                    "external training backend `{backend}` failed: {message}"
                )
            }
        }
    }
}

impl std::error::Error for FineTuneError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_mentions_backend_and_reason() {
        let e = FineTuneError::BackendUnavailable {
            backend: "unsloth".to_string(),
            reason: "feature `live` not enabled".to_string(),
        };
        let s = e.to_string();
        assert!(s.contains("unsloth"));
        assert!(s.contains("live"));
    }

    #[test]
    fn errors_are_comparable() {
        assert_eq!(FineTuneError::EmptyDataset, FineTuneError::EmptyDataset);
        assert_ne!(
            FineTuneError::EmptyDataset,
            FineTuneError::UnsupportedMethod {
                method: "hra".to_string()
            }
        );
    }
}
