//! S8.4.8 — The adapter library + dynamic mounting.
//!
//! A small base model plus a *shelf of cheap specialists* covers many domains
//! (S8.4.8). [`AdapterLibrary`] is the in-memory registry of those specialists:
//! it stores [`AdapterArtifact`]s with their provenance, supports
//! register/get/list, and tracks **dynamic mounting** — which adapter is mounted
//! onto which `(tier, model)` mount point.
//!
//! The real library lives at `~/.anima/<agent_id>/adapters/` and is served by
//! vLLM/llama.cpp; this in-memory form is the abstraction the rest of AnimaOS
//! programs against and tests deterministically.
//!
//! ## Capacity & eviction
//!
//! The registry is capacity-bounded. When full, [`AdapterLibrary::register`]
//! evicts according to [`EvictionPolicy`] (default: oldest by `created_at_ns`,
//! i.e. least-recently-trained). A **currently-mounted** adapter is never
//! evicted — mounting pins an entry. Eviction here is a registry-hygiene
//! concern (S8.4.8 "library hygiene"); on-disk eviction in the real library
//! would additionally delete the adapter files.

use crate::adoption::AdoptionDecision;
use crate::artifact::AdapterArtifact;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;

/// Identifies a place an adapter can be mounted: a serving `tier` (e.g.
/// `"cheap-local"`) bound to a base `model` (S8.4.8 / §4 route mapping).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MountId {
    /// The tier this mount point serves (e.g. `"instinct"`, `"cheap-local"`).
    pub tier: String,
    /// The base model the adapter is mounted onto.
    pub model: String,
}

impl MountId {
    /// Construct a mount id from a tier and model.
    pub fn new(tier: impl Into<String>, model: impl Into<String>) -> Self {
        MountId {
            tier: tier.into(),
            model: model.into(),
        }
    }
}

/// A live mounting of an adapter onto a [`MountId`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountPoint {
    /// Where the adapter is mounted.
    pub mount_id: MountId,
    /// The id of the mounted adapter.
    pub adapter_id: String,
}

/// How the library evicts entries when at capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvictionPolicy {
    /// Evict the entry with the smallest `created_at_ns` (oldest training run).
    #[default]
    OldestFirst,
    /// Reject new registrations once full (no eviction).
    RejectWhenFull,
}

/// Errors from mounting / library operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountError {
    /// No adapter with the given id is registered.
    UnknownAdapter {
        /// The id that was requested.
        adapter_id: String,
    },
    /// The adapter is a baked variant and cannot be hot-mounted (S8.4.8): it must
    /// be served as a distinct model instead.
    NotMountable {
        /// The id of the baked-variant adapter.
        adapter_id: String,
    },
    /// The library is full and the [`EvictionPolicy`] forbids eviction (or every
    /// entry is pinned by an active mount).
    CapacityExceeded {
        /// The configured capacity.
        capacity: usize,
    },
    /// Nothing is mounted at the given mount point.
    NotMounted {
        /// The mount point that was empty.
        mount_id: MountId,
    },
    /// The adapter is registered and mountable in principle, but has not cleared
    /// the adoption gate (eval harness + alignment), so [`AdapterLibrary::mount_gated`]
    /// refuses to put it on a live serving tier.
    NotAdopted {
        /// The id of the un-adopted adapter.
        adapter_id: String,
    },
}

impl fmt::Display for MountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MountError::UnknownAdapter { adapter_id } => {
                write!(f, "no adapter registered with id `{adapter_id}`")
            }
            MountError::NotMountable { adapter_id } => write!(
                f,
                "adapter `{adapter_id}` is a baked variant and cannot be hot-mounted"
            ),
            MountError::CapacityExceeded { capacity } => {
                write!(
                    f,
                    "adapter library at capacity ({capacity}); cannot register"
                )
            }
            MountError::NotMounted { mount_id } => write!(
                f,
                "nothing mounted at tier `{}` / model `{}`",
                mount_id.tier, mount_id.model
            ),
            MountError::NotAdopted { adapter_id } => write!(
                f,
                "adapter `{adapter_id}` has not passed the adoption gate (eval + alignment)"
            ),
        }
    }
}

impl std::error::Error for MountError {}

/// An in-memory registry of [`AdapterArtifact`]s with dynamic-mount tracking.
#[derive(Debug, Clone)]
pub struct AdapterLibrary {
    capacity: usize,
    policy: EvictionPolicy,
    adapters: HashMap<String, AdapterArtifact>,
    mounts: HashMap<MountId, String>,
    /// Adapter ids that have cleared the adoption gate (eval + alignment) and
    /// may therefore be mounted via [`AdapterLibrary::mount_gated`]. An adapter
    /// loses adoption when it is replaced (new weights) or evicted.
    adopted: HashSet<String>,
}

