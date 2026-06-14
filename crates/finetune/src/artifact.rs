//! The output of a fine-tune: an [`AdapterArtifact`] plus its [`Provenance`].
//!
//! An artifact is the unit the [`crate::library::AdapterLibrary`] stores and the
//! runtime mounts (S8.4.8). It is *metadata about* a trained adapter — id,
//! format, base model, the [`crate::method::AdaptationMethod`] used, and the
//! provenance an operator needs to trust it — not the adapter weights
//! themselves (those are produced externally by the real backend, S8.4.5/6).
//!
//! In the fixture layer the `adapter_id` and `weights_digest` are derived
//! deterministically from the config + training data (see
//! [`crate::tuner::FixtureFineTuner`]), so every test sees a stable, reproducible
//! artifact with no GPU and no I/O.

use crate::method::{AdaptationMethod, MergePath, ServingTier};
use serde::{Deserialize, Serialize};

/// The on-disk / on-wire format an adapter is served as (mirrors S8.4.8 tiers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterFormat {
    /// A LoRA-format adapter, hot-mountable onto a live base.
    LoraAdapter,
    /// A clean structural transform (e.g. HyperAdapt scaling) applied at mount
    /// time where the runtime allows it.
    StructuralTransform,
    /// A baked, distinct GGUF model variant (dense merges, full fine-tunes).
    BakedGguf,
}

impl AdapterFormat {
    /// The format produced by a given method, following the S8.4.8 two-tier map.
    pub fn for_method(method: &AdaptationMethod) -> Self {
        use crate::method::HraKind;
        match method {
            AdaptationMethod::Lora { .. } | AdaptationMethod::QLora { .. } => {
                AdapterFormat::LoraAdapter
            }
            AdaptationMethod::Hra { family, .. } => match family {
                HraKind::Hrp => AdapterFormat::LoraAdapter,
                HraKind::HyperAdapt => AdapterFormat::StructuralTransform,
                HraKind::Ohora | HraKind::Hira | HraKind::Boha => AdapterFormat::BakedGguf,
            },
            AdaptationMethod::FullFineTune => AdapterFormat::BakedGguf,
        }
    }
}

/// Trust/lineage metadata for an adapter (E11 S11.6 provenance + S8.4.8 entry).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// The base model + quant the adapter was trained against (e.g.
    /// `"qwen2.5-1.5b-instruct-q4"`).
    pub base_model: String,
    /// The adaptation method (and its params) used to produce the adapter.
    pub method: AdaptationMethod,
    /// Identifier of the [`crate::tuner::FineTuneJob`] that produced this
    /// artifact, for tracing back to the run.
    pub source_job: String,
    /// Wall-clock creation time (nanoseconds since the Unix epoch).
    ///
    /// In the fixture layer callers pass a fixed value so artifacts stay
    /// byte-identical across runs.
    pub created_at_ns: u64,
}

/// A trained adapter and everything needed to register, evaluate, and mount it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterArtifact {
    /// Stable, unique adapter identifier. In the fixture layer this is a
    /// content-derived hash so it is reproducible.
    pub adapter_id: String,
    /// Human-readable domain/description used for task→adapter selection
    /// (S8.4.8 reuses the E7 length-robust filter on this text).
    pub description: String,
    /// The serving format (mountable vs baked).
    pub format: AdapterFormat,
    /// The merge path the method implies (clean vs Hadamard).
    pub merge_path: MergePath,
    /// Which library tier this artifact belongs to.
    pub serving_tier: ServingTier,
    /// A stable digest standing in for the (externally-produced) adapter weights.
    pub weights_digest: String,
    /// Filesystem path to the produced adapter artifacts, when the backend writes
    /// them to disk (e.g. the external Unsloth trainer). `None` for in-memory or
    /// fixture artifacts that produce no on-disk files.
    pub adapter_path: Option<String>,
    /// Filesystem path to the merged GGUF for baked variants, when one was
    /// produced. `None` for mountable adapters or fixture artifacts.
    pub merged_gguf_path: Option<String>,
    /// Lineage / trust metadata.
    pub provenance: Provenance,
}

impl AdapterArtifact {
    /// Whether this artifact can be hot-mounted onto a live base, or must be
    /// served as a baked variant.
    pub fn is_mountable(&self) -> bool {
        matches!(self.serving_tier, ServingTier::MountableAdapter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::method::HraKind;

    #[test]
    fn format_follows_method_tier() {
        assert_eq!(
            AdapterFormat::for_method(&AdaptationMethod::default()),
            AdapterFormat::LoraAdapter
        );
        assert_eq!(
            AdapterFormat::for_method(&AdaptationMethod::Hra {
                family: HraKind::HyperAdapt,
                rank: 64
            }),
            AdapterFormat::StructuralTransform
        );
        assert_eq!(
            AdapterFormat::for_method(&AdaptationMethod::Hra {
                family: HraKind::Hira,
                rank: 128
            }),
            AdapterFormat::BakedGguf
        );
        assert_eq!(
            AdapterFormat::for_method(&AdaptationMethod::FullFineTune),
            AdapterFormat::BakedGguf
        );
    }

    fn artifact(tier: ServingTier) -> AdapterArtifact {
        AdapterArtifact {
            adapter_id: "a1".to_string(),
            description: "math tutor".to_string(),
            format: AdapterFormat::LoraAdapter,
            merge_path: MergePath::Clean,
            serving_tier: tier,
            weights_digest: "deadbeef".to_string(),
            adapter_path: None,
            merged_gguf_path: None,
            provenance: Provenance {
                base_model: "base-q4".to_string(),
                method: AdaptationMethod::default(),
                source_job: "job-1".to_string(),
                created_at_ns: 0,
            },
        }
    }

    #[test]
    fn mountable_flag_tracks_tier() {
        assert!(artifact(ServingTier::MountableAdapter).is_mountable());
        assert!(!artifact(ServingTier::BakedVariant).is_mountable());
    }

    #[test]
    fn artifact_serde_round_trip() {
        let a = artifact(ServingTier::MountableAdapter);
        let json = serde_json::to_string(&a).unwrap();
        let back: AdapterArtifact = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }
}
