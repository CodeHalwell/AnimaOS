#![forbid(unsafe_code)]

//! Self-preservation plane: autonomous lifecycle director.

pub mod audit;
#[cfg(feature = "std")]
pub mod cortex_bridge;
#[cfg(feature = "std")]
pub mod defence_bridge;
#[cfg(feature = "std")]
pub mod dispatch;
pub mod episodic;
pub mod gate;
pub mod identity;
pub mod kv_gate;
#[cfg(feature = "std")]
pub mod metacognition;
/// E12 — Motivation ↔ Striatal Gate integration (drive-augmented arbitration).
pub mod motivation_gate;
#[cfg(feature = "std")]
pub mod prospective;
pub mod router;
#[cfg(feature = "std")]
pub mod sensors;
pub mod sleep;
#[cfg(feature = "std")]
pub mod watchdog;

pub use audit::{AuditEntry, AuditLog};
#[cfg(feature = "std")]
pub use cortex_bridge::{
    archive_episode, cortex_handle, ChatCortexBridge, CortexBackend, CortexError, CortexHandle,
    CortexInvocationResult, FnDispatcher, InvokeMemoryScope, InvokeRequest, MockCortexBridge,
    PythonCortexBridge, ToolDispatcher, ToolSpec, DEFAULT_MAX_TOOL_CALLS, DEFAULT_MAX_TURNS,
};
#[cfg(feature = "std")]
pub use defence_bridge::push_defence_outcome;
#[cfg(feature = "std")]
pub use dispatch::{redact_url, EgressAwareDispatcher};
pub use episodic::{
    embed_episode, make_episode_archived_item, make_episode_provenance, pack_episode_payload,
    unpack_episode, EpisodeMatch, EpisodeQuery, EpisodeRecord, EpisodeStore,
};
pub use gate::{
    record_gate_decision, CostClass, EventFeatures, Gate, GateConfig, GateDecision, GateOverride,
    HomeostaticSignals, SemanticClass, ThresholdGate,
};
// E12 — Motivation ↔ Striatal Gate: drive-augmented arbitration types.
pub use identity::{
    AgentSelfModel, IdentityDocument, IdentityError, IdentityMemory, ObservedPattern,
    RecurringTask, SystemPolicies, UserPreferences,
};
pub use kv_gate::{
    effective_budget_under_pressure, gate_working_context, gate_working_context_with_signals,
    ContextBlock, GatePassResult,
};
#[cfg(feature = "std")]
pub use metacognition::{CalibrationRecord, ConfidenceScore, ConfidenceTracker, HelpRequest};
pub use motivation_gate::{candidate_from_event, MotivatedGate};
#[cfg(feature = "std")]
pub use prospective::{
    inject_due_intentions, CompletionOutcome, Intention, IntentionStore, IntentionStoreError,
    DEFAULT_OVERDUE_GRACE_NS,
};
#[cfg(feature = "std")]
pub use router::TierBackends;
pub use router::{
    build_routed_request, default_routes, model_selector_for_cost_class,
    record_modulated_router_decision, record_router_decision, validate_route, MemoryScope,
    ModelSelector, ModulationDecision, PromptScaffold, Route, RouteError, RouteId, Router,
    StaticRouter, TerminationPolicy, ToolScope,
};
/// Object-safe alias for the provider-agnostic LLM backend trait.
///
/// [`router::TierBackends`] stores `Arc<dyn LlmBackendRef>` per tier; the alias
/// keeps the router module decoupled from the `scheduler` crate path while
/// pointing at the very same trait the rest of `vita` already uses
/// ([`scheduler::backend::LlmBackend`]).
pub use scheduler::LlmBackend as LlmBackendRef;
#[cfg(feature = "std")]
pub use sensors::AuditSignalPublisher;
pub use sleep::{SleepMaintenanceReport, SleepRoutine, SleepRoutineOutcome};
#[cfg(feature = "std")]
pub use watchdog::{AgentSnapshot, CognitiveWatchdog, WatchdogConfig, WatchdogTrip};

#[cfg(not(feature = "std"))]
extern crate alloc;
#[cfg(not(feature = "std"))]
use alloc::sync::Arc;
#[cfg(not(feature = "std"))]
use core::time::Duration;
#[cfg(feature = "std")]
use std::sync::{Arc, Mutex};
#[cfg(feature = "std")]
use std::time::Duration;

use interoception::HomeostaticMonitor;
#[cfg(feature = "std")]
use interoception::{InteroceptiveSensorBundle, NullPublisher};
use memory::{
    AuditTraceEntry, CompilationConfig, DreamConfig, L1PruningStore, VirtualContextManager,
};
use scheduler::{CancellationToken, IterationAwareMlfq, LlmBackend, Task, TaskAgenda};
use senses::{HumanGuidance, SensoryBridge, SensoryBridgeError, SensoryPriority};
#[cfg(feature = "std")]
use skills::{EpisodeSummary, PromotionGateConfig, ReflectionConfig, SkillRegistry};
use sleep::{CompilationContext, DreamContext, PruningContext, ReplayContext};

#[cfg(feature = "std")]
pub use sleep::{run_self_improvement_reflection, ReflectionRegistration};

/// Lifecycle runtime state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleState {
    /// Active sensorimotor execution loop.
    Awake,
    /// Maintenance and consolidation mode.
    Sleep,
}

/// Runtime limits supplied to lifecycle control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleConfig {
    /// Maximum context window available to the active agent.
    pub max_context: u32,
}

/// Errors from lifecycle operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleError {
    /// Sensory bridge read failed.
    SensoryInput(SensoryBridgeError),
}

impl From<SensoryBridgeError> for LifecycleError {
    fn from(value: SensoryBridgeError) -> Self {
        Self::SensoryInput(value)
    }
}

/// Primary autonomous lifecycle manager.
#[derive(Clone)]
pub struct LifecycleManager {
    /// Human-readable agent identifier (carried into audit entries).
    pub agent_id: String,
    /// Human sensory bridge.
    pub senses: SensoryBridge,
    /// Active working memory (L1 token-count tracker).
    pub memory: VirtualContextManager,
    /// L1 episodic memory store — the named-node layer that the pruning phase
    /// operates on during each sleep cycle (E3.5).
    pub l1_memory: L1PruningStore,
    /// Elapsed seconds applied to each sleep-cycle pruning pass.  Defaults to
    /// `1.0`; callers may adjust this to match the wall-clock cadence of their
    /// sleep schedules.
    pub pruning_elapsed: f32,
    /// L3 cerebral archive for memory consolidation across sleep cycles (E2.6).
    ///
    /// `None` when running without persistent storage (e.g. in tests that do not
    /// require L3 persistence).
    pub l3_archive: Option<memory::L3Archive>,
    /// Monotonically increasing ID counter for L3 archive entries.
    pub next_archive_id: u64,
    /// Replay configuration for the `GenerativeReplay` sleep phase (E3.6).
    ///
    /// Applied every sleep cycle when an L3 archive is configured.
    pub replay_config: memory::ReplayConfig,
    /// Dream-exploration configuration for the `DreamExploration` sleep phase (E3.7).
    ///
    /// Applied every sleep cycle when an L3 archive is configured.
    pub dream_config: DreamConfig,
    /// Policy-compilation configuration for the `PolicyCompilation` sleep phase (E3.8).
    ///
    /// `None` disables file output (compilation runs in-memory only, no JSONL files written).
    pub compilation_config: Option<CompilationConfig>,
    /// Task scheduler.
    pub scheduler: IterationAwareMlfq,
    /// Agenda of pending tasks.
    pub agenda: TaskAgenda,
    /// Active lifecycle state.
    pub state: LifecycleState,
    /// Current policy bounds consumed from the human signal channel.
    pub policy_bounds: HumanGuidance,
    /// Runtime configuration.
    pub config: LifecycleConfig,
    /// LLM backend used to execute dispatched tasks.
    pub backend: Arc<dyn LlmBackend>,
    /// Per-agent audit log.
    pub audit: AuditLog,
    /// Cancellation handle for the dispatch currently in flight.  A fresh
    /// [`CancellationToken`] is installed before every dispatch; external
    /// callers (signal handlers, stress monitors, timeouts) can trip the
    /// running task via [`LifecycleManager::cancel_current_task`].
    task_cancel: Arc<Mutex<CancellationToken>>,
    /// Monotonically increasing counter for sensory-derived task IDs.
    ///
    /// Persisted on the lifecycle (not reset per `somatic_execution_loop` call)
    /// so that gate-decision audit entries carry unique event IDs even when the
    /// loop is called multiple times on the same `LifecycleManager`.
    /// Starts at `2^63` to avoid collisions with caller-supplied task IDs.
    next_sensory_task_id: u64,
    /// Optional iteration limit to allow bounded runs.
    pub max_iterations: Option<u32>,
    iterations: u32,
    /// Last memory pressure level emitted to the audit log.  Used to suppress
    /// duplicate `MemoryPressureEvent` entries — only transitions are logged.
    last_pressure_level: memory::MemoryPressureEvent,
    /// Optional interoceptive sensor bundle for 1 Hz signal publication (E5.7).
    ///
    /// When `Some`, each somatic-loop iteration calls `tick()` on the bundle and
    /// pushes an [`AuditEntry::InteroceptiveSnapshot`] to the audit log so the
    /// production audit pipeline satisfies the EX.2 wiring requirement.
    ///
    /// Wrapped in `Arc` so `LifecycleManager::clone()` shares the same bundle
    /// (sensors are inherently process-global resources).
    #[cfg(feature = "std")]
    pub sensor_bundle: Option<Arc<InteroceptiveSensorBundle>>,
    /// Optional E12 motivated Striatal Gate (drive-augmented arbitration).
    ///
    /// `None` by default — when absent the somatic loop's gate behaviour is
    /// byte-for-byte identical to before this integration.  Install it
    /// additively via [`LifecycleManager::enable_motivation`] /
    /// [`LifecycleManager::with_motivation`].  When `Some`, each somatic-loop
    /// iteration refreshes the gate's drive snapshot from the current
    /// interoceptive reading and pushes [`AuditEntry::DriveStateSnapshot`] +
    /// [`AuditEntry::AffectStateSnapshot`] entries.
    ///
    /// Wrapped in `Arc<Mutex<…>>` (mirroring `task_cancel`) so
    /// `LifecycleManager::clone()` shares one gate — drive satiation/mastery
    /// state is inherently agent-global — and so the `&mut` `update_signals`
    /// call and the `&self` `decide_motivated` call can both go through it.
    #[cfg(feature = "std")]
    pub motivated_gate: Option<Arc<Mutex<MotivatedGate>>>,
    /// Optional E9 S9.5 per-tier backend map (router-aware dispatch).
    ///
    /// `None` by default — when absent, every task dispatches through the single
    /// [`LifecycleManager::backend`], so behaviour is byte-for-byte identical to
    /// before this integration.  Install it additively via
    /// [`LifecycleManager::with_tier_backends`] /
    /// [`LifecycleManager::set_tier_backends`].  When `Some`, the task-dispatch
    /// path resolves the gate's [`CostClass`] (via [`ThresholdGate`]) to a
    /// [`ModelSelector`] tier and dispatches to the backend bound to that tier,
    /// emitting a [`AuditEntry::RouterDecision`] recording the selected tier.
    ///
    /// Wrapped only in the map's own `Arc`s (each tier backend is already
    /// `Arc<dyn LlmBackendRef>`), so `LifecycleManager::clone()` shares the
    /// backends — consistent with the single-backend field.
    #[cfg(feature = "std")]
    pub tier_backends: Option<router::TierBackends>,
    /// Optional E11 (S11.5) self-improvement skill registry.
    ///
    /// `None` by default — when absent, the Dreaming sleep phase runs exactly
    /// as before (no reflection, no skill registration), so behaviour is
    /// byte-for-byte identical to before this integration.  Install it
    /// additively via [`LifecycleManager::enable_skill_reflection`] /
    /// [`LifecycleManager::with_skill_registry`].  When `Some`, each sleep
    /// cycle's Dreaming phase reflects over the buffered
    /// [`recent_episode_summaries`](Self::recent_episode_summaries), drafts
    /// skills for friction patterns above threshold, and registers
    /// agent-authored `Proposed` drafts into this registry (emitting
    /// `SkillReflectionCompleted` + `SkillRegistered` audit entries).
    ///
    /// `vita` never routes the resulting pending proposals into the E15
    /// approval queue — that would require a `vita → lifecycle` dependency and
    /// create a cycle.  The hosted kernel (which may depend on both) drains the
    /// registry's pending agent-authored skills through
    /// `lifecycle::SkillApprovalBridge` instead.
    ///
    /// Wrapped in `Arc<Mutex<…>>` (mirroring `task_cancel`) because
    /// [`SkillRegistry`] is not `Clone`, so `LifecycleManager::clone()` shares
    /// one registry — consistent with the other shared-state fields.
    #[cfg(feature = "std")]
    pub skill_registry: Option<Arc<Mutex<SkillRegistry>>>,
    /// Reflection configuration applied during the Dreaming phase (E11 S11.5).
    ///
    /// Only consulted when [`skill_registry`](Self::skill_registry) is `Some`.
    #[cfg(feature = "std")]
    pub reflection_config: ReflectionConfig,
    /// Promotion-gate configuration for agent-authored skill drafts (E11 S11.5).
    ///
    /// Defaults to operator gating (`auto_promote_agent_skills: false`) so
    /// reflection-authored skills land as `Proposed` and await approval through
    /// the hosted approval-queue bridge.  Only consulted when
    /// [`skill_registry`](Self::skill_registry) is `Some`.
    #[cfg(feature = "std")]
    pub promotion_gate_config: PromotionGateConfig,
    /// Buffer of recent episode summaries the Dreaming-phase reflection consumes
    /// (E11 S11.5).
    ///
    /// Callers push summaries via
    /// [`record_episode_summary`](Self::record_episode_summary) as episodes
    /// complete.  Empty by default, so reflection is a no-op until episodes are
    /// recorded.
    #[cfg(feature = "std")]
    pub recent_episode_summaries: Vec<EpisodeSummary>,
}

