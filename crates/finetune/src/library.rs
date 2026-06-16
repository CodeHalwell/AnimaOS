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
use std::collections::HashMap;
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
    /// The adapter cleared the automated adoption gate but has not received
    /// operator sign-off (the E15 `WeightUpdate` proposal is not yet approved),
    /// so [`AdapterLibrary::mount_gated`] refuses to serve it. This is the human
    /// half of the two-stage gate.
    NotOperatorApproved {
        /// The id of the adapter awaiting operator approval.
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
            MountError::NotOperatorApproved { adapter_id } => write!(
                f,
                "adapter `{adapter_id}` has not received operator sign-off (E15 approval)"
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
    /// Adapter ids that have cleared the **automated** half of the adoption gate
    /// (eval + alignment) via [`AdapterLibrary::record_adoption`], mapped to the
    /// exact `weights_digest` that was evaluated. [`AdapterLibrary::mount_gated`]
    /// only honours it when the stored digest still matches the registered
    /// artifact, so recording a stale decision after the id's weights were
    /// replaced cannot adopt the new, unevaluated weights. An adapter loses this
    /// when it is (re)registered (new weights), evicted, or its adoption is
    /// revoked.
    adopted: HashMap<String, String>,
    /// Adapter ids that have additionally received **operator** sign-off via
    /// [`AdapterLibrary::record_operator_approval`] (the human half of the gate:
    /// the E15 `WeightUpdate` proposal was approved), mapped to the exact
    /// `weights_digest` the operator approved. [`AdapterLibrary::mount_gated`]
    /// requires both this *and* [`Self::adopted`], and the stored digest must
    /// match the currently-registered artifact — so approving a stale proposal
    /// for an id whose weights have since been replaced never clears the new
    /// weights. Cleared on the same events as `adopted`.
    operator_approved: HashMap<String, String>,
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
            adopted: HashMap::new(),
            operator_approved: HashMap::new(),
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
    ///
    /// Any registration of an id — first-time or replacement — revokes that id's
    /// adoption clearance and drops any live mounts of it, so the registered
    /// weights must clear the gate ([`record_adoption`]) and be re-mounted before
    /// the router serves them. This is what prevents a clearance recorded for an
    /// id *before* its artifact exists from auto-adopting whatever weights are
    /// later registered under that id: clearance only ever applies to weights
    /// that were already in the library when it was recorded.
    ///
    /// [`record_adoption`]: Self::record_adoption
    pub fn register(&mut self, artifact: AdapterArtifact) -> Result<(), MountError> {
        // A (re)registration changes (or first establishes) the bytes behind this
        // id, so any standing adoption clearance and any live mount of it are
        // stale and must be cleared before the new weights can be served. Doing
        // this on every path — not just replacement — also closes the
        // "adopt-then-register" pre-clearance hole. For a brand-new id these are
        // no-ops (nothing adopted or mounted yet).
        self.adopted.remove(&artifact.adapter_id);
        self.operator_approved.remove(&artifact.adapter_id);
        self.mounts
            .retain(|_, mounted| mounted != &artifact.adapter_id);
        if self.adapters.contains_key(&artifact.adapter_id) {
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
                self.operator_approved.remove(&id);
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
    /// An **approved** decision clears the evaluated weights for [`mount_gated`];
    /// a rejected decision revokes any prior clearance (e.g. after a re-eval that
    /// no longer beats baseline) and unmounts the adapter.
    ///
    /// The clearance is bound to [`AdoptionDecision::weights_digest`] (the digest
    /// of the artifact the decision was made about) and checked against the
    /// registered artifact by [`is_adopted`], so a stale decision recorded after
    /// the id's weights were replaced cannot adopt the new, unevaluated weights.
    /// (Registration already clears clearance, so recording before the artifact
    /// exists also has no lasting effect.)
    ///
    /// [`mount_gated`]: Self::mount_gated
    /// [`is_adopted`]: Self::is_adopted
    pub fn record_adoption(&mut self, decision: &AdoptionDecision) {
        if decision.approved {
            self.adopted
                .insert(decision.adapter_id.clone(), decision.weights_digest.clone());
        } else {
            // Revoking automated clearance also drops the operator sign-off (it
            // was for weights that no longer pass) and unmounts any live serving
            // of this adapter — otherwise the router would keep serving weights
            // the gate just rejected without ever calling `mount_gated` again.
            self.adopted.remove(&decision.adapter_id);
            self.operator_approved.remove(&decision.adapter_id);
            self.mounts
                .retain(|_, mounted| mounted != &decision.adapter_id);
        }
    }

    /// Record operator sign-off — the **human** half of the adoption gate — for
    /// the `weights_digest` weights of `adapter_id` (S8.4.8). Call this when the
    /// E15 `WeightUpdate` proposal that [`crate`]'s lifecycle bridge created from
    /// the adoption decision is approved by an operator, passing the digest that
    /// proposal carried; [`mount_gated`](Self::mount_gated) then requires it on
    /// top of automated adoption.
    ///
    /// The digest is recorded (not just the id) so that approving a **stale**
    /// proposal — one whose weights have since been replaced under the same id —
    /// does not clear the new weights: [`is_operator_approved`] only honours the
    /// approval when the stored digest still matches the registered artifact.
    ///
    /// [`is_operator_approved`]: Self::is_operator_approved
    pub fn record_operator_approval(&mut self, adapter_id: &str, weights_digest: &str) {
        self.operator_approved
            .insert(adapter_id.to_string(), weights_digest.to_string());
    }

    /// Withdraw operator sign-off for `adapter_id` (e.g. the proposal was
    /// rejected or rescinded) and unmount any live serving of it, so revoking
    /// approval immediately stops the router serving the adapter — human
    /// sign-off stays enforced after an approval changes, mirroring the
    /// automated rejection branch of [`record_adoption`](Self::record_adoption).
    /// A subsequent [`mount_gated`](Self::mount_gated) refuses with
    /// [`MountError::NotOperatorApproved`] until re-approved.
    pub fn revoke_operator_approval(&mut self, adapter_id: &str) {
        self.operator_approved.remove(adapter_id);
        self.mounts.retain(|_, mounted| mounted != adapter_id);
    }

    /// Whether `adapter_id` has cleared the **automated** adoption gate for its
    /// **currently-registered** weights, and may be mounted via
    /// [`mount_gated`](Self::mount_gated) once operator sign-off is also recorded.
    /// Returns `false` when the adapter is unregistered, has no recorded
    /// clearance, or the cleared digest no longer matches the registered artifact
    /// (a stale decision was recorded after the weights were replaced).
    pub fn is_adopted(&self, adapter_id: &str) -> bool {
        match (self.adopted.get(adapter_id), self.adapters.get(adapter_id)) {
            (Some(adopted_digest), Some(artifact)) => adopted_digest == &artifact.weights_digest,
            _ => false,
        }
    }

    /// Whether `adapter_id` has operator sign-off for its **currently-registered**
    /// weights (the human half of the gate). Returns `false` when the adapter is
    /// unregistered, has no recorded approval, or the approved digest no longer
    /// matches the registered artifact (a stale proposal was approved after the
    /// weights were replaced).
    pub fn is_operator_approved(&self, adapter_id: &str) -> bool {
        match (
            self.operator_approved.get(adapter_id),
            self.adapters.get(adapter_id),
        ) {
            (Some(approved_digest), Some(artifact)) => approved_digest == &artifact.weights_digest,
            _ => false,
        }
    }

    /// Adoption-gated mount — the entry point the router must use before serving
    /// a self-trained adapter (S8.4.8 "before the router mounts it").
    ///
    /// Identical to [`mount`](Self::mount) but additionally enforces **both**
    /// halves of the adoption gate, so self-trained weights never reach a serving
    /// tier on automated clearance alone:
    /// 1. [`MountError::NotAdopted`] — the automated eval + alignment gate has not
    ///    cleared the adapter ([`record_adoption`](Self::record_adoption)).
    /// 2. [`MountError::NotOperatorApproved`] — the operator has not signed off on
    ///    the resulting proposal ([`record_operator_approval`](Self::record_operator_approval)).
    ///
    /// The checks run before the format check so an un-gated adapter never reaches
    /// it.
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
        if !self.is_operator_approved(adapter_id) {
            return Err(MountError::NotOperatorApproved {
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
    fn reregistering_replaced_id_revokes_adoption_and_unmounts() {
        let mut lib = AdapterLibrary::new(8);
        lib.register(mountable("a", 1)).unwrap();
        lib.record_adoption(&approved("a", "d"));
        lib.record_operator_approval("a", "d");
        let mp = MountId::new("instinct", "base-q4");
        lib.mount_gated(mp.clone(), "a").unwrap();
        assert_eq!(lib.mounted_at(&mp), Some("a"));

        // Re-register the same id with fresh (un-gated) weights.
        lib.register(mountable("a", 5)).unwrap();

        // Adoption + operator clearance are revoked and the stale mount is gone,
        // so the router can no longer serve the swapped-in weights until they
        // clear both halves of the gate again.
        assert!(!lib.is_adopted("a"));
        assert!(!lib.is_operator_approved("a"));
        assert!(lib.mounted_at(&mp).is_none());
        assert_eq!(
            lib.mount_gated(mp, "a"),
            Err(MountError::NotAdopted {
                adapter_id: "a".to_string()
            })
        );
    }

    #[test]
    fn adoption_recorded_before_registration_does_not_pre_clear_weights() {
        let mut lib = AdapterLibrary::new(8);
        // Clearance recorded for an id that has no artifact yet.
        lib.record_adoption(&approved("a", "d"));
        // Registering *any* weights under that id must not inherit the stale
        // clearance — those bytes were never evaluated.
        lib.register(mountable("a", 1)).unwrap();
        assert!(!lib.is_adopted("a"));
        let mp = MountId::new("instinct", "base-q4");
        assert_eq!(
            lib.mount_gated(mp, "a"),
            Err(MountError::NotAdopted {
                adapter_id: "a".to_string()
            })
        );
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

    fn approved(id: &str, digest: &str) -> AdoptionDecision {
        AdoptionDecision {
            adapter_id: id.to_string(),
            weights_digest: digest.to_string(),
            approved: true,
            eval_passed: true,
            alignment_passed: true,
            reasons: vec![],
        }
    }

    fn rejected(id: &str, digest: &str) -> AdoptionDecision {
        AdoptionDecision {
            adapter_id: id.to_string(),
            weights_digest: digest.to_string(),
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
    fn mount_gated_allows_after_both_gate_halves() {
        let mut lib = AdapterLibrary::new(8);
        lib.register(mountable("a", 1)).unwrap();
        lib.record_adoption(&approved("a", "d"));
        assert!(lib.is_adopted("a"));
        let mp = MountId::new("instinct", "base-q4");
        // Automated clearance without operator sign-off is still refused.
        assert_eq!(
            lib.mount_gated(mp.clone(), "a"),
            Err(MountError::NotOperatorApproved {
                adapter_id: "a".to_string()
            })
        );
        // Both halves recorded ⇒ mount succeeds.
        lib.record_operator_approval("a", "d");
        lib.mount_gated(mp.clone(), "a").unwrap();
        assert_eq!(lib.mounted_at(&mp), Some("a"));
    }

    #[test]
    fn revoking_adoption_unmounts_live_adapter() {
        let mut lib = AdapterLibrary::new(8);
        lib.register(mountable("a", 1)).unwrap();
        lib.record_adoption(&approved("a", "d"));
        lib.record_operator_approval("a", "d");
        let mp = MountId::new("instinct", "base-q4");
        lib.mount_gated(mp.clone(), "a").unwrap();
        assert_eq!(lib.mounted_at(&mp), Some("a"));

        // A later rejecting decision (failed re-eval / alignment veto) must pull
        // the live mount, not just the clearance bit.
        lib.record_adoption(&rejected("a", "d"));
        assert!(!lib.is_adopted("a"));
        assert!(!lib.is_operator_approved("a"));
        assert!(lib.mounted_at(&mp).is_none());
    }

    #[test]
    fn stale_operator_approval_does_not_clear_replaced_weights() {
        let mut lib = AdapterLibrary::new(8);
        // v1 of adapter "a" with a distinct digest, fully gated and mounted.
        let mut v1 = mountable("a", 1);
        v1.weights_digest = "digest-v1".to_string();
        lib.register(v1).unwrap();
        lib.record_adoption(&approved("a", "digest-v1"));
        lib.record_operator_approval("a", "digest-v1");
        assert!(lib.is_operator_approved("a"));

        // New weights are registered under the same id (clears both gates).
        let mut v2 = mountable("a", 2);
        v2.weights_digest = "digest-v2".to_string();
        lib.register(v2).unwrap();
        lib.record_adoption(&approved("a", "digest-v2"));

        // Approving the *stale* v1 proposal (digest-v1) must not clear v2: the
        // stored digest no longer matches the registered artifact.
        lib.record_operator_approval("a", "digest-v1");
        assert!(!lib.is_operator_approved("a"));
        let mp = MountId::new("instinct", "base-q4");
        assert_eq!(
            lib.mount_gated(mp.clone(), "a"),
            Err(MountError::NotOperatorApproved {
                adapter_id: "a".to_string()
            })
        );

        // Approving the correct v2 digest clears it.
        lib.record_operator_approval("a", "digest-v2");
        lib.mount_gated(mp, "a").unwrap();
    }

    #[test]
    fn stale_adoption_decision_does_not_adopt_replaced_weights() {
        let mut lib = AdapterLibrary::new(8);
        // v1 trained and registered, then replaced by v2 under the same id.
        let mut v1 = mountable("a", 1);
        v1.weights_digest = "digest-v1".to_string();
        lib.register(v1).unwrap();
        let mut v2 = mountable("a", 2);
        v2.weights_digest = "digest-v2".to_string();
        lib.register(v2).unwrap();

        // A decision evaluated against v1 arrives late and is recorded now. It
        // must not adopt v2 — those weights were never evaluated.
        lib.record_adoption(&approved("a", "digest-v1"));
        assert!(!lib.is_adopted("a"));

        // The decision for the actually-registered v2 weights does adopt it.
        lib.record_adoption(&approved("a", "digest-v2"));
        assert!(lib.is_adopted("a"));
    }

    #[test]
    fn revoking_operator_approval_unmounts_live_adapter() {
        let mut lib = AdapterLibrary::new(8);
        lib.register(mountable("a", 1)).unwrap();
        lib.record_adoption(&approved("a", "d"));
        lib.record_operator_approval("a", "d");
        let mp = MountId::new("instinct", "base-q4");
        lib.mount_gated(mp.clone(), "a").unwrap();
        assert_eq!(lib.mounted_at(&mp), Some("a"));

        // Rescinding operator sign-off must stop live serving immediately.
        lib.revoke_operator_approval("a");
        assert!(!lib.is_operator_approved("a"));
        assert!(lib.mounted_at(&mp).is_none());
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
        lib.record_adoption(&rejected("a", "d"));
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
        lib.record_adoption(&approved("a", "d"));
        assert!(lib.is_adopted("a"));
        // A later re-eval that fails must revoke the clearance.
        lib.record_adoption(&rejected("a", "d"));
        assert!(!lib.is_adopted("a"));
    }

    #[test]
    fn re_registering_revokes_adoption() {
        let mut lib = AdapterLibrary::new(8);
        lib.register(mountable("a", 1)).unwrap();
        lib.record_adoption(&approved("a", "d"));
        // New weights under the same id ⇒ must re-earn adoption.
        lib.register(mountable("a", 2)).unwrap();
        assert!(!lib.is_adopted("a"));
    }

    #[test]
    fn eviction_drops_adoption() {
        let mut lib = AdapterLibrary::new(1);
        lib.register(mountable("old", 1)).unwrap();
        lib.record_adoption(&approved("old", "d"));
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
        lib.record_adoption(&approved("h", "d"));
        lib.record_operator_approval("h", "d");
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