impl AdapterLibrary {
    /// Create a library bounded to `capacity` entries using the default
    /// (oldest-first) eviction policy.
    pub fn new(capacity: usize) -> Self {
        Self::with_policy(capacity, EvictionPolicy::default())
    }

    /// Create a library with an explicit capacity and eviction policy.
    pub fn with_policy(capacity: usize, policy: EvictionPolicy) -> Self {
        AdapterLibrary {
            capacity: capacity.max(1),
            policy,
            adapters: HashMap::new(),
            mounts: HashMap::new(),
            adopted: HashSet::new(),
        }
    }

    /// Configured maximum number of stored adapters.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Number of adapters currently registered.
    pub fn len(&self) -> usize {
        self.adapters.len()
    }

    /// Whether the library is empty.
    pub fn is_empty(&self) -> bool {
        self.adapters.is_empty()
    }

    /// Register an artifact. Re-registering the same `adapter_id` replaces it in
    /// place (no eviction). Otherwise, if full, eviction follows the configured
    /// [`EvictionPolicy`]; mounted adapters are pinned and never evicted.
    pub fn register(&mut self, artifact: AdapterArtifact) -> Result<(), MountError> {
        if self.adapters.contains_key(&artifact.adapter_id) {
            // Re-registering replaces the weights, so any prior adoption is
            // stale — the new artifact must clear the gate again before mounting.
            self.adopted.remove(&artifact.adapter_id);
            self.adapters.insert(artifact.adapter_id.clone(), artifact);
            return Ok(());
        }
        if self.adapters.len() >= self.capacity {
            self.evict_one()?;
        }
        self.adapters.insert(artifact.adapter_id.clone(), artifact);
        Ok(())
    }

    /// Evict a single entry per policy. Never evicts a mounted (pinned) adapter.
    fn evict_one(&mut self) -> Result<(), MountError> {
        if self.policy == EvictionPolicy::RejectWhenFull {
            return Err(MountError::CapacityExceeded {
                capacity: self.capacity,
            });
        }
        let mounted: std::collections::HashSet<&str> =
            self.mounts.values().map(|s| s.as_str()).collect();
        // OldestFirst: smallest created_at_ns among un-pinned entries.
        let victim = self
            .adapters
            .values()
            .filter(|a| !mounted.contains(a.adapter_id.as_str()))
            .min_by_key(|a| (a.provenance.created_at_ns, a.adapter_id.clone()))
            .map(|a| a.adapter_id.clone());
        match victim {
            Some(id) => {
                self.adapters.remove(&id);
                self.adopted.remove(&id);
                Ok(())
            }
            // Every entry is pinned by an active mount.
            None => Err(MountError::CapacityExceeded {
                capacity: self.capacity,
            }),
        }
    }

    /// Fetch an adapter by id.
    pub fn get(&self, adapter_id: &str) -> Option<&AdapterArtifact> {
        self.adapters.get(adapter_id)
    }

    /// List all registered adapters, sorted by id for deterministic output.
    pub fn list(&self) -> Vec<&AdapterArtifact> {
        let mut out: Vec<&AdapterArtifact> = self.adapters.values().collect();
        out.sort_by(|a, b| a.adapter_id.cmp(&b.adapter_id));
        out
    }

    /// List only the adapters that can be hot-mounted (S8.4.8 mountable tier),
    /// sorted by id.
    pub fn list_mountable(&self) -> Vec<&AdapterArtifact> {
        let mut out: Vec<&AdapterArtifact> = self
            .adapters
            .values()
            .filter(|a| a.is_mountable())
            .collect();
        out.sort_by(|a, b| a.adapter_id.cmp(&b.adapter_id));
        out
    }

    /// Mount a registered, mountable adapter at a `(tier, model)` mount point.
    ///
    /// Replaces any adapter already mounted there. Errors if the adapter is
    /// unknown or is a baked variant ([`MountError::NotMountable`]).
    pub fn mount(&mut self, mount_id: MountId, adapter_id: &str) -> Result<(), MountError> {
        let artifact = self
            .adapters
            .get(adapter_id)
            .ok_or_else(|| MountError::UnknownAdapter {
                adapter_id: adapter_id.to_string(),
            })?;
        if !artifact.is_mountable() {
            return Err(MountError::NotMountable {
                adapter_id: adapter_id.to_string(),
            });
        }
        self.mounts.insert(mount_id, adapter_id.to_string());
        Ok(())
    }