impl std::fmt::Debug for LifecycleManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LifecycleManager")
            .field("agent_id", &self.agent_id)
            .field("state", &self.state)
            .field("config", &self.config)
            .field("policy_bounds", &self.policy_bounds)
            .field("agenda_len", &self.agenda.len())
            .field("l1_memory_nodes", &self.l1_memory.len())
            .field("pruning_elapsed", &self.pruning_elapsed)
            .field(
                "replay_config_threshold",
                &self.replay_config.accuracy_threshold,
            )
            .field("dream_config_seed", &self.dream_config.seed)
            .field(
                "compilation_config",
                &self
                    .compilation_config
                    .as_ref()
                    .map(|c| c.output_dir.display().to_string()),
            )
            .field("backend", &self.backend.id())
            .field("audit_len", &self.audit.len())
            .field("iterations", &self.iterations)
            .field("max_iterations", &self.max_iterations)
            .finish()
    }
}

impl LifecycleManager {
    /// Constructs a new manager in the awake state.
    pub fn new(
        agent_id: impl Into<String>,
        senses: SensoryBridge,
        memory: VirtualContextManager,
        config: LifecycleConfig,
        initial_bounds: HumanGuidance,
        backend: Arc<dyn LlmBackend>,
        max_iterations: Option<u32>,
    ) -> Self {
        let agent_id: String = agent_id.into();
        #[cfg(feature = "std")]
        let audit = AuditLog::from_env(&agent_id);
        #[cfg(not(feature = "std"))]
        let audit = AuditLog::new();
        Self {
            agent_id,
            senses,
            memory,
            l1_memory: L1PruningStore::new(),
            pruning_elapsed: 1.0,
            l3_archive: None,
            next_archive_id: 0,
            replay_config: memory::ReplayConfig::default(),
            dream_config: DreamConfig::default(),
            compilation_config: None,
            scheduler: IterationAwareMlfq::default(),
            agenda: TaskAgenda::new(),
            state: LifecycleState::Awake,
            policy_bounds: initial_bounds,
            config,
            backend,
            audit,
            task_cancel: Arc::new(Mutex::new(CancellationToken::new())),
            next_sensory_task_id: 1u64 << 63,
            max_iterations,
            iterations: 0,
            last_pressure_level: memory::MemoryPressureEvent::Normal,
            #[cfg(feature = "std")]
            sensor_bundle: None::<Arc<InteroceptiveSensorBundle>>,
            #[cfg(feature = "std")]
            motivated_gate: None::<Arc<Mutex<MotivatedGate>>>,
            #[cfg(feature = "std")]
            tier_backends: None::<router::TierBackends>,
            #[cfg(feature = "std")]
            skill_registry: None::<Arc<Mutex<SkillRegistry>>>,
            #[cfg(feature = "std")]
            reflection_config: ReflectionConfig::default(),
            // Operator gating by default: agent skills land as `Proposed`.
            #[cfg(feature = "std")]
            promotion_gate_config: PromotionGateConfig {
                auto_promote_agent_skills: false,
            },
            #[cfg(feature = "std")]
            recent_episode_summaries: Vec::new(),
        }
    }

    /// Installs the E9 S9.5 per-tier backend map on this manager (additive).
    ///
    /// After this call, the task-dispatch path selects the LLM backend bound to
    /// the gate decision's [`CostClass`] tier instead of the single
    /// [`LifecycleManager::backend`].  Leaves the constructor signature
    /// untouched — call this after [`LifecycleManager::new`].
    ///
    /// Use [`router::TierBackends::uniform`] to point all three tiers at one
    /// backend for backward-compatible behaviour through the new code path.
    #[cfg(feature = "std")]
    pub fn set_tier_backends(&mut self, tiers: router::TierBackends) {
        self.tier_backends = Some(tiers);
    }

    /// Builder variant of [`LifecycleManager::set_tier_backends`] returning
    /// `self` for chaining off [`LifecycleManager::new`].
    #[cfg(feature = "std")]
    pub fn with_tier_backends(mut self, tiers: router::TierBackends) -> Self {
        self.set_tier_backends(tiers);
        self
    }

    /// `true` when the per-tier backend map is installed and active.
    #[cfg(feature = "std")]
    pub fn tier_dispatch_enabled(&self) -> bool {
        self.tier_backends.is_some()
    }

    /// Enables the E12 motivated Striatal Gate on this manager (additive).
    ///
    /// Installs the supplied [`MotivatedGate`] so that subsequent
    /// [`somatic_execution_loop`] iterations route gate decisions through the
    /// drive hierarchy and emit drive/affect audit entries.  Leaves the
    /// constructor signature untouched — call this after [`LifecycleManager::new`].
    #[cfg(feature = "std")]
    pub fn enable_motivation(&mut self, gate: MotivatedGate) {
        self.motivated_gate = Some(Arc::new(Mutex::new(gate)));
    }

    /// Builder variant of [`LifecycleManager::enable_motivation`] returning
    /// `self` for chaining off [`LifecycleManager::new`].
    #[cfg(feature = "std")]
    pub fn with_motivation(mut self, gate: MotivatedGate) -> Self {
        self.enable_motivation(gate);
        self
    }

    /// `true` when the motivated Striatal Gate is installed and active.
    #[cfg(feature = "std")]
    pub fn motivation_enabled(&self) -> bool {
        self.motivated_gate.is_some()
    }

    // ── E11 S11.5 — Dreaming-phase self-improvement reflection ────────────────

    /// Installs an E11 [`SkillRegistry`] on this manager (additive).
    ///
    /// After this call the Dreaming sleep phase reflects over the buffered
    /// episode summaries and registers agent-authored skill drafts into this
    /// registry.  Leaves the constructor signature untouched — call this after
    /// [`LifecycleManager::new`].
    #[cfg(feature = "std")]
    pub fn enable_skill_reflection(&mut self, registry: SkillRegistry) {
        self.skill_registry = Some(Arc::new(Mutex::new(registry)));
    }

    /// Builder variant of [`LifecycleManager::enable_skill_reflection`]
    /// returning `self` for chaining off [`LifecycleManager::new`].
    #[cfg(feature = "std")]
    pub fn with_skill_registry(mut self, registry: SkillRegistry) -> Self {
        self.enable_skill_reflection(registry);
        self
    }

    /// `true` when a skill registry is installed and the Dreaming phase will
    /// run self-improvement reflection.
    #[cfg(feature = "std")]
    pub fn skill_reflection_enabled(&self) -> bool {
        self.skill_registry.is_some()
    }

    /// Returns a clone of the shared skill-registry handle, if installed.
    ///
    /// The hosted kernel uses this after a sleep cycle to drain the registry's
    /// pending agent-authored proposals into the E15 approval queue (via
    /// `lifecycle::SkillApprovalBridge`), keeping the `vita → lifecycle` edge
    /// absent and the dependency graph acyclic.
    #[cfg(feature = "std")]
    pub fn skill_registry_handle(&self) -> Option<Arc<Mutex<SkillRegistry>>> {
        self.skill_registry.clone()
    }

    /// Buffers one episode summary for the next Dreaming-phase reflection pass
    /// (E11 S11.5).  No-op semantics until a skill registry is installed (the
    /// summaries are still buffered, but reflection only runs when a registry
    /// is present).
    #[cfg(feature = "std")]
    pub fn record_episode_summary(&mut self, summary: EpisodeSummary) {
        self.recent_episode_summaries.push(summary);
    }

    /// Run the E11 Dreaming-phase self-improvement reflection (S11.5).
    ///
    /// When a [`SkillRegistry`] is installed, reflects over the buffered
    /// [`recent_episode_summaries`](Self::recent_episode_summaries) via
    /// [`sleep::run_self_improvement_reflection`], drafting and registering
    /// agent-authored `Proposed` skills and emitting the existing
    /// `SkillReflectionCompleted` and `SkillRegistered` audit entries.  Returns
    /// the resulting [`ReflectionRegistration`].
    ///
    /// When no registry is installed this is a no-op returning the default
    /// (empty) registration, so the Dreaming phase's existing behaviour is
    /// untouched.
    #[cfg(feature = "std")]
    fn run_dreaming_reflection(&mut self) -> ReflectionRegistration {
        let Some(registry) = self.skill_registry.clone() else {
            return ReflectionRegistration::default();
        };
        if self.recent_episode_summaries.is_empty() {
            return ReflectionRegistration::default();
        }
        let proposed_at_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let agent_id = self.agent_id.clone();
        let mut guard = registry.lock().expect("skill_registry poisoned");
        sleep::run_self_improvement_reflection(
            &agent_id,
            &self.recent_episode_summaries,
            &self.reflection_config,
            &self.promotion_gate_config,
            &mut guard,
            &mut self.audit,
            proposed_at_ns,
        )
    }

    /// Returns a clone of the cancellation handle for the dispatch currently
    /// in flight (or the most recently installed one between dispatches).
    /// Clones share state with the live token, so tripping the clone cancels
    /// the running backend stream.
    pub fn current_cancel_handle(&self) -> CancellationToken {
        self.task_cancel.lock().expect("poisoned").clone()
    }

    /// Trips the cancellation token for the dispatch currently in flight.
    /// Safe to call from any thread that holds a reference to the manager.
    pub fn cancel_current_task(&self) {
        self.task_cancel.lock().expect("poisoned").cancel();
    }

    /// Installs a fresh [`CancellationToken`] and returns a clone bound to it.
    /// Called by [`somatic_execution_loop`] immediately before each dispatch
    /// so previous cancellation requests do not affect the next task.
    fn install_fresh_cancel(&self) -> CancellationToken {
        let fresh = CancellationToken::new();
        let handle = fresh.clone();
        *self.task_cancel.lock().expect("poisoned") = fresh;
        handle
    }

    /// Applies new human policy bounds.
    pub fn update_policy_bounds(&mut self, guidance: HumanGuidance) {
        self.policy_bounds = guidance;
    }

