//! Real (non-fixture) fine-tuning backends.
//!
//! AnimaOS's first-party trainer is **Unsloth** (`docs/13-local-llm-providers.md`
//! §S8.4). Real training runs Python/torch/CUDA and **cannot run in CI**, so the
//! real path lives here behind the `live` feature flag and is documented as an
//! external dependency. The deterministic [`crate::tuner::FixtureFineTuner`] is
//! the only trainer that actually produces artifacts in a hermetic build.
//!
//! The [`unsloth`] module is always present so the type and its capabilities are
//! visible, but the [`unsloth::UnslothFineTuner`] is a deliberate *skeleton*:
//!
//! - Built **without** `--features live`, every call returns
//!   [`crate::error::FineTuneError::BackendUnavailable`] explaining the feature
//!   is off.
//! - Built **with** `--features live`, it still returns `BackendUnavailable`
//!   unless the external Unsloth/PEFT runtime and its environment are actually
//!   present — which is the real, out-of-process training path.
//!
//! The real HRA training and merge/quantisation pipeline (S8.4.5/6) execute
//! inside that external process; they are not reimplemented in Rust here.

pub mod unsloth;