    /// Record the outcome of the adoption gate for one adapter (S8.4.8).
    ///
    /// An **approved** decision clears the adapter for [`mount_gated`]; a
    /// rejected decision revokes any prior clearance (e.g. after a re-eval that
    /// no longer beats baseline). The adapter need not be registered yet — the
    /// clearance is keyed by id and consulted at mount time.
    ///
    /// [`mount_gated`]: Self::mount_gated
    pub fn record_adoption(&mut self, decision: &AdoptionDecision) {
        if decision.approved {
            self.adopted.insert(decision.adapter_id.clone());
        } else {
            self.adopted.remove(&decision.adapter_id);
        }
    }

    /// Whether `adapter_id` has cleared the adoption gate and may be mounted via
    /// [`mount_gated`](Self::mount_gated).
    pub fn is_adopted(&self, adapter_id: &str) -> bool {
        self.adopted.contains(adapter_id)
    }

    /// Adoption-gated mount — the entry point the router must use before serving
    /// a self-trained adapter (S8.4.8 "before the router mounts it").
    ///
    /// Identical to [`mount`](Self::mount) but additionally refuses, with
    /// [`MountError::NotAdopted`], any adapter that has not been cleared by
    /// [`record_adoption`](Self::record_adoption). The adoption check runs first
    /// so an un-gated adapter never reaches the format check.
    pub fn mount_gated(&mut self, mount_id: MountId, adapter_id: &str) -> Result<(), MountError> {
        if !self.adapters.contains_key(adapter_id) {
            return Err(MountError::UnknownAdapter {
                adapter_id: adapter_id.to_string(),
            });
        }
        if !self.is_adopted(adapter_id) {
            return Err(MountError::NotAdopted {
                adapter_id: adapter_id.to_string(),
            });
        }
        self.mount(mount_id, adapter_id)
    }

    /// Unmount whatever is mounted at `mount_id`, returning the adapter id that
    /// was removed.
    pub fn unmount(&mut self, mount_id: &MountId) -> Result<String, MountError> {
        self.mounts
            .remove(mount_id)
            .ok_or_else(|| MountError::NotMounted {
                mount_id: mount_id.clone(),
            })
    }

    /// The adapter id mounted at `mount_id`, if any.
    pub fn mounted_at(&self, mount_id: &MountId) -> Option<&str> {
        self.mounts.get(mount_id).map(|s| s.as_str())
    }

    /// All current mount points, sorted by `(tier, model)` for determinism.
    pub fn mounts(&self) -> Vec<MountPoint> {
        let mut out: Vec<MountPoint> = self
            .mounts
            .iter()
            .map(|(mount_id, adapter_id)| MountPoint {
                mount_id: mount_id.clone(),
                adapter_id: adapter_id.clone(),
            })
            .collect();
        out.sort_by(|a, b| {
            (a.mount_id.tier.as_str(), a.mount_id.model.as_str())
                .cmp(&(b.mount_id.tier.as_str(), b.mount_id.model.as_str()))
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{AdapterFormat, Provenance};
    use crate::method::{AdaptationMethod, HraKind, MergePath, ServingTier};

    fn mountable(id: &str, created: u64) -> AdapterArtifact {
        AdapterArtifact {
            adapter_id: id.to_string(),
            description: format!("desc-{id}"),
            format: AdapterFormat::LoraAdapter,
            merge_path: MergePath::Clean,
            serving_tier: ServingTier::MountableAdapter,
            weights_digest: "d".to_string(),
            adapter_path: None,
            merged_gguf_path: None,
            provenance: Provenance {
                base_model: "base-q4".to_string(),
                method: AdaptationMethod::default(),
                source_job: format!("job-{id}"),
                created_at_ns: created,
            },
        }
    }

    fn baked(id: &str, created: u64) -> AdapterArtifact {
        let mut a = mountable(id, created);
        a.format = AdapterFormat::BakedGguf;
        a.serving_tier = ServingTier::BakedVariant;
        a.merge_path = MergePath::Hadamard;
        a.provenance.method = AdaptationMethod::Hra {
            family: HraKind::Hira,
            rank: 128,
        };
        a
    }

    #[test]
    fn register_get_list_with_provenance() {
        let mut lib = AdapterLibrary::new(8);
        lib.register(mountable("a", 1)).unwrap();
        lib.register(mountable("b", 2)).unwrap();
        assert_eq!(lib.len(), 2);

        let got = lib.get("a").expect("registered");
        assert_eq!(got.provenance.base_model, "base-q4");
        assert_eq!(got.provenance.source_job, "job-a");
        assert_eq!(got.provenance.method, AdaptationMethod::default());

        let ids: Vec<&str> = lib.list().iter().map(|a| a.adapter_id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]); // sorted, deterministic
    }

    #[test]
    fn mount_unmount_round_trip() {
        let mut lib = AdapterLibrary::new(8);
        lib.register(mountable("a", 1)).unwrap();
        let mp = MountId::new("instinct", "base-q4");

        assert!(lib.mounted_at(&mp).is_none());
        lib.mount(mp.clone(), "a").unwrap();
        assert_eq!(lib.mounted_at(&mp), Some("a"));

        let mounts = lib.mounts();
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].adapter_id, "a");

        let removed = lib.unmount(&mp).unwrap();
        assert_eq!(removed, "a");
        assert!(lib.mounted_at(&mp).is_none());
    }