    /// Transitions into the sleep state, emitting a [`AuditEntry::SleepEntered`]
    /// entry followed by audited maintenance-phase entries for each of the four
    /// sleep routines (E3.4 exit criterion 1).
    ///
    /// The `MemoryPruning` phase now runs *real* L1 emotional-decay pruning via
    /// [`L1PruningStore::run_pruning_pass_with`] using `self.pruning_elapsed`
    /// as the elapsed time (E3.5).
    ///
    /// The `DreamExploration` phase runs real random-walk exploration when an
    /// L3 archive is configured (E3.7).
    ///
    /// The `PolicyCompilation` phase compiles audit traces into training datasets
    /// when a `compilation_config` is set (E3.8).
    ///
    /// Calling this method when already in the sleep state is a no-op to
    /// prevent duplicate transitions from the somatic loop.
    pub async fn transition_to_sleep_state(&mut self) -> Result<(), LifecycleError> {
        if self.state != LifecycleState::Sleep {
            self.state = LifecycleState::Sleep;
            self.audit.push(AuditEntry::SleepEntered {
                agent_id: self.agent_id.clone(),
            });
            let agent_id = self.agent_id.clone();
            let elapsed = self.pruning_elapsed;

            // Replay context (E3.6): immutable borrow of l3_archive.
            let replay_ctx = self.l3_archive.as_ref().map(|l3| sleep::ReplayContext {
                l3,
                config: self.replay_config.clone(),
            });

            // Dream context (E3.7): immutable borrow of l3_archive.
            let dream_ctx = self.l3_archive.as_ref().map(|l3| DreamContext {
                l3,
                config: self.dream_config.clone(),
            });

            // Compilation context (E3.8): convert audit entries to trace entries.
            // We collect them into an owned Vec so that the CompilationContext borrow
            // lives long enough for the duration of run_maintenance_audited.
            let trace_entries: Vec<AuditTraceEntry> = self
                .audit
                .entries()
                .iter()
                .map(audit_entry_to_trace)
                .collect();
            let compilation_ctx = self
                .compilation_config
                .as_ref()
                .map(|cfg| CompilationContext {
                    entries: &trace_entries,
                    config: cfg.clone(),
                });

            // Pruning context (E3.5): mutable borrow of l1_memory — compatible
            // because all other borrows above target different fields (l3_archive).
            let ctx = PruningContext {
                l1: &mut self.l1_memory,
                elapsed,
                floor: None,
            };
            let report = sleep::run_maintenance_audited(
                &agent_id,
                &mut self.audit,
                Some(ctx),
                replay_ctx,
                dream_ctx,
                compilation_ctx,
            );

            // E2.6: Demote evicted L1 nodes to L3 archive if present.
            if let Some(l3) = self.l3_archive.as_mut() {
                if let Some(outcome) = report.outcomes.first() {
                    for (key, node) in &outcome.evicted_l1_nodes {
                        let id = self.next_archive_id;
                        self.next_archive_id += 1;
                        let item = memory::archive_memory_node(id, key, node);
                        let prov = memory::Provenance::now(memory::SourceTier::L1, key.as_str());
                        let _ = l3.demote(item, prov);
                    }
                }
            }

            // E3.6: Re-insert rollback nodes from GenerativeReplay into L1.
            if let Some(replay_outcome) = report.outcomes.get(1) {
                for (key, node) in &replay_outcome.replay_rollback_nodes {
                    self.l1_memory.insert(key.clone(), node.clone());
                }
            }

            // E11 S11.5: Dreaming-phase self-improvement reflection.  No-op
            // unless a skill registry is installed and episodes are buffered.
            #[cfg(feature = "std")]
            let _ = self.run_dreaming_reflection();
        }
        Ok(())
    }

    /// Transitions back into the active waking state, emitting a
    /// [`AuditEntry::WakeEntered`] entry.
    pub fn transition_to_waking_state(&mut self) {
        if self.state != LifecycleState::Awake {
            self.state = LifecycleState::Awake;
            self.audit.push(AuditEntry::WakeEntered {
                agent_id: self.agent_id.clone(),
            });
        }
    }

    /// Explicitly runs one complete sleep-maintenance cycle (all four phases)
    /// with full audit logging.
    ///
    /// Unlike [`transition_to_sleep_state`], this method always runs the cycle
    /// regardless of the current state — useful for on-demand maintenance and
    /// the E3.4 soak test.
    ///
    /// The `MemoryPruning` phase runs real L1 emotional-decay pruning (E3.5).
    /// Evicted L1 nodes are demoted to the L3 archive when one is configured (E2.6).
    /// The `DreamExploration` phase runs real random-walk exploration when an
    /// L3 archive is configured (E3.7).
    /// The `PolicyCompilation` phase compiles audit traces into training pairs
    /// when a `compilation_config` is set (E3.8).
    pub fn run_sleep_cycle(&mut self) -> SleepMaintenanceReport {
        let agent_id = self.agent_id.clone();
        let elapsed = self.pruning_elapsed;

        // Build replay context (E3.6) and dream context (E3.7): immutable borrows
        // of l3_archive — compatible with mutable borrow of l1_memory below since
        // they target different fields.
        let replay_ctx = self.l3_archive.as_ref().map(|l3| ReplayContext {
            l3,
            config: self.replay_config.clone(),
        });

        let dream_ctx = self.l3_archive.as_ref().map(|l3| DreamContext {
            l3,
            config: self.dream_config.clone(),
        });

        // Compilation context (E3.8).
        let trace_entries: Vec<AuditTraceEntry> = self
            .audit
            .entries()
            .iter()
            .map(audit_entry_to_trace)
            .collect();
        let compilation_ctx = self
            .compilation_config
            .as_ref()
            .map(|cfg| CompilationContext {
                entries: &trace_entries,
                config: cfg.clone(),
            });

        let ctx = PruningContext {
            l1: &mut self.l1_memory,
            elapsed,
            floor: None,
        };
        let report = sleep::run_maintenance_audited(
            &agent_id,
            &mut self.audit,
            Some(ctx),
            replay_ctx,
            dream_ctx,
            compilation_ctx,
        );

        // E2.6: Demote evicted L1 nodes to L3 archive if present.
        if let Some(l3) = self.l3_archive.as_mut() {
            if let Some(outcome) = report.outcomes.first() {
                for (key, node) in &outcome.evicted_l1_nodes {
                    let id = self.next_archive_id;
                    self.next_archive_id += 1;
                    let item = memory::archive_memory_node(id, key, node);
                    let prov = memory::Provenance::now(memory::SourceTier::L1, key.as_str());
                    let _ = l3.demote(item, prov);
                }
            }
        }

        // E3.6: Re-insert rollback nodes from the GenerativeReplay phase into L1.
        if let Some(replay_outcome) = report.outcomes.get(1) {
            for (key, node) in &replay_outcome.replay_rollback_nodes {
                self.l1_memory.insert(key.clone(), node.clone());
            }
        }

        // E11 S11.5: Dreaming-phase self-improvement reflection.  No-op unless a
        // skill registry is installed and episodes are buffered.
        #[cfg(feature = "std")]
        let _ = self.run_dreaming_reflection();

        report
    }

    fn should_stop(&self) -> bool {
        self.max_iterations
            .map(|limit| self.iterations >= limit)
            .unwrap_or(false)
    }

    /// Resolve the LLM backend used to dispatch a task (E9 S9.5).
    ///
    /// When the per-tier backend map is **absent**, returns a clone of the
    /// single [`LifecycleManager::backend`] — identical to the legacy path.
    ///
    /// When the map is **present**, derives the gate [`CostClass`] for the task's
    /// MLFQ priority tier (via [`cost_class_for_mlfq_tier`]), maps it onto the
    /// router's [`ModelSelector`], selects the backend bound to that tier, and
    /// pushes a [`AuditEntry::RouterDecision`] recording the selected tier and
    /// backend so the routing is permanently traceable (reusing the existing
    /// E5.3 router-decision audit variant — no new audit variants added).
    #[cfg(feature = "std")]
    fn resolve_dispatch_backend(&mut self, task_id: u64, mlfq_tier: u8) -> Arc<dyn LlmBackend> {
        match &self.tier_backends {
            None => Arc::clone(&self.backend),
            Some(tiers) => {
                let cost_class = cost_class_for_mlfq_tier(mlfq_tier);
                let selector = router::model_selector_for_cost_class(cost_class);
                let backend = Arc::clone(tiers.backend_for(selector));
                // Reuse the E5.3 RouterDecision audit entry to record the
                // per-tier backend selection (model_selector carries the tier;
                // tool_scope_name carries the chosen backend id for traceability).
                self.audit.push(AuditEntry::RouterDecision {
                    agent_id: self.agent_id.clone(),
                    event_id: format!("dispatch-{task_id}"),
                    route_id: selector.as_str().to_string(),
                    model_selector: selector.as_str().to_string(),
                    tool_scope_name: backend.id().to_string(),
                    tools_available: 0,
                    tools_permitted: 0,
                    memory_scope_identity: true,
                    memory_scope_l1: true,
                    memory_scope_l2: !matches!(selector, ModelSelector::CheapLocal),
                    memory_scope_l3: matches!(selector, ModelSelector::Frontier),
                    max_turns: 0,
                    max_tool_calls: 0,
                });
                backend
            }
        }
    }
}

/// Map an MLFQ priority tier onto a gate [`CostClass`] for per-tier dispatch
/// (E9 S9.5).
///
/// The somatic loop's normal task path does not run the full Striatal Gate
/// (only operator-forced packets are gated), so the per-tier backend map keys
/// off the task's MLFQ priority instead — a deterministic, audit-friendly proxy
/// for the cost tier:
///
/// | MLFQ tier | Priority origin            | Cost class   |
/// |-----------|----------------------------|--------------|
/// | `0`       | Critical / High / forced   | `Frontier`   |
/// | `1`       | Normal interaction         | `MidTier`    |
/// | `≥ 2`     | Low / background           | `CheapLocal` |
///
/// This mirrors [`priority_to_mlfq_tier`] in reverse so the highest-priority
/// work reaches the most capable bound backend.
pub fn cost_class_for_mlfq_tier(mlfq_tier: u8) -> CostClass {
    match mlfq_tier {
        0 => CostClass::Frontier,
        1 => CostClass::MidTier,
        _ => CostClass::CheapLocal,
    }
}

// ── AuditEntry → AuditTraceEntry conversion ────────────────────────────────────

/// Converts a vita [`AuditEntry`] into an [`AuditTraceEntry`] for the
/// policy-compilation phase.
///
/// Only `TaskStarted`, `TaskCompleted`, and `TaskFailed` carry task-level
/// information; all other variants map to [`AuditTraceEntry::Other`].
fn audit_entry_to_trace(entry: &AuditEntry) -> AuditTraceEntry {
    match entry {
        AuditEntry::TaskStarted {
            task_id,
            tier,
            prompt,
            ..
        } => AuditTraceEntry::TaskStarted {
            task_id: *task_id,
            tier: *tier,
            prompt: prompt.clone(),
        },
        AuditEntry::TaskCompleted {
            task_id,
            tokens_emitted,
            response,
            ..
        } => AuditTraceEntry::TaskCompleted {
            task_id: *task_id,
            tokens_emitted: *tokens_emitted,
            response: response.clone(),
        },
        AuditEntry::TaskFailed { task_id, error, .. } => AuditTraceEntry::TaskFailed {
            task_id: *task_id,
            error: error.clone(),
        },
        _ => AuditTraceEntry::Other,
    }
}

// ── Sensory-priority → MLFQ-tier mapping ─────────────────────────────────────

/// Maps a [`SensoryPriority`] to the numeric MLFQ tier used by [`Task::new`].
///
/// | Priority | MLFQ tier | Notes                          |
/// |----------|-----------|-------------------------------|
/// | Critical | 0         | Highest — interrupt-level      |
/// | High     | 0         | Highest — operator urgency     |
/// | Normal   | 1         | Medium — standard interaction  |
/// | Low      | 2         | Lowest — background work       |
fn priority_to_mlfq_tier(priority: SensoryPriority) -> u8 {
    match priority {
        SensoryPriority::Critical | SensoryPriority::High => 0,
        SensoryPriority::Normal => 1,
        SensoryPriority::Low => 2,
    }
}

