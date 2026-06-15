//! E8 S8.4 — Unsloth Adaptation Engine (abstraction + fixture layer)
//!
//! AnimaOS treats Unsloth as the **default, first-party fine-tuning engine**
//! (see `docs/13-local-llm-providers.md` §S8.4). Fine-tuning is *training, not
//! serving*: an offline capability that produces adapters and merged models
//! which the inference backends (E8 §S8.1–.3) later serve.
//!
//! Real adaptation runs on GPUs via Unsloth / HF PEFT (Python, torch, CUDA) and
//! **cannot run in CI**. Following this repo's established convention for the
//! inference backends, this crate ships the *abstractions* plus a deterministic,
//! CI-hermetic **fixture** implementation. The real backend is a clearly-marked,
//! feature-gated path ([`backend::unsloth`], `--features live`).
//!
//! ## Stories delivered (abstraction + fixture layer)
//!
//! | Story | Module | Description |
//! |-------|--------|-------------|
//! | S8.4.2 | [`tuner`]   | `FineTuner` trait (thin provider abstraction) + [`tuner::FixtureFineTuner`] |
//! | S8.4.3 | [`dataset`] | `TrainingPair` — the consolidation/"dreaming" output format + builder |
//! | S8.4.4 | [`method`]  | `AdaptationMethod` abstraction (LoRA / QLoRA / HRA / full) |
//! | S8.4.7 | [`eval`]    | Adaptation eval harness (the LoRA-vs-HRA decider), fixture scoring |
//! | S8.4.8 | [`library`] | `AdapterLibrary` registry + provenance + dynamic mounting |
//!
//! ## What stays external (feature-gated, not implemented here)
//!
//! - **S8.4.5 / S8.4.6** — real HRA training (HyperAdapt/HiRA/BoHA/HRP) and the
//!   merge & quantisation pipeline run inside the Unsloth/PEFT process. The
//!   [`backend::unsloth::UnslothFineTuner`] skeleton always compiles, but returns
//!   [`FineTuneError::BackendUnavailable`] unless the crate is built with
//!   `--features live` *and* the external Unsloth/PEFT runtime and environment
//!   are present.
//! - **S8.4.3 dreaming loop** — wiring episodic memory into the dataset source is
//!   a gated research spike (catastrophic-forgetting / value-drift risk). This
//!   crate only fixes the *format* ([`TrainingPair`]) that loop would emit.
//!
//! ## The fixture-vs-real boundary
//!
//! Everything in this crate is deterministic and side-effect free by default.
//! [`tuner::FixtureFineTuner`] derives a reproducible [`AdapterArtifact`] from a
//! stable hash of its inputs (config + training pairs) — no GPU, no I/O, no
//! randomness — so the *pipeline shape* (dataset → train → register → eval →
//! mount) is fully testable. Swapping in the real trainer changes only which
//! [`FineTuner`] is constructed; every downstream type is identical.

#![forbid(unsafe_code)]

pub mod adoption;
pub mod artifact;
pub mod backend;
pub mod dataset;
pub mod error;
pub mod eval;
pub mod hash;
pub mod library;
pub mod method;
pub mod tuner;

pub use adoption::{decide_adoption, AdoptionDecision, AdoptionPolicy, AlignmentOutcome};
pub use artifact::{AdapterArtifact, AdapterFormat, Provenance};
pub use dataset::{TrainingPair, TrainingSet};
pub use error::FineTuneError;
pub use eval::{evaluate_adapter, EvalCase, EvalReport, MetricScores};
pub use library::{AdapterLibrary, EvictionPolicy, MountError, MountId, MountPoint};
pub use method::{AdaptationMethod, MergePath, ServingTier};
pub use tuner::{FineTuneConfig, FineTuneJob, FineTuner, FineTunerCapabilities, FixtureFineTuner};