    #[test]
    fn mount_replaces_existing() {
        let mut lib = AdapterLibrary::new(8);
        lib.register(mountable("a", 1)).unwrap();
        lib.register(mountable("b", 2)).unwrap();
        let mp = MountId::new("instinct", "base-q4");
        lib.mount(mp.clone(), "a").unwrap();
        lib.mount(mp.clone(), "b").unwrap();
        assert_eq!(lib.mounted_at(&mp), Some("b"));
    }

    #[test]
    fn baked_variant_cannot_be_mounted() {
        let mut lib = AdapterLibrary::new(8);
        lib.register(baked("h", 1)).unwrap();
        let mp = MountId::new("instinct", "base-q4");
        assert_eq!(
            lib.mount(mp, "h"),
            Err(MountError::NotMountable {
                adapter_id: "h".to_string()
            })
        );
    }

    // ── Adoption gate (S8.4.8) ────────────────────────────────────────────────

    fn approved(id: &str) -> AdoptionDecision {
        AdoptionDecision {
            adapter_id: id.to_string(),
            approved: true,
            eval_passed: true,
            alignment_passed: true,
            reasons: vec![],
        }
    }

    fn rejected(id: &str) -> AdoptionDecision {
        AdoptionDecision {
            adapter_id: id.to_string(),
            approved: false,
            eval_passed: false,
            alignment_passed: true,
            reasons: vec!["eval: did not beat baseline".to_string()],
        }
    }

    #[test]
    fn mount_gated_rejects_unadopted_adapter() {
        let mut lib = AdapterLibrary::new(8);
        lib.register(mountable("a", 1)).unwrap();
        let mp = MountId::new("instinct", "base-q4");
        // Registered and mountable, but never gated → refused.
        assert_eq!(
            lib.mount_gated(mp, "a"),
            Err(MountError::NotAdopted {
                adapter_id: "a".to_string()
            })
        );
        assert!(!lib.is_adopted("a"));
    }

    #[test]
    fn mount_gated_allows_after_adoption() {
        let mut lib = AdapterLibrary::new(8);
        lib.register(mountable("a", 1)).unwrap();
        lib.record_adoption(&approved("a"));
        assert!(lib.is_adopted("a"));
        let mp = MountId::new("instinct", "base-q4");
        lib.mount_gated(mp.clone(), "a").unwrap();
        assert_eq!(lib.mounted_at(&mp), Some("a"));
    }

    #[test]
    fn mount_gated_rejects_unknown_adapter() {
        let mut lib = AdapterLibrary::new(8);
        let mp = MountId::new("instinct", "base-q4");
        assert_eq!(
            lib.mount_gated(mp, "ghost"),
            Err(MountError::UnknownAdapter {
                adapter_id: "ghost".to_string()
            })
        );
    }

    #[test]
    fn rejected_decision_does_not_clear_for_mount() {
        let mut lib = AdapterLibrary::new(8);
        lib.register(mountable("a", 1)).unwrap();
        lib.record_adoption(&rejected("a"));
        let mp = MountId::new("instinct", "base-q4");
        assert!(matches!(
            lib.mount_gated(mp, "a"),
            Err(MountError::NotAdopted { .. })
        ));
    }

    #[test]
    fn rejected_decision_revokes_prior_adoption() {
        let mut lib = AdapterLibrary::new(8);
        lib.register(mountable("a", 1)).unwrap();
        lib.record_adoption(&approved("a"));
        assert!(lib.is_adopted("a"));
        // A later re-eval that fails must revoke the clearance.
        lib.record_adoption(&rejected("a"));
        assert!(!lib.is_adopted("a"));
    }

