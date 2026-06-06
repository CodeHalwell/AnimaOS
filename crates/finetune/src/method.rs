//! S8.4.4 — The [`AdaptationMethod`] abstraction.
//!
//! AnimaOS keeps **QLoRA** as the conservative default (`docs/13-local-llm-providers.md`
//! S8.4.4) and makes **High-Rank Adaptation (HRA)** selectable — and recommended —
//! for the instinct tier (S8.4.5). Each method also implies a *merge path*
//! ([`MergePath`]) and a *serving tier* ([`ServingTier`]), because the serving
//! distinction "falls straight out of the merge maths" (S8.4.8): only
//! structurally-clean / LoRA-format adapters are cheaply hot-mountable; a dense
//! Hadamard `ΔW` must be baked into a model variant.
//!
//! This module is pure metadata: it describes *what* method an adapter used and
//! *how* it would be served. The actual training math lives in the external
//! backend (S8.4.5/6) and is out of scope for this crate.

use crate::hash::Fnv1a;
use serde::{Deserialize, Serialize};

/// The specific High-Rank Adaptation family used for an [`AdaptationMethod::Hra`].
///
/// Names follow the report's method table (S8.4 §"Adaptation methods").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HraKind {
    /// HyperAdapt — structural scaling `W = S₁·W₀·S₂`. Clean, cheap broadcast
    /// merge; the **default for the quantised serving path**.
    HyperAdapt,
    /// OHoRA — orthogonal/QR projection (~0.04% params); merges cleanly.
    Ohora,
    /// HiRA — Hadamard `ΔW = (B₁A₁)⊙(B₂A₂)`; dense materialise → dequant → merge
    /// → requant. Highest raw expressiveness; baked into a variant.
    Hira,
    /// BoHA — blockwise Hadamard variant; locality curbs catastrophic forgetting,
    /// useful for the continual self-improvement loop.
    Boha,
    /// HRP — high-rank preheat → SVD → low-rank LoRA. Ends as a LoRA, so it
    /// merges/mounts like vanilla LoRA; a cheap robustness upgrade.
    Hrp,
}

impl HraKind {
    /// Whether this HRA family produces a dense Hadamard `ΔW` that must be baked
    /// into a model variant rather than hot-mounted as an adapter.
    pub fn is_hadamard(self) -> bool {
        matches!(self, HraKind::Hira | HraKind::Boha)
    }

    /// Stable lowercase identifier used in hashing and provenance.
    pub fn as_str(self) -> &'static str {
        match self {
            HraKind::HyperAdapt => "hyperadapt",
            HraKind::Ohora => "ohora",
            HraKind::Hira => "hira",
            HraKind::Boha => "boha",
            HraKind::Hrp => "hrp",
        }
    }
}

/// The fine-tuning / adaptation method applied to a base model (S8.4.4).
///
/// Serde-serializable with an internal `kind` tag so configs and adapter
/// provenance read cleanly as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AdaptationMethod {
    /// Low-Rank Adaptation: `ΔW = BA`, rank `r`, scaling `alpha`.
    Lora {
        /// LoRA rank `r` (the low-rank bottleneck).
        rank: u32,
        /// LoRA scaling factor `alpha`.
        alpha: u32,
    },
    /// Quantised LoRA — LoRA over a 4-bit (NF4) base. The conservative default.
    QLora {
        /// LoRA rank `r`.
        rank: u32,
        /// LoRA scaling factor `alpha`.
        alpha: u32,
        /// Base-model quantisation bit width (typically `4`).
        base_bits: u8,
    },
    /// High-Rank Adaptation — lifts the rank ceiling at a comparable parameter
    /// footprint (S8.4.5). The [`HraKind`] selects the concrete family.
    Hra {
        /// Which HRA family is used.
        family: HraKind,
        /// Effective rank target (may exceed a LoRA bottleneck).
        rank: u32,
    },
    /// Full fine-tune — updates all weights (no adapter; produces a variant).
    FullFineTune,
}

impl Default for AdaptationMethod {
    /// QLoRA is the conservative default (S8.4.4) with rank 16, alpha 32, 4-bit.
    fn default() -> Self {
        AdaptationMethod::QLora {
            rank: 16,
            alpha: 32,
            base_bits: 4,
        }
    }
}

impl AdaptationMethod {
    /// A short stable label, e.g. `"qlora"`, `"hra:hyperadapt"`, `"full"`.
    pub fn label(&self) -> String {
        match self {
            AdaptationMethod::Lora { .. } => "lora".to_string(),
            AdaptationMethod::QLora { .. } => "qlora".to_string(),
            AdaptationMethod::Hra { family, .. } => format!("hra:{}", family.as_str()),
            AdaptationMethod::FullFineTune => "full".to_string(),
        }
    }

    /// How this method's update merges into a 4-bit GGUF base (S8.4.6).
    pub fn merge_path(&self) -> MergePath {
        match self {
            // LoRA/QLoRA and structurally-clean HRA broadcast cleanly.
            AdaptationMethod::Lora { .. } | AdaptationMethod::QLora { .. } => MergePath::Clean,
            AdaptationMethod::Hra { family, .. } => {
                if family.is_hadamard() {
                    MergePath::Hadamard
                } else {
                    MergePath::Clean
                }
            }
            // A full fine-tune simply *is* the merged model.
            AdaptationMethod::FullFineTune => MergePath::Clean,
        }
    }