/// Autonomous lifecycle control loop.
///
/// Each iteration:
/// 1. **Starvation boost** — promotes waiting Medium/Low tasks to High if the
///    boost interval has elapsed.
/// 2. **Sensory ingestion** — drains all pending priority-tagged packets from
///    the senses bridge and enqueues them as MLFQ tasks (E3.3 exit criterion 1:
///    text and voice reach `vita` as priority-tagged packets).
/// 3. **Policy update** — refreshes the active policy bounds from the bridge.
/// 4. **Stress check** — computes the homeostatic stress index via the
///    interoception monitor.
/// 5. **Sleep / wake decision** — the agent sleeps when the agenda is empty
///    (normal or emergency maintenance); it wakes when the agenda is non-empty,
///    which happens automatically when sensory packets are converted to tasks in
///    step 2 (E3.4 exit criterion: wake on sensory event).
/// 6. **Task dispatch** — selects and dispatches the optimal task through the
///    LLM backend, updating working memory and recording the outcome.
pub async fn somatic_execution_loop(
    lifecycle: &mut LifecycleManager,
    monitor: &HomeostaticMonitor,
) -> Result<(), LifecycleError> {
    loop {
        // ── 1. Starvation-prevention boost ───────────────────────────────────
        lifecycle.scheduler.check_and_boost(&mut lifecycle.agenda);

        // ── 2. Sensory ingestion (E3.3) ───────────────────────────────────────
        // Drain all pending priority-tagged packets and promote them to agenda
        // tasks.  This also serves as the sensory-wake trigger for E3.4:
        // if packets arrive while the agent is sleeping, they are converted to
        // tasks here, and the subsequent agenda check transitions back to Awake.
        while let Some(pkt) = lifecycle.senses.next_prioritized_packet() {
            use senses::SensoryPacket;
            let tier = priority_to_mlfq_tier(pkt.priority);
            let prompt = match &pkt.packet {
                SensoryPacket::Text(t) => t.clone(),
                SensoryPacket::Pcm(samples) => format!("[PCM {} samples]", samples.len()),
                SensoryPacket::Image {
                    mime,
                    bytes,
                    caption,
                } => format!(
                    "[Image {} B {mime}{}]",
                    bytes.len(),
                    caption
                        .as_deref()
                        .map(|c| format!(" caption={c:?}"))
                        .unwrap_or_default()
                ),
            };

            // E6.6: operator-force override — record an audited gate decision
            // before admitting the task.  The override always forces invoke=true
            // at Frontier cost class; neutral homeostatic signals are used because
            // the somatic loop hasn't yet sampled the sensor bundle for this tick.
            let task_id = lifecycle.next_sensory_task_id;
            lifecycle.next_sensory_task_id = lifecycle.next_sensory_task_id.wrapping_add(1);

            if let Some(reason) = pkt.gate_override_reason.as_deref() {
                let event = EventFeatures {
                    urgency: 1.0,
                    novelty: 0.5,
                    semantic_class: SemanticClass::OperatorCommand,
                    user_facing: true,
                };
                let signals = HomeostaticSignals::neutral();
                let override_hint = GateOverride::OperatorForced {
                    reason: reason.to_owned(),
                };
                let event_id = format!("sensory-{task_id}");

                // E12: when the motivated gate is installed, route the decision
                // through the drive hierarchy and emit drive/affect audit
                // entries alongside the gate decision.  The operator-force
                // override semantics are preserved exactly (invoke at Frontier).
                // When absent, this is byte-for-byte the original code path.
                #[cfg(feature = "std")]
                let motivated = lifecycle.motivated_gate.clone();
                #[cfg(not(feature = "std"))]
                let motivated: Option<()> = None;

                let decision = if let Some(mg) = motivated {
                    #[cfg(feature = "std")]
                    {
                        let guard = mg.lock().expect("motivated_gate poisoned");
                        let (decision, augmented, affect) =
                            guard.decide_motivated(&event_id, &event, &signals, &override_hint);
                        let snapshot = guard.drive_snapshot();
                        drop(guard);

                        let u = &snapshot.urgencies;
                        lifecycle.audit.push(AuditEntry::DriveStateSnapshot {
                            agent_id: lifecycle.agent_id.clone(),
                            viability_urgency: u[motivation::DriveTier::Viability.index()],
                            integrity_urgency: u[motivation::DriveTier::Integrity.index()],
                            service_urgency: u[motivation::DriveTier::Service.index()],
                            epistemic_urgency: u[motivation::DriveTier::Epistemic.index()],
                            achievement_urgency: u[motivation::DriveTier::Achievement.index()],
                            self_actualisation_urgency: u
                                [motivation::DriveTier::SelfActualisation.index()],
                            drive_delta: augmented.drive_delta,
                            lattice_suppression_active: augmented.lattice_suppression_active,
                        });
                        lifecycle.audit.push(AuditEntry::AffectStateSnapshot {
                            agent_id: lifecycle.agent_id.clone(),
                            valence: affect.valence,
                            arousal: affect.arousal,
                            gate_threshold_nudge: affect.gate_threshold_nudge(),
                        });
                        decision
                    }
                    #[cfg(not(feature = "std"))]
                    {
                        unreachable!("motivated_gate is std-only")
                    }
                } else {
                    let gate = ThresholdGate::with_defaults();
                    gate.decide(&event_id, &event, &signals, &override_hint)
                };

                record_gate_decision(
                    &mut lifecycle.audit,
                    &lifecycle.agent_id,
                    &decision,
                    &event,
                    &signals,
                );
                if !decision.invoke {
                    continue;
                }
            }

            lifecycle.agenda.push(Task::new(task_id, tier, prompt));
        }

        // ── 3. Policy update ──────────────────────────────────────────────────
        let human_guidance = lifecycle.senses.read_active_bounds()?;
        lifecycle.update_policy_bounds(human_guidance);

        // ── 4. Stress index + memory pressure ────────────────────────────────
        let active_tokens = lifecycle.memory.get_l1_token_count();
        let _stress_index =
            monitor.compute_systemic_stress_index(active_tokens, lifecycle.config.max_context);

        // Publish interoceptive snapshot to the audit log on every iteration
        // when a sensor bundle is configured (EX.2 wiring, E5.7 S5.7.1).
        #[cfg(feature = "std")]
        if let Some(ref bundle) = lifecycle.sensor_bundle {
            let now_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            let signals = bundle.tick(
                monitor,
                active_tokens,
                lifecycle.config.max_context,
                now_ns,
                &NullPublisher,
            );
            lifecycle.audit.push(AuditEntry::InteroceptiveSnapshot {
                agent_id: lifecycle.agent_id.clone(),
                tick_ns: now_ns,
                thermal_load: signals.thermal_load,
                compute_pressure: signals.compute_pressure,
                memory_pressure: signals.memory_pressure,
                power_budget: signals.power_budget,
                financial_budget: signals.financial_budget,
                attention_demand: signals.attention_demand,
                aggregate_stress: signals.aggregate_stress(),
            });

            // E12: refresh the motivated gate's drive registry from this tick's
            // real interoceptive reading so drive urgencies (and thus the
            // augmented value score) track the live homeostatic state at ~1 Hz.
            if let Some(ref mg) = lifecycle.motivated_gate {
                let h = HomeostaticSignals::from_interoceptive(&signals);
                mg.lock()
                    .expect("motivated_gate poisoned")
                    .update_signals(&h);
            }
        }

        let pressure = lifecycle.memory.check_pressure();
        if pressure != lifecycle.last_pressure_level {
            lifecycle.last_pressure_level = pressure;
            // Log every level transition including the return to Normal so that
            // audit-log consumers can see the full pressure envelope over time.
            let agent_id = lifecycle.agent_id.clone();
            let max_context = lifecycle.config.max_context;
            #[cfg(feature = "std")]
            let level = format!("{pressure:?}");
            #[cfg(not(feature = "std"))]
            let level = alloc::format!("{pressure:?}");
            lifecycle.audit.push(AuditEntry::MemoryPressureEvent {
                agent_id,
                level,
                active_tokens,
                max_context,
            });
        }

        // ── 5. Sleep / wake decision (E3.4) ──────────────────────────────────
        // Sleep whenever the agenda is empty; wake when a task is available.
        // Sensory packets converted in step 2 re-populate the agenda, providing
        // the implicit "wake on sensory event" trigger.
        let is_idle = if lifecycle.agenda.is_empty() {
            lifecycle.transition_to_sleep_state().await?;
            true
        } else if let Some(task) = lifecycle.agenda.select_optimal_task() {
            // ── 6. Task dispatch ──────────────────────────────────────────────
            if lifecycle.state == LifecycleState::Sleep {
                lifecycle.transition_to_waking_state();
            }
            let agent_id = lifecycle.agent_id.clone();
            let task_id = task.id;
            let tier = task.mlfq_level;
            let prompt = task.prompt.clone();

            lifecycle.audit.push(AuditEntry::TaskStarted {
                agent_id: agent_id.clone(),
                task_id,
                tier,
                prompt,
            });

            let cancel = lifecycle.install_fresh_cancel();
            // E9 S9.5: when a per-tier backend map is installed, resolve the
            // dispatch backend for this task's cost-class tier and record the
            // routing decision; otherwise fall back to the single backend so
            // the default path is unchanged.
            #[cfg(feature = "std")]
            let backend = lifecycle.resolve_dispatch_backend(task_id, tier);
            #[cfg(not(feature = "std"))]
            let backend = Arc::clone(&lifecycle.backend);
            let dispatch_result = lifecycle
                .scheduler
                .dispatch_task(task, &*backend, &cancel)
                .await;

            match dispatch_result {
                Ok(outcome) => {
                    lifecycle.memory.add_tokens(outcome.tokens_emitted);
                    lifecycle.audit.push(AuditEntry::TaskCompleted {
                        agent_id,
                        task_id,
                        tokens_emitted: outcome.tokens_emitted,
                        response: outcome.response,
                    });
                }
                Err(error) => {
                    lifecycle.audit.push(AuditEntry::TaskFailed {
                        agent_id,
                        task_id,
                        error: format!("{error:?}"),
                    });
                }
            }
            false
        } else {
            true
        };

        if is_idle {
            std::thread::sleep(Duration::from_millis(1));
        }

        lifecycle.iterations = lifecycle.iterations.saturating_add(1);
        if lifecycle.should_stop() {
            return Ok(());
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use interoception::HomeostaticMonitor;
    use scheduler::{MockLlmBackend, Task};
    use senses::SensoryPriority;
    use std::future::Future;
    use std::task::{Context, Poll, Waker};
    // E3.5 test helpers — explicit re-imports from the memory crate.

    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut future = Box::pin(future);
        loop {
            match future.as_mut().poll(&mut cx) {
                Poll::Ready(value) => return value,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    fn manager(agent_id: &str, max_iterations: Option<u32>) -> LifecycleManager {
        LifecycleManager::new(
            agent_id,
            SensoryBridge::new(HumanGuidance::new("test")),
            VirtualContextManager::with_capacity(0, 1000),
            LifecycleConfig { max_context: 1000 },
            HumanGuidance::new("initial"),
            Arc::new(MockLlmBackend::new()),
            max_iterations,
        )
    }

    // ── Existing tests (backward-compat) ──────────────────────────────────────

    #[test]
    fn lifecycle_enters_sleep_when_idle_and_low_stress() {
        let mut m = manager("agent-a", Some(1));
        let mut monitor = HomeostaticMonitor::new(1.0, 0.5, 16);
        monitor.record_ttft(0.0);

        block_on(somatic_execution_loop(&mut m, &monitor)).unwrap();

        assert_eq!(m.state, LifecycleState::Sleep);
        assert!(m
            .audit
            .entries()
            .iter()
            .any(|e| matches!(e, AuditEntry::SleepEntered { .. })));
    }

    #[test]
    fn lifecycle_dispatches_available_tasks_and_records_audit() {
        let mut m = manager("agent-b", Some(1));
        m.agenda.push(Task::new(42, 1, "hello mock backend"));

        let mut monitor = HomeostaticMonitor::new(1.0, 0.5, 16);
        monitor.record_ttft(2.0);

        block_on(somatic_execution_loop(&mut m, &monitor)).unwrap();

        assert_eq!(m.state, LifecycleState::Awake);
        assert_eq!(m.scheduler.dispatched_tasks.len(), 1);
        assert_eq!(m.scheduler.dispatched_tasks[0].id, 42);

        let entries = m.audit.entries();
        assert!(matches!(
            entries.first(),
            Some(AuditEntry::TaskStarted { task_id: 42, .. })
        ));
        let completed = entries
            .iter()
            .find_map(|e| match e {
                AuditEntry::TaskCompleted {
                    task_id,
                    tokens_emitted,
                    response,
                    ..
                } if *task_id == 42 => Some((*tokens_emitted, response.clone())),
                _ => None,
            })
            .expect("expected TaskCompleted entry");
        assert_eq!(completed.0, 3);
        assert_eq!(completed.1, "hello mock backend ");
    }

    #[test]
    fn cancel_current_task_trips_handle_observed_externally() {
        let m = manager("agent-c", None);

        let pre = m.current_cancel_handle();
        assert!(!pre.is_cancelled());

        let installed = m.install_fresh_cancel();
        let observed = m.current_cancel_handle();
        assert!(!installed.is_cancelled());
        assert!(!observed.is_cancelled());

        m.cancel_current_task();
        assert!(installed.is_cancelled());
        assert!(observed.is_cancelled());
        assert!(!pre.is_cancelled());
    }

    // ── E3.3 — Sensory Bridge integration ────────────────────────────────────

    #[test]
    fn text_packet_reaches_vita_and_is_dispatched_as_high_priority_task() {
        // Inject a High-priority text packet before running the loop.
        let mut m = manager("agent-sensory", Some(2));
        m.senses
            .packetize_text_checked("urgent operator query", SensoryPriority::High)
            .expect("valid text should be accepted");

        let monitor = HomeostaticMonitor::new(1.0, 0.5, 16);

        block_on(somatic_execution_loop(&mut m, &monitor)).unwrap();

        // The sensory packet should have been converted to a task and dispatched.
        assert_eq!(
            m.scheduler.dispatched_tasks.len(),
            1,
            "one sensory-derived task should have been dispatched"
        );
        // A High-priority sensory packet maps to MLFQ tier 0 (High).
        assert_eq!(
            m.scheduler.dispatched_tasks[0].mlfq_level, 0,
            "high-priority sensory task must be in MLFQ tier 0"
        );
    }

    #[test]
    fn voice_pcm_packet_reaches_vita_and_is_dispatched_as_task() {
        let mut m = manager("agent-pcm", Some(2));
        m.senses
            .packetize_pcm_checked(vec![100i16; 160], SensoryPriority::Normal)
            .expect("valid PCM frame should be accepted");

        let monitor = HomeostaticMonitor::new(1.0, 0.5, 16);

        block_on(somatic_execution_loop(&mut m, &monitor)).unwrap();

        assert_eq!(
            m.scheduler.dispatched_tasks.len(),
            1,
            "PCM sensory packet should produce one dispatched task"
        );
    }

    #[test]
    fn critical_sensory_packet_maps_to_mlfq_tier_zero() {
        let mut m = manager("agent-critical", Some(2));
        m.senses
            .packetize_text_checked("CRITICAL override", SensoryPriority::Critical)
            .expect("valid text");

        let monitor = HomeostaticMonitor::new(1.0, 0.5, 16);
        block_on(somatic_execution_loop(&mut m, &monitor)).unwrap();

        assert_eq!(m.scheduler.dispatched_tasks[0].mlfq_level, 0);
    }

    #[test]
    fn low_priority_sensory_packet_maps_to_mlfq_tier_two() {
        let mut m = manager("agent-low", Some(2));
        m.senses
            .packetize_text_checked("background note", SensoryPriority::Low)
            .expect("valid text");

        let monitor = HomeostaticMonitor::new(1.0, 0.5, 16);
        block_on(somatic_execution_loop(&mut m, &monitor)).unwrap();

        assert_eq!(m.scheduler.dispatched_tasks[0].mlfq_level, 2);
    }

    #[test]
    fn forced_operator_packet_records_gate_decision_with_override_active_true() {
        // E6.6: a packet submitted via packetize_text_forced must cause vita's
        // somatic loop to emit a GateDecision audit entry with override_active=true.
        let mut m = manager("agent-forced", Some(2));
        m.senses
            .packetize_text_forced("deploy immediately", "on-call escalation")
            .expect("valid forced text");

        let monitor = HomeostaticMonitor::new(1.0, 0.5, 16);
        block_on(somatic_execution_loop(&mut m, &monitor)).unwrap();

        // The task should have been dispatched at Critical (tier 0).
        assert_eq!(
            m.scheduler.dispatched_tasks.len(),
            1,
            "forced packet should produce one dispatched task"
        );
        assert_eq!(m.scheduler.dispatched_tasks[0].mlfq_level, 0);

        // An audited GateDecision entry with override_active=true must be present.
        let gate_entry = m.audit.entries().iter().find(|e| {
            matches!(
                e,
                AuditEntry::GateDecision {
                    override_active: true,
                    ..
                }
            )
        });
        assert!(
            gate_entry.is_some(),
            "audit log must contain a GateDecision with override_active=true; entries: {:?}",
            m.audit.entries()
        );

        // The reasoning should mention the operator-force override.
        if let Some(AuditEntry::GateDecision { reasoning, .. }) = gate_entry {
            assert!(
                reasoning.contains("operator-forced override"),
                "reasoning should reference the operator-forced override; got: {reasoning}"
            );
        }
    }

    // ── E12 — MotivatedGate wired into LifecycleManager ──────────────────────

    #[test]
    fn enabling_motivation_emits_drive_and_affect_audit_entries() {
        // With the motivated gate installed, a forced operator packet routes
        // through the drive hierarchy and emits DriveStateSnapshot +
        // AffectStateSnapshot alongside the GateDecision.
        use crate::motivation_gate::MotivatedGate;

        let mut m = manager("agent-motivated", Some(2));
        m.enable_motivation(MotivatedGate::with_defaults(&HomeostaticSignals::neutral()));
        assert!(m.motivation_enabled());
        m.senses
            .packetize_text_forced("deploy now", "on-call escalation")
            .expect("valid forced text");

        let monitor = HomeostaticMonitor::new(1.0, 0.5, 16);
        block_on(somatic_execution_loop(&mut m, &monitor)).unwrap();

        // Operator-force semantics preserved: dispatched at tier 0, gate
        // decision present with override_active=true and Frontier reasoning.
        assert_eq!(m.scheduler.dispatched_tasks.len(), 1);
        assert_eq!(m.scheduler.dispatched_tasks[0].mlfq_level, 0);

        let entries = m.audit.entries();
        let has_drive = entries
            .iter()
            .any(|e| matches!(e, AuditEntry::DriveStateSnapshot { .. }));
        let has_affect = entries
            .iter()
            .any(|e| matches!(e, AuditEntry::AffectStateSnapshot { .. }));
        let gate_override = entries.iter().any(|e| {
            matches!(
                e,
                AuditEntry::GateDecision {
                    override_active: true,
                    ..
                }
            )
        });
        assert!(
            has_drive,
            "motivated loop must emit a DriveStateSnapshot entry"
        );
        assert!(
            has_affect,
            "motivated loop must emit an AffectStateSnapshot entry"
        );
        assert!(
            gate_override,
            "operator override must still produce a GateDecision with override_active=true"
        );

        // The affect nudge recorded must respect the documented [0.9, 1.1] band.
        for e in entries {
            if let AuditEntry::AffectStateSnapshot {
                gate_threshold_nudge,
                ..
            } = e
            {
                assert!(
                    (0.9..=1.1).contains(gate_threshold_nudge),
                    "recorded affect nudge {gate_threshold_nudge} out of [0.9, 1.1]"
                );
            }
        }
    }

    #[test]
    fn motivation_disabled_by_default_emits_no_drive_entries() {
        // Default manager has no motivated gate; a forced packet must produce
        // the original GateDecision and NO drive/affect entries (byte-for-byte
        // unchanged behaviour).
        let mut m = manager("agent-no-motivation", Some(2));
        assert!(!m.motivation_enabled());
        m.senses
            .packetize_text_forced("deploy now", "on-call escalation")
            .expect("valid forced text");

        let monitor = HomeostaticMonitor::new(1.0, 0.5, 16);
        block_on(somatic_execution_loop(&mut m, &monitor)).unwrap();

        let entries = m.audit.entries();
        assert!(
            entries.iter().any(|e| matches!(
                e,
                AuditEntry::GateDecision {
                    override_active: true,
                    ..
                }
            )),
            "disabled path must still emit the operator-force GateDecision"
        );
        assert!(
            !entries
                .iter()
                .any(|e| matches!(e, AuditEntry::DriveStateSnapshot { .. })),
            "disabled path must NOT emit DriveStateSnapshot entries"
        );
        assert!(
            !entries
                .iter()
                .any(|e| matches!(e, AuditEntry::AffectStateSnapshot { .. })),
            "disabled path must NOT emit AffectStateSnapshot entries"
        );
    }

    #[test]
    fn with_motivation_builder_installs_gate() {
        use crate::motivation_gate::MotivatedGate;
        let m = manager("agent-builder", Some(1))
            .with_motivation(MotivatedGate::with_defaults(&HomeostaticSignals::neutral()));
        assert!(
            m.motivation_enabled(),
            "with_motivation builder must install the gate"
        );
    }

    // ── E9 S9.5 — Per-tier backend dispatch wired into LifecycleManager ───────

    /// A tier map with three distinguishable mock backends.
    fn distinct_tiers() -> crate::router::TierBackends {
        crate::router::TierBackends::new(
            Arc::new(MockLlmBackend::with_id("cheap")),
            Arc::new(MockLlmBackend::with_id("mid")),
            Arc::new(MockLlmBackend::with_id("frontier")),
        )
    }

    /// The backend id recorded by the most recent tier-dispatch RouterDecision
    /// (we stash the chosen backend id in `tool_scope_name`).
    fn dispatched_backend_id(m: &LifecycleManager) -> Option<String> {
        m.audit.entries().iter().rev().find_map(|e| match e {
            AuditEntry::RouterDecision {
                tool_scope_name, ..
            } => Some(tool_scope_name.clone()),
            _ => None,
        })
    }

    #[test]
    fn tier_dispatch_disabled_by_default() {
        let m = manager("agent-no-tiers", Some(1));
        assert!(!m.tier_dispatch_enabled());
    }

    #[test]
    fn with_tier_backends_builder_installs_map() {
        let m = manager("agent-tier-builder", Some(1)).with_tier_backends(distinct_tiers());
        assert!(m.tier_dispatch_enabled());
    }

    #[test]
    fn frontier_tier_task_dispatches_to_frontier_backend() {
        // MLFQ tier 0 (Critical/High) → Frontier cost class → frontier backend.
        let mut m =
            manager("agent-frontier-dispatch", Some(1)).with_tier_backends(distinct_tiers());
        m.agenda.push(Task::new(1, 0, "high priority work"));

        let mut monitor = HomeostaticMonitor::new(1.0, 0.5, 16);
        monitor.record_ttft(1.0);
        block_on(somatic_execution_loop(&mut m, &monitor)).unwrap();

        assert_eq!(m.scheduler.dispatched_tasks.len(), 1);
        assert_eq!(
            dispatched_backend_id(&m).as_deref(),
            Some("frontier"),
            "a tier-0 (Frontier) task must dispatch to the frontier backend"
        );
    }

    #[test]
    fn cheap_local_tier_task_dispatches_to_cheap_backend() {
        // MLFQ tier 2 (Low/background) → CheapLocal cost class → cheap backend.
        let mut m = manager("agent-cheap-dispatch", Some(1)).with_tier_backends(distinct_tiers());
        m.agenda.push(Task::new(7, 2, "background chore"));

        let mut monitor = HomeostaticMonitor::new(1.0, 0.5, 16);
        monitor.record_ttft(1.0);
        block_on(somatic_execution_loop(&mut m, &monitor)).unwrap();

        assert_eq!(m.scheduler.dispatched_tasks.len(), 1);
        assert_eq!(
            dispatched_backend_id(&m).as_deref(),
            Some("cheap"),
            "a tier-2 (CheapLocal) task must dispatch to the cheap backend"
        );
    }

    #[test]
    fn mid_tier_task_dispatches_to_mid_backend() {
        let mut m = manager("agent-mid-dispatch", Some(1)).with_tier_backends(distinct_tiers());
        m.agenda.push(Task::new(3, 1, "normal interaction"));

        let mut monitor = HomeostaticMonitor::new(1.0, 0.5, 16);
        monitor.record_ttft(1.0);
        block_on(somatic_execution_loop(&mut m, &monitor)).unwrap();

        assert_eq!(
            dispatched_backend_id(&m).as_deref(),
            Some("mid"),
            "a tier-1 (MidTier) task must dispatch to the mid backend"
        );
    }

    #[test]
    fn uniform_tier_map_preserves_single_backend_behaviour() {
        // A uniform map routes every tier to one backend: behaviour through the
        // new path matches the legacy single-backend dispatch.
        let tiers = crate::router::TierBackends::uniform(Arc::new(MockLlmBackend::with_id("solo")));
        let mut m = manager("agent-uniform", Some(1)).with_tier_backends(tiers);
        m.agenda.push(Task::new(9, 0, "anything"));

        let mut monitor = HomeostaticMonitor::new(1.0, 0.5, 16);
        monitor.record_ttft(1.0);
        block_on(somatic_execution_loop(&mut m, &monitor)).unwrap();

        assert_eq!(m.scheduler.dispatched_tasks.len(), 1);
        assert_eq!(dispatched_backend_id(&m).as_deref(), Some("solo"));
    }

    #[test]
    fn no_tier_map_emits_no_router_decision_and_uses_single_backend() {
        // Backward compatibility: without a tier map the dispatch path is
        // unchanged — no RouterDecision entry, single backend used.
        let mut m = manager("agent-legacy-dispatch", Some(1));
        m.agenda.push(Task::new(11, 0, "legacy task"));

        let mut monitor = HomeostaticMonitor::new(1.0, 0.5, 16);
        monitor.record_ttft(1.0);
        block_on(somatic_execution_loop(&mut m, &monitor)).unwrap();

        assert_eq!(m.scheduler.dispatched_tasks.len(), 1);
        assert!(
            dispatched_backend_id(&m).is_none(),
            "no RouterDecision entry must be emitted when no tier map is installed"
        );
    }

    #[test]
    fn cost_class_for_mlfq_tier_maps_priorities() {
        assert_eq!(cost_class_for_mlfq_tier(0), CostClass::Frontier);
        assert_eq!(cost_class_for_mlfq_tier(1), CostClass::MidTier);
        assert_eq!(cost_class_for_mlfq_tier(2), CostClass::CheapLocal);
        assert_eq!(cost_class_for_mlfq_tier(5), CostClass::CheapLocal);
    }

    #[test]
    fn normal_packet_does_not_record_gate_decision_in_somatic_loop() {
        // Non-forced packets must NOT produce a GateDecision entry; the gate
        // is only consulted when an explicit operator-force override is present.
        let mut m = manager("agent-normal-no-gate", Some(2));
        m.senses
            .packetize_text_checked("routine query", SensoryPriority::Normal)
            .expect("valid text");

        let monitor = HomeostaticMonitor::new(1.0, 0.5, 16);
        block_on(somatic_execution_loop(&mut m, &monitor)).unwrap();

        let has_gate_entry = m
            .audit
            .entries()
            .iter()
            .any(|e| matches!(e, AuditEntry::GateDecision { .. }));
        assert!(
            !has_gate_entry,
            "a normal packet must not produce a GateDecision audit entry"
        );
    }

    #[test]
    fn forced_packets_across_two_loop_calls_produce_distinct_gate_event_ids() {
        // E6.6 fix: the next_sensory_task_id counter is persisted on
        // LifecycleManager so that gate event IDs remain unique when
        // somatic_execution_loop is called multiple times on the same manager.
        let monitor = HomeostaticMonitor::new(1.0, 0.5, 16);
        let mut m = manager("agent-unique-ids", Some(2));

        // First loop call: one forced packet.
        m.senses
            .packetize_text_forced("first command", "reason-one")
            .expect("valid");
        block_on(somatic_execution_loop(&mut m, &monitor)).unwrap();

        // Second loop call: another forced packet.
        m.max_iterations = Some(2);
        m.iterations = 0;
        m.senses
            .packetize_text_forced("second command", "reason-two")
            .expect("valid");
        block_on(somatic_execution_loop(&mut m, &monitor)).unwrap();

        // Collect all GateDecision event_ids from the audit log.
        let ids: Vec<String> = m
            .audit
            .entries()
            .iter()
            .filter_map(|e| {
                if let AuditEntry::GateDecision { event_id, .. } = e {
                    Some(event_id.clone())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            ids.len(),
            2,
            "expected two GateDecision entries; got: {ids:?}"
        );
        assert_ne!(
            ids[0], ids[1],
            "event IDs must be distinct across loop calls; both were {:?}",
            ids[0]
        );
    }

    #[test]
    fn sensory_event_during_sleep_triggers_wake_transition() {
        let monitor = HomeostaticMonitor::new(1.0, 0.5, 16);

        // Run 1 idle iteration so the agent enters sleep.
        let mut m1 = manager("agent-wake", Some(1));
        block_on(somatic_execution_loop(&mut m1, &monitor)).unwrap();
        assert_eq!(m1.state, LifecycleState::Sleep, "should enter sleep");

        // Inject a sensory packet and run one more iteration.
        // Use the unchecked method (no policy bounds required for test inputs).
        m1.senses.packetize_text("wake me up");
        m1.max_iterations = Some(2);
        block_on(somatic_execution_loop(&mut m1, &monitor)).unwrap();

        // The sensory packet should have been converted to a task and dispatched.
        assert_eq!(
            m1.scheduler.dispatched_tasks.len(),
            1,
            "sensory event during sleep should produce a dispatched task"
        );
        assert_eq!(
            m1.state,
            LifecycleState::Awake,
            "agent should be awake after dispatch"
        );
    }

    // ── E3.4 — Wake/Sleep transitions audited end-to-end ─────────────────────

    #[test]
    fn sleep_transition_audits_all_four_phases_in_order() {
        let mut m = manager("agent-sleep-audit", Some(1));
        let monitor = HomeostaticMonitor::new(1.0, 0.5, 16);

        block_on(somatic_execution_loop(&mut m, &monitor)).unwrap();

        assert_eq!(m.state, LifecycleState::Sleep);

        let entries = m.audit.entries();
        // Expect: SleepEntered + 4×(PhaseStarted + PhaseCompleted) = 9 entries.
        assert_eq!(entries.len(), 9, "should have 9 audit entries");

        assert!(matches!(entries[0], AuditEntry::SleepEntered { .. }));

        let phase_names = [
            "MemoryPruning",
            "GenerativeReplay",
            "DreamExploration",
            "PolicyCompilation",
        ];
        for (i, phase) in phase_names.iter().enumerate() {
            let start_idx = 1 + i * 2;
            let complete_idx = start_idx + 1;
            assert!(
                matches!(
                    &entries[start_idx],
                    AuditEntry::SleepPhaseStarted { phase: p, .. } if p == phase
                ),
                "entry {start_idx} should be SleepPhaseStarted for {phase}"
            );
            assert!(
                matches!(
                    &entries[complete_idx],
                    AuditEntry::SleepPhaseCompleted { phase: p, success: true, .. } if p == phase
                ),
                "entry {complete_idx} should be SleepPhaseCompleted for {phase}"
            );
        }
    }

    #[test]
    fn wake_transition_is_audited_after_sleep() {
        // Iteration 1: no tasks → sleep (audited).
        // Iteration 2: task arrives → wake (audited) → dispatch.
        let mut m = manager("agent-wake-audit", Some(2));
        m.agenda.push(Task::new(99, 0, "wake-up task"));

        let monitor = HomeostaticMonitor::new(1.0, 0.5, 16);

        // Pre-drain the agenda so first iteration sleeps.
        // Actually, with a task in the agenda, iteration 1 dispatches.
        // Use a different scenario: run 1 idle, then 1 with task.
        let mut m_idle = manager("agent-wake-audit", Some(1));
        block_on(somatic_execution_loop(&mut m_idle, &monitor)).unwrap();
        assert_eq!(m_idle.state, LifecycleState::Sleep);

        // Push a task and allow one more iteration.
        m_idle.agenda.push(Task::new(99, 0, "wake-up task"));
        m_idle.max_iterations = Some(2);
        block_on(somatic_execution_loop(&mut m_idle, &monitor)).unwrap();

        let entries = m_idle.audit.entries();
        let has_wake = entries
            .iter()
            .any(|e| matches!(e, AuditEntry::WakeEntered { .. }));
        assert!(has_wake, "WakeEntered must appear in the audit log");
    }

    /// E3.4 exit criterion 2: 100 consecutive sleep cycles complete without error.
    #[test]
    fn one_hundred_sleep_cycles_complete_without_error() {
        let mut m = manager("soak-agent", None);

        for cycle in 0..100 {
            let report = m.run_sleep_cycle();
            assert!(
                report.all_completed(),
                "sleep cycle {cycle} reported incomplete: {report:?}"
            );
        }

        // Verify: 100 cycles × 4 phases = 400 SleepPhaseCompleted entries,
        // all with success = true.
        let completed_count = m
            .audit
            .entries()
            .iter()
            .filter(|e| matches!(e, AuditEntry::SleepPhaseCompleted { success: true, .. }))
            .count();
        assert_eq!(
            completed_count, 400,
            "expected 400 successful SleepPhaseCompleted entries (100 cycles × 4 phases)"
        );

        // All started phases must have a paired completion.
        let started_count = m
            .audit
            .entries()
            .iter()
            .filter(|e| matches!(e, AuditEntry::SleepPhaseStarted { .. }))
            .count();
        assert_eq!(
            started_count, 400,
            "started count must equal completed count"
        );
    }

    // ── E3.5 — Pruning phase wired into LifecycleManager ─────────────────────

    /// E3.5: Inserting nodes into l1_memory and running a sleep cycle prunes
    /// decayed entries.
    #[test]
    fn sleep_cycle_prunes_decayed_l1_nodes() {
        let mut m = manager("prune-agent", None);
        // Fast-decaying node — will be below floor at elapsed=1.0.
        m.l1_memory
            .insert("fast", memory::MemoryNode::new(0.9, 20.0));
        // Stable node — never decays below initial activation.
        m.l1_memory
            .insert("stable", memory::MemoryNode::new(0.9, 0.0));

        assert_eq!(m.l1_memory.len(), 2, "both nodes before pruning");

        let report = m.run_sleep_cycle();
        assert!(report.all_completed());

        // MemoryPruning outcome should have a populated PruningReport.
        let pr = report.outcomes[0]
            .pruning
            .as_ref()
            .expect("MemoryPruning must carry a PruningReport when l1_memory is non-empty");

        assert_eq!(pr.nodes_before, 2);
        assert_eq!(pr.nodes_removed, 1, "fast-decaying node should be pruned");
        assert_eq!(m.l1_memory.len(), 1, "one node should remain after pruning");
        assert!(
            m.l1_memory.get("stable").is_some(),
            "stable node must survive"
        );
        assert!(
            m.l1_memory.get("fast").is_none(),
            "fast-decaying node must be evicted"
        );
    }

    /// E3.5 exit criterion 1: pruning bounded by the semantic floor under stress.
    #[test]
    fn lifecycle_pruning_bounded_by_floor_under_stress() {
        let mut m = manager("stress-agent", None);
        // High-arousal node with slow decay: activation stays well above floor.
        let mut stressed = memory::MemoryNode::new(0.7, 0.1);
        stressed.emotion = memory::EmotionalContext {
            arousal: 5.0,
            surprise: 1.0,
        };
        m.l1_memory.insert("stressed", stressed);

        let report = m.run_sleep_cycle();
        assert!(report.all_completed());

        let pr = report.outcomes[0].pruning.as_ref().unwrap();
        assert_eq!(
            pr.nodes_removed, 0,
            "stressed node with high activation must not be pruned"
        );
        assert_eq!(m.l1_memory.len(), 1, "stressed node survives");
    }

    /// E3.5 exit criterion 2: no retained node has activation below the floor
    /// after a sleep cycle with a populated l1_memory.
    #[test]
    fn lifecycle_no_retained_node_below_floor_after_sleep_cycle() {
        use memory::decay::SEMANTIC_FLOOR;

        let mut m = manager("invariant-agent", None);
        let elapsed = m.pruning_elapsed;

        // Insert 15 nodes with varying decay rates.
        for i in 0..15u32 {
            let lambda = i as f32 * 0.5;
            m.l1_memory
                .insert(format!("n{i}"), memory::MemoryNode::new(0.85, lambda));
        }

        m.run_sleep_cycle();

        // Post-cycle: every surviving node is strictly above the semantic floor.
        for (key, node) in m.l1_memory.iter() {
            let activation: f32 = node.activation_at(elapsed);
            assert!(
                activation > SEMANTIC_FLOOR,
                "retained node '{key}' has activation {activation:.4} ≤ floor {SEMANTIC_FLOOR:.4}"
            );
        }
    }

    /// E3.5: 100 consecutive sleep cycles with populated l1_memory remain stable.
    #[test]
    fn one_hundred_sleep_cycles_with_l1_memory_complete_without_error() {
        let mut m = manager("soak-prune-agent", None);

        // Seed the store with stable nodes (lambda=0 → never pruned).
        for i in 0..10u32 {
            m.l1_memory
                .insert(format!("stable-{i}"), memory::MemoryNode::new(0.9, 0.0));
        }

        for cycle in 0..100 {
            let report = m.run_sleep_cycle();
            assert!(
                report.all_completed(),
                "sleep cycle {cycle} should complete"
            );
        }

        // All 10 stable nodes must survive all 100 cycles.
        assert_eq!(
            m.l1_memory.len(),
            10,
            "stable nodes must persist across all 100 cycles"
        );
    }

    // ── E2.6 — L3 archive demotion from sleep cycle ───────────────────────────

    /// E2.6 exit criterion 2a: demotion is idempotent; fast-decaying L1 nodes
    /// end up in L3 after a sleep cycle.
    #[test]
    fn sleep_cycle_demotes_pruned_l1_nodes_to_l3() {
        let path = std::env::temp_dir().join("animaos_test_e26_sleep_demote.json");
        let _ = std::fs::remove_file(&path);

        let mut m = manager("l3-demote-agent", None);
        m.l3_archive = Some(memory::L3Archive::open(&path, 4, 100).unwrap());

        // Add a fast-decaying node that will be pruned at elapsed=1.0.
        m.l1_memory
            .insert("fast-decay", memory::MemoryNode::new(0.9, 20.0));
        // Stable node — stays in L1, not demoted.
        m.l1_memory
            .insert("stable", memory::MemoryNode::new(0.9, 0.0));

        m.run_sleep_cycle();

        // The fast-decay node should have been demoted to L3.
        let l3 = m.l3_archive.as_ref().unwrap();
        assert_eq!(l3.len(), 1, "one pruned node should be in L3");

        // Stable node remains in L1.
        assert_eq!(m.l1_memory.len(), 1);
        assert!(m.l1_memory.get("stable").is_some());

        let _ = std::fs::remove_file(&path);
    }

    /// E2.6 exit criterion 2b: demotion is idempotent — running a second sleep
    /// cycle on the same store does not re-insert the already-present entry.
    #[test]
    fn sleep_cycle_demotion_is_idempotent() {
        let path = std::env::temp_dir().join("animaos_test_e26_idempotent.json");
        let _ = std::fs::remove_file(&path);

        let mut m = manager("l3-idem-agent", None);
        m.l3_archive = Some(memory::L3Archive::open(&path, 4, 100).unwrap());

        m.l1_memory
            .insert("fast", memory::MemoryNode::new(0.9, 20.0));
        m.run_sleep_cycle();
        assert_eq!(m.l3_archive.as_ref().unwrap().len(), 1);

        // A second cycle with an empty L1 should not change L3 length.
        m.run_sleep_cycle();
        assert_eq!(
            m.l3_archive.as_ref().unwrap().len(),
            1,
            "second cycle must not duplicate the already-archived entry"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// E2.6 exit criterion 1: L3 archive survives a simulated process restart.
    #[test]
    fn l3_archive_survives_sleep_cycle_restart() {
        let path = std::env::temp_dir().join("animaos_test_e26_restart.json");
        let _ = std::fs::remove_file(&path);

        // First "process": sleep cycle with fast-decaying node.
        {
            let mut m = manager("restart-agent", None);
            m.l3_archive = Some(memory::L3Archive::open(&path, 4, 100).unwrap());
            m.l1_memory
                .insert("fast", memory::MemoryNode::new(0.9, 20.0));
            m.run_sleep_cycle();
            assert_eq!(
                m.l3_archive.as_ref().unwrap().len(),
                1,
                "one node should be in L3 before restart"
            );
            // m drops here, simulating process exit.
        }

        // Second "process": reopen archive from disk.
        {
            let l3 = memory::L3Archive::open(&path, 4, 100).unwrap();
            assert_eq!(l3.len(), 1, "L3 must survive process restart");
            // Search should return the demoted node.
            let query = memory::embed_memory_node(&memory::MemoryNode::new(0.9, 20.0));
            let results = l3.search(&query, 1);
            assert_eq!(results.len(), 1, "retrieval after restart must work");
        }

        let _ = std::fs::remove_file(&path);
    }

    // ── E3.6 — Replay validation wired into LifecycleManager ─────────────────

    /// E3.6 exit criterion 2: replay accuracy is logged for every sleep cycle
    /// when an L3 archive is configured.
    #[test]
    fn sleep_cycle_logs_replay_accuracy_when_l3_is_configured() {
        let path = std::env::temp_dir().join("animaos_test_e36_accuracy.json");
        let _ = std::fs::remove_file(&path);

        let mut m = manager("replay-log-agent", None);
        // Fast-decaying node — will be pruned and archived in the first cycle.
        m.l1_memory
            .insert("fast", memory::MemoryNode::new(0.9, 20.0));
        m.l3_archive = Some(memory::L3Archive::open(&path, 4, 100).unwrap());

        // Cycle 1: L3 is empty before maintenance, so queries_run = 0.
        let report1 = m.run_sleep_cycle();
        let rr1 = report1.outcomes[1]
            .replay
            .as_ref()
            .expect("replay report must be present when l3 is configured");
        assert_eq!(rr1.queries_run, 0, "L3 was empty at the start of cycle 1");

        // Cycle 2: the fast-decaying node is now in L3 with a unique embedding.
        let report2 = m.run_sleep_cycle();
        let rr2 = report2.outcomes[1]
            .replay
            .as_ref()
            .expect("replay report must be present in cycle 2");
        assert_eq!(rr2.queries_run, 1, "one entry in L3 → one query");
        assert_eq!(
            rr2.queries_validated, 1,
            "unique embedding → perfect retrieval"
        );
        assert!(
            (rr2.accuracy - 1.0).abs() < f32::EPSILON,
            "accuracy must be 1.0 when retrieval is perfect"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// E3.6 exit criterion 1: soak test demonstrates at least one rollback.
    ///
    /// Pre-populate L3 with entries that share the same embedding so that
    /// `search(q, 1)` always returns the lowest-ID entry.  With 3 entries,
    /// accuracy = 1/3 < threshold 0.5, triggering rollback.  The rolled-back
    /// nodes are re-inserted into the L1 pruning store.
    #[test]
    fn soak_test_sleep_cycle_triggers_rollback_and_restores_l1_nodes() {
        let path = std::env::temp_dir().join("animaos_test_e36_rollback_soak.json");
        let _ = std::fs::remove_file(&path);

        let mut m = manager("rollback-soak-agent", None);

        // Pre-populate L3 with 3 entries sharing the same embedding.
        {
            let mut l3 = memory::L3Archive::open(&path, 4, 100).unwrap();
            let node = memory::MemoryNode::new(0.9, 0.1);
            for i in 1..=3u64 {
                let item = memory::archive_memory_node(i, &format!("rb-key-{i}"), &node);
                let prov = memory::Provenance::now(memory::SourceTier::L1, &format!("rb-key-{i}"));
                l3.demote(item, prov).unwrap();
            }
            m.l3_archive = Some(l3);
        }

        m.replay_config = memory::ReplayConfig {
            accuracy_threshold: 0.5,
            max_sample_size: 16,
            rollback_enabled: true,
        };

        let report = m.run_sleep_cycle();

        // The GenerativeReplay phase (index 1) must have triggered rollback.
        let rr = report.outcomes[1]
            .replay
            .as_ref()
            .expect("replay report must be present");
        assert!(
            rr.triggered_rollback,
            "rollback must trigger when accuracy ({}) < threshold ({})",
            rr.accuracy, rr.threshold
        );
        assert!(rr.rolled_back > 0, "at least one node must be rolled back");

        // Rolled-back nodes must be re-inserted into L1 by run_sleep_cycle.
        assert!(
            !m.l1_memory.is_empty(),
            "rollback nodes must be re-inserted into L1"
        );
        assert_eq!(
            m.l1_memory.len(),
            rr.rolled_back,
            "l1_memory size must equal the rollback count"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Without an L3 archive, the GenerativeReplay phase falls back to the stub.
    #[test]
    fn sleep_cycle_replay_uses_stub_when_no_l3_configured() {
        let mut m = manager("no-l3-replay-agent", None);
        let report = m.run_sleep_cycle();

        let replay_outcome = &report.outcomes[1];
        assert_eq!(replay_outcome.routine, SleepRoutine::GenerativeReplay);
        assert!(
            replay_outcome.replay.is_none(),
            "replay report must be None when no L3 is configured"
        );
        assert!(replay_outcome.replay_rollback_nodes.is_empty());
    }

    /// 100 consecutive sleep cycles with L3 configured: every cycle logs
    /// a replay report (E3.6 exit criterion 2).
    #[test]
    fn one_hundred_sleep_cycles_with_l3_log_replay_report_every_cycle() {
        let path = std::env::temp_dir().join("animaos_test_e36_100cycles.json");
        let _ = std::fs::remove_file(&path);

        let mut m = manager("100-cycles-replay-agent", None);
        m.l3_archive = Some(memory::L3Archive::open(&path, 4, 1000).unwrap());

        // Seed with fast-decaying nodes with different embeddings (via arousal).
        for i in 0..3u32 {
            let mut node = memory::MemoryNode::new(0.9, 20.0);
            node.emotion.arousal = i as f32 * 2.0;
            m.l1_memory.insert(format!("n{i}"), node);
        }

        for cycle in 0..100 {
            let report = m.run_sleep_cycle();
            assert!(
                report.all_completed(),
                "sleep cycle {cycle} should complete"
            );
            // Every cycle must carry a replay report (E3.6 exit criterion 2).
            let rr = report.outcomes[1]
                .replay
                .as_ref()
                .expect("replay report must be present every cycle");
            assert!(
                rr.accuracy >= 0.0 && rr.accuracy <= 1.0,
                "accuracy must be in [0, 1], got {} in cycle {}",
                rr.accuracy,
                cycle
            );
        }

        // Verify via audit that GenerativeReplay completed 100 times.
        let replay_completions = m
            .audit
            .entries()
            .iter()
            .filter(|e| {
                matches!(e, AuditEntry::SleepPhaseCompleted { phase, .. }
                    if phase == "GenerativeReplay")
            })
            .count();
        assert_eq!(
            replay_completions, 100,
            "GenerativeReplay must complete every cycle (100 cycles × 1 = 100)"
        );

        let _ = std::fs::remove_file(&path);
    }

    // ── E3.7 — Dream exploration wired into LifecycleManager ─────────────────

    /// E3.7 exit criterion 1: dream report is logged every cycle when L3 is configured.
    #[test]
    fn sleep_cycle_logs_dream_report_when_l3_is_configured() {
        let path = std::env::temp_dir().join("animaos_test_e37_dream_log.json");
        let _ = std::fs::remove_file(&path);

        let mut m = manager("dream-log-agent", None);
        m.l3_archive = Some(memory::L3Archive::open(&path, 4, 100).unwrap());

        // Cycle 1: L3 is empty → no walks, but report is still emitted.
        let report1 = m.run_sleep_cycle();
        let dr1 = report1.outcomes[2]
            .dream
            .as_ref()
            .expect("dream report must be present when l3 is configured");
        assert_eq!(dr1.walks_run, 0, "empty L3 → no walks run");
        assert_eq!(dr1.candidates_found, 0);

        let _ = std::fs::remove_file(&path);
    }

    /// Without an L3 archive, the DreamExploration phase falls back to the stub.
    #[test]
    fn sleep_cycle_dream_uses_stub_when_no_l3_configured() {
        let mut m = manager("no-l3-dream-agent", None);
        let report = m.run_sleep_cycle();

        let dream_outcome = &report.outcomes[2];
        assert_eq!(dream_outcome.routine, SleepRoutine::DreamExploration);
        assert!(
            dream_outcome.dream.is_none(),
            "dream report must be None when no L3 is configured"
        );
        assert!(dream_outcome.dream_candidates.is_empty());
    }

    /// E3.7 exit criterion 1: same seed → same candidates across two cycles
    /// with identical L3 contents.
    #[test]
    fn dream_candidates_are_reproducible_across_lifecycle_cycles() {
        let path = std::env::temp_dir().join("animaos_test_e37_dream_repro.json");
        let _ = std::fs::remove_file(&path);

        let mut m = manager("dream-repro-agent", None);

        // Pre-populate L3 with distinct nodes (so walks can find neighbours).
        {
            let mut l3 = memory::L3Archive::open(&path, 4, 100).unwrap();
            for i in 1u64..=5 {
                let node = memory::MemoryNode::new(0.9, 0.1 * i as f32);
                let item = memory::archive_memory_node(i, &format!("d{i}"), &node);
                let prov = memory::Provenance::now(memory::SourceTier::L1, &format!("d{i}"));
                l3.demote(item, prov).unwrap();
            }
            m.l3_archive = Some(l3);
        }

        // Fix the seed and use a low threshold to maximise candidate yield.
        m.dream_config = memory::DreamConfig {
            seed: 99,
            similarity_threshold: 0.0,
            ..Default::default()
        };

        // Run cycle 1 and collect candidates.
        let r1 = m.run_sleep_cycle();
        let candidates1 = r1.outcomes[2].dream_candidates.clone();

        // Restore dream_config (run_sleep_cycle does not modify it).
        m.dream_config = memory::DreamConfig {
            seed: 99,
            similarity_threshold: 0.0,
            ..Default::default()
        };

        // Run cycle 2 with the same config — candidates must be identical.
        let r2 = m.run_sleep_cycle();
        let candidates2 = r2.outcomes[2].dream_candidates.clone();

        assert_eq!(
            candidates1, candidates2,
            "dream candidates must be reproducible for the same seed and archive"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// E3.7 exit criterion 2: threshold filtering excludes low-similarity edges
    /// at the lifecycle level.
    #[test]
    fn dream_threshold_filters_low_similarity_edges_in_lifecycle() {
        let path = std::env::temp_dir().join("animaos_test_e37_dream_thresh.json");
        let _ = std::fs::remove_file(&path);

        let mut m = manager("dream-thresh-agent", None);

        // Insert two orthogonal nodes (cosine similarity = 0).
        {
            use memory::archival::ArchivedItem;
            let mut l3 = memory::L3Archive::open(&path, 4, 100).unwrap();
            let item1 = ArchivedItem {
                id: 1,
                embedding: vec![1.0, 0.0, 0.0, 0.0],
                payload: vec![],
            };
            let item2 = ArchivedItem {
                id: 2,
                embedding: vec![0.0, 1.0, 0.0, 0.0],
                payload: vec![],
            };
            l3.demote(
                item1,
                memory::Provenance::now(memory::SourceTier::L1, "orth-a"),
            )
            .unwrap();
            l3.demote(
                item2,
                memory::Provenance::now(memory::SourceTier::L1, "orth-b"),
            )
            .unwrap();
            m.l3_archive = Some(l3);
        }

        // High threshold → orthogonal nodes (similarity 0) are filtered.
        m.dream_config = memory::DreamConfig {
            similarity_threshold: 0.9,
            num_walks: 4,
            walk_length: 4,
            ..Default::default()
        };

        let report = m.run_sleep_cycle();
        let dream_outcome = &report.outcomes[2];
        let dr = dream_outcome.dream.as_ref().unwrap();
        assert_eq!(
            dr.candidates_found, 0,
            "orthogonal nodes must be filtered by a high threshold"
        );
        assert!(dream_outcome.dream_candidates.is_empty());

        let _ = std::fs::remove_file(&path);
    }

    // ── E3.8 — Policy compilation wired into LifecycleManager ────────────────

    /// E3.8: compilation report is present when compilation_config is set.
    #[test]
    fn sleep_cycle_logs_compilation_report_when_config_is_set() {
        let dir = std::env::temp_dir().join("animaos_test_e38_compile_log");
        let _ = std::fs::remove_dir_all(&dir);

        let mut m = manager("compile-log-agent", None);
        m.compilation_config = Some(memory::CompilationConfig {
            output_dir: dir.clone(),
            formats: vec![memory::TrainingFormat::Alpaca],
            append: false,
        });

        let report = m.run_sleep_cycle();
        let comp_outcome = &report.outcomes[3];
        assert_eq!(comp_outcome.routine, SleepRoutine::PolicyCompilation);
        // Compilation ran, even if no pairs were found (no prior tasks in audit).
        assert!(
            comp_outcome.compilation.is_some(),
            "compilation report must be present when compilation_config is set"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Without a compilation_config, the PolicyCompilation phase is a no-op stub.
    #[test]
    fn sleep_cycle_compilation_uses_stub_when_no_config() {
        let mut m = manager("no-compile-agent", None);
        let report = m.run_sleep_cycle();

        let comp_outcome = &report.outcomes[3];
        assert_eq!(comp_outcome.routine, SleepRoutine::PolicyCompilation);
        assert!(comp_outcome.compilation.is_none());
    }

    /// E3.8 exit criterion 1: completed tasks appear in the compiled corpus.
    #[test]
    fn sleep_cycle_compiles_completed_tasks_into_training_corpus() {
        let dir = std::env::temp_dir().join("animaos_test_e38_corpus");
        let _ = std::fs::remove_dir_all(&dir);

        let mut m = manager("corpus-agent", None);

        // Inject a task-completed pair directly into the audit log.
        m.audit.push(AuditEntry::TaskStarted {
            agent_id: "corpus-agent".into(),
            task_id: 1,
            tier: 0,
            prompt: "What is 1+1?".into(),
        });
        m.audit.push(AuditEntry::TaskCompleted {
            agent_id: "corpus-agent".into(),
            task_id: 1,
            tokens_emitted: 1,
            response: "2".into(),
        });

        m.compilation_config = Some(memory::CompilationConfig {
            output_dir: dir.clone(),
            formats: vec![memory::TrainingFormat::Alpaca],
            append: false,
        });

        let report = m.run_sleep_cycle();
        let cr = report.outcomes[3]
            .compilation
            .as_ref()
            .expect("compilation report must be present");

        assert_eq!(cr.pairs_compiled, 1, "one task pair must be compiled");
        assert_eq!(cr.files_written, 1, "one JSONL file must be written");

        // Validate the Alpaca file on disk.
        let alpaca_path = dir.join("alpaca.jsonl");
        assert!(alpaca_path.exists(), "alpaca.jsonl must be created");
        let content = std::fs::read_to_string(&alpaca_path).unwrap();
        let record: memory::AlpacaRecord = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(record.instruction, "What is 1+1?");
        assert_eq!(record.output, "2");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// E3.8 exit criterion 2: emergency consolidation flag is accessible via
    /// the public API.
    #[test]
    fn emergency_consolidation_produces_a_marked_report() {
        let dir = std::env::temp_dir().join("animaos_test_e38_emergency");
        let _ = std::fs::remove_dir_all(&dir);

        let entries = vec![
            memory::AuditTraceEntry::TaskStarted {
                task_id: 99,
                tier: 0,
                prompt: "urgent".into(),
            },
            memory::AuditTraceEntry::TaskCompleted {
                task_id: 99,
                tokens_emitted: 1,
                response: "done".into(),
            },
        ];

        let cfg = memory::CompilationConfig {
            output_dir: dir.clone(),
            formats: vec![memory::TrainingFormat::Alpaca],
            append: false,
        };

        let (report, pairs, errors) = memory::emergency_consolidate(&entries, &cfg);
        assert!(errors.is_empty());
        assert!(report.emergency_consolidation, "emergency flag must be set");
        assert_eq!(pairs.len(), 1);
        assert_eq!(report.files_written, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── E11 S11.5 — Dreaming-phase self-improvement reflection wiring ──────────

    use skills::{EpisodeSummary, SkillRegistry};

    /// Three episodes that all pair the same two tools → one friction pattern
    /// above the default reflection threshold.
    fn co_occurrence_summaries() -> Vec<EpisodeSummary> {
        (0..3)
            .map(|i| EpisodeSummary {
                episode_id: format!("ep-{i}"),
                summary: format!("episode {i}: searched then archived"),
                tools_used: vec!["web-search".to_string(), "archive".to_string()],
                success: true,
            })
            .collect()
    }

    #[test]
    fn skill_reflection_disabled_by_default() {
        let m = manager("agent-no-reflection", Some(1));
        assert!(!m.skill_reflection_enabled());
        assert!(m.skill_registry_handle().is_none());
    }

    #[test]
    fn with_skill_registry_builder_installs_registry() {
        let m = manager("agent-reflection-builder", Some(1))
            .with_skill_registry(SkillRegistry::default());
        assert!(m.skill_reflection_enabled());
        assert!(m.skill_registry_handle().is_some());
    }

    #[test]
    fn dreaming_phase_registers_proposed_skill_and_emits_audit() {
        let mut m = manager("dream-agent", Some(0)).with_skill_registry(SkillRegistry::default());
        for s in co_occurrence_summaries() {
            m.record_episode_summary(s);
        }

        // Run one explicit sleep cycle: the Dreaming phase reflects + registers.
        m.run_sleep_cycle();

        // The shared registry now holds >=1 Proposed agent-authored skill.
        let registry = m.skill_registry_handle().unwrap();
        let guard = registry.lock().unwrap();
        let proposed: Vec<_> = guard
            .list_all()
            .into_iter()
            .filter(|e| e.state.is_proposed())
            .collect();
        assert!(
            !proposed.is_empty(),
            "Dreaming reflection must register at least one Proposed skill"
        );
        for e in &proposed {
            assert_eq!(e.provenance.authored_by, skills::SkillAuthor::Agent);
        }
        // Proposed skills are not active (gated on operator approval).
        assert_eq!(guard.list_active().len(), 0);
        drop(guard);

        // Audit: exactly one SkillReflectionCompleted + >=1 SkillRegistered.
        let entries = m.audit.entries();
        assert_eq!(
            entries
                .iter()
                .filter(|e| matches!(e, AuditEntry::SkillReflectionCompleted { .. }))
                .count(),
            1
        );
        assert!(
            entries
                .iter()
                .any(|e| matches!(e, AuditEntry::SkillRegistered { .. })),
            "at least one SkillRegistered audit entry expected"
        );
    }

    #[test]
    fn dreaming_phase_with_no_pattern_registers_nothing() {
        let mut m =
            manager("dream-agent-empty", Some(0)).with_skill_registry(SkillRegistry::default());
        // Distinct tools per episode → no qualifying co-occurrence pattern.
        for (i, tools) in [
            ["tool-a", "tool-b"],
            ["tool-c", "tool-d"],
            ["tool-e", "tool-f"],
        ]
        .into_iter()
        .enumerate()
        {
            m.record_episode_summary(EpisodeSummary {
                episode_id: format!("e{i}"),
                summary: format!("episode {i}"),
                tools_used: tools.iter().map(|t| t.to_string()).collect(),
                success: true,
            });
        }

        m.run_sleep_cycle();

        let registry = m.skill_registry_handle().unwrap();
        assert!(
            registry.lock().unwrap().is_empty(),
            "no skill should be registered without a qualifying pattern"
        );

        // The reflection summary is still emitted; no SkillRegistered entries.
        let entries = m.audit.entries();
        assert_eq!(
            entries
                .iter()
                .filter(|e| matches!(e, AuditEntry::SkillReflectionCompleted { .. }))
                .count(),
            1
        );
        assert_eq!(
            entries
                .iter()
                .filter(|e| matches!(e, AuditEntry::SkillRegistered { .. }))
                .count(),
            0
        );
    }

    #[test]
    fn dreaming_phase_without_registry_is_unchanged() {
        // No registry installed: the sleep cycle behaves exactly as before —
        // no reflection audit entries even when episodes are buffered.
        let mut m = manager("dream-agent-noreg", Some(0));
        for s in co_occurrence_summaries() {
            m.record_episode_summary(s);
        }
        m.run_sleep_cycle();

        let entries = m.audit.entries();
        assert!(
            !entries
                .iter()
                .any(|e| matches!(e, AuditEntry::SkillReflectionCompleted { .. })),
            "no reflection should run without a registry"
        );
        assert!(!entries
            .iter()
            .any(|e| matches!(e, AuditEntry::SkillRegistered { .. })));
    }
}