    #[test]
    fn re_registering_revokes_adoption() {
        let mut lib = AdapterLibrary::new(8);
        lib.register(mountable("a", 1)).unwrap();
        lib.record_adoption(&approved("a"));
        // New weights under the same id ⇒ must re-earn adoption.
        lib.register(mountable("a", 2)).unwrap();
        assert!(!lib.is_adopted("a"));
    }

    #[test]
    fn eviction_drops_adoption() {
        let mut lib = AdapterLibrary::new(1);
        lib.register(mountable("old", 1)).unwrap();
        lib.record_adoption(&approved("old"));
        assert!(lib.is_adopted("old"));
        // Registering a second adapter evicts the (un-mounted) oldest.
        lib.register(mountable("new", 2)).unwrap();
        assert!(lib.get("old").is_none());
        assert!(!lib.is_adopted("old"));
    }

    #[test]
    fn mount_gated_still_enforces_mountability() {
        // Even if a baked variant were (wrongly) marked adopted, the format
        // check still refuses it — the gate is additive, not a bypass.
        let mut lib = AdapterLibrary::new(8);
        lib.register(baked("h", 1)).unwrap();
        lib.record_adoption(&approved("h"));
        let mp = MountId::new("instinct", "base-q4");
        assert_eq!(
            lib.mount_gated(mp, "h"),
            Err(MountError::NotMountable {
                adapter_id: "h".to_string()
            })
        );
    }

    #[test]
    fn mounting_unknown_adapter_errors() {
        let mut lib = AdapterLibrary::new(8);
        let mp = MountId::new("instinct", "base-q4");
        assert_eq!(
            lib.mount(mp, "nope"),
            Err(MountError::UnknownAdapter {
                adapter_id: "nope".to_string()
            })
        );
    }

    #[test]
    fn unmount_empty_point_errors() {
        let mut lib = AdapterLibrary::new(8);
        let mp = MountId::new("instinct", "base-q4");
        assert_eq!(
            lib.unmount(&mp),
            Err(MountError::NotMounted { mount_id: mp })
        );
    }

    #[test]
    fn list_mountable_filters_baked() {
        let mut lib = AdapterLibrary::new(8);
        lib.register(mountable("a", 1)).unwrap();
        lib.register(baked("h", 2)).unwrap();
        let ids: Vec<&str> = lib
            .list_mountable()
            .iter()
            .map(|a| a.adapter_id.as_str())
            .collect();
        assert_eq!(ids, vec!["a"]);
    }

    #[test]
    fn oldest_first_eviction_when_full() {
        let mut lib = AdapterLibrary::new(2);
        lib.register(mountable("old", 1)).unwrap();
        lib.register(mountable("mid", 5)).unwrap();
        // Registering a third evicts the oldest ("old", created_at 1).
        lib.register(mountable("new", 9)).unwrap();
        assert_eq!(lib.len(), 2);
        assert!(lib.get("old").is_none());
        assert!(lib.get("mid").is_some());
        assert!(lib.get("new").is_some());
    }

    #[test]
    fn mounted_adapter_is_pinned_against_eviction() {
        let mut lib = AdapterLibrary::new(2);
        lib.register(mountable("old", 1)).unwrap();
        lib.register(mountable("mid", 5)).unwrap();
        // Pin the oldest by mounting it.
        lib.mount(MountId::new("instinct", "base-q4"), "old")
            .unwrap();
        lib.register(mountable("new", 9)).unwrap();
        // "old" survived; "mid" (next-oldest, un-pinned) was evicted.
        assert!(lib.get("old").is_some());
        assert!(lib.get("mid").is_none());
        assert!(lib.get("new").is_some());
    }

    #[test]
    fn reject_when_full_policy_errors() {
        let mut lib = AdapterLibrary::with_policy(1, EvictionPolicy::RejectWhenFull);
        lib.register(mountable("a", 1)).unwrap();
        assert_eq!(
            lib.register(mountable("b", 2)),
            Err(MountError::CapacityExceeded { capacity: 1 })
        );
    }

    #[test]
    fn re_register_same_id_replaces_without_eviction() {
        let mut lib = AdapterLibrary::new(1);
        lib.register(mountable("a", 1)).unwrap();
        let mut updated = mountable("a", 1);
        updated.description = "updated".to_string();
        lib.register(updated).unwrap();
        assert_eq!(lib.len(), 1);
        assert_eq!(lib.get("a").unwrap().description, "updated");
    }

    #[test]
    fn capacity_floor_is_one() {
        let lib = AdapterLibrary::new(0);
        assert_eq!(lib.capacity(), 1);
    }
}