    /// Which library tier this method's output lands in (S8.4.8).
    ///
    /// LoRA-format / structurally-clean adapters are hot-mountable; dense
    /// Hadamard updates and full merges are baked variants.
    pub fn serving_tier(&self) -> ServingTier {
        match self {
            AdaptationMethod::Lora { .. } | AdaptationMethod::QLora { .. } => {
                ServingTier::MountableAdapter
            }
            AdaptationMethod::Hra { family, .. } => {
                // HRP ends as a LoRA (mountable); HyperAdapt is a clean transform
                // (mountable where the runtime allows a custom apply); OHoRA and
                // the Hadamard families bake to a variant.
                match family {
                    HraKind::Hrp | HraKind::HyperAdapt => ServingTier::MountableAdapter,
                    HraKind::Ohora | HraKind::Hira | HraKind::Boha => ServingTier::BakedVariant,
                }
            }
            AdaptationMethod::FullFineTune => ServingTier::BakedVariant,
        }
    }

    /// Absorb the method into a deterministic fingerprint so fixture artifacts
    /// are reproducible (see [`crate::tuner::FixtureFineTuner`]).
    pub(crate) fn hash_into(&self, h: &mut Fnv1a) {
        h.write_str(&self.label());
        match self {
            AdaptationMethod::Lora { rank, alpha }
            | AdaptationMethod::QLora { rank, alpha, .. } => {
                h.write_u64(*rank as u64);
                h.write_u64(*alpha as u64);
            }
            AdaptationMethod::Hra { family, rank } => {
                h.write_str(family.as_str());
                h.write_u64(*rank as u64);
            }
            AdaptationMethod::FullFineTune => {}
        }
        if let AdaptationMethod::QLora { base_bits, .. } = self {
            h.write_u64(*base_bits as u64);
        }
    }
}

/// The export/merge path implied by a method (S8.4.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergePath {
    /// Direct merge → GGUF export (HyperAdapt/OHoRA/HRP/LoRA/full).
    Clean,
    /// Materialise dense `ΔW` → dequantise base → merge → requantise (HiRA/BoHA).
    Hadamard,
}

/// Which serving tier of the adapter library an output belongs to (S8.4.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServingTier {
    /// Hot-loaded onto a live base (vLLM/llama.cpp); swappable per task/request.
    MountableAdapter,
    /// A distinct GGUF model; swappable per route/model.
    BakedVariant,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_qlora() {
        match AdaptationMethod::default() {
            AdaptationMethod::QLora {
                rank, base_bits, ..
            } => {
                assert_eq!(rank, 16);
                assert_eq!(base_bits, 4);
            }
            other => panic!("expected QLora default, got {other:?}"),
        }
    }

    #[test]
    fn labels_are_stable() {
        assert_eq!(
            AdaptationMethod::Lora { rank: 8, alpha: 16 }.label(),
            "lora"
        );
        assert_eq!(AdaptationMethod::default().label(), "qlora");
        assert_eq!(
            AdaptationMethod::Hra {
                family: HraKind::HyperAdapt,
                rank: 64
            }
            .label(),
            "hra:hyperadapt"
        );
        assert_eq!(AdaptationMethod::FullFineTune.label(), "full");
    }

    #[test]
    fn hadamard_methods_bake_to_variant() {
        for fam in [HraKind::Hira, HraKind::Boha] {
            let m = AdaptationMethod::Hra {
                family: fam,
                rank: 128,
            };
            assert_eq!(m.merge_path(), MergePath::Hadamard);
            assert_eq!(m.serving_tier(), ServingTier::BakedVariant);
        }
    }

    #[test]
    fn clean_hra_families_are_mountable_or_clean() {
        // HRP and HyperAdapt are mountable; OHoRA is clean-merge but baked.
        let hrp = AdaptationMethod::Hra {
            family: HraKind::Hrp,
            rank: 32,
        };
        assert_eq!(hrp.merge_path(), MergePath::Clean);
        assert_eq!(hrp.serving_tier(), ServingTier::MountableAdapter);

        let ohora = AdaptationMethod::Hra {
            family: HraKind::Ohora,
            rank: 32,
        };
        assert_eq!(ohora.merge_path(), MergePath::Clean);
        assert_eq!(ohora.serving_tier(), ServingTier::BakedVariant);
    }

    #[test]
    fn lora_is_mountable_and_clean() {
        let m = AdaptationMethod::Lora { rank: 8, alpha: 16 };
        assert_eq!(m.merge_path(), MergePath::Clean);
        assert_eq!(m.serving_tier(), ServingTier::MountableAdapter);
    }

    #[test]
    fn serde_round_trip_all_variants() {
        let methods = vec![
            AdaptationMethod::Lora { rank: 8, alpha: 16 },
            AdaptationMethod::default(),
            AdaptationMethod::Hra {
                family: HraKind::Boha,
                rank: 128,
            },
            AdaptationMethod::FullFineTune,
        ];
        for m in methods {
            let json = serde_json::to_string(&m).expect("serialise");
            let back: AdaptationMethod = serde_json::from_str(&json).expect("deserialise");
            assert_eq!(m, back, "round-trip mismatch for {json}");
        }
    }

    #[test]
    fn serde_tag_is_readable() {
        let json = serde_json::to_string(&AdaptationMethod::Hra {
            family: HraKind::HyperAdapt,
            rank: 64,
        })
        .unwrap();
        assert!(json.contains("\"kind\":\"hra\""), "got {json}");
        assert!(json.contains("\"family\":\"hyper_adapt\""), "got {json}");
    }
}
