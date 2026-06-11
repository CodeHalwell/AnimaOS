// crates/vita/src/router.rs
//! Thalamic Router — E5.3.
//!
//! Route selection: which model, which tools, which memory scopes,
//! which prompt scaffolding, and which termination conditions apply for a
//! given cortex invocation.
//!
//! # Design
//!
//! The default implementation is a **static route table** keyed on the
//! [`CostClass`] produced by the Striatal Gate (E5.2). Three baseline routes
//! ship out of the box:
//!
//! | Route ID      | Model     | Tools          | Memory scope        | Max turns |
//! |---------------|-----------|----------------|---------------------|-----------|
//! | `cheap-local` | CheapLocal| clock, echo    | identity + L1       |         3 |
//! | `mid-tier`    | MidTier   | clock+echo+tio | identity + L1 + L2  |         8 |
//! | `frontier`    | Frontier  | all built-ins  | all tiers           |        20 |
//!
//! The [`Router`] trait is the hookpoint for a learned routing replacement
//! (S5.3.5) — a learned model may be installed without changing any caller.
//!
//! # Route validation (exit criterion 2)
//!
//! All routes are validated at [`StaticRouter`] **construction time**.
//! A route that references an unknown tool, disables identity memory, uses
//! an empty route ID, or specifies a zero termination policy is rejected
//! immediately with a [`RouteError`] — never at invocation time.
//!
//! # Router → cortex handshake (S5.3.3)
//!
//! [`build_routed_request`] is the integration point between the router and the
//! cortex bridge.  It:
//!
//! 1. Filters the available [`ToolSpec`] list to only the names permitted by the
//!    route's [`ToolScope`].
//! 2. Sets [`InvokeRequest::memory_scope`] so the cortex knows which memory tiers
//!    it may access.
//! 3. Sets [`InvokeRequest::max_turns`] and [`InvokeRequest::max_tool_calls`] from
//!    the route's [`TerminationPolicy`].
//! 4. Applies the route's [`PromptScaffold`] to the task description.
//!
//! Identity memory is always present in the request per S5.3.4.
//!
//! # Exit criteria (E5.3)
//!
//! 1. Each baseline route is exercised in an integration test that asserts the
//!    cortex sees exactly the configured tool subset and memory scope.
//! 2. A route misconfiguration (unknown tool reference, missing identity memory,
//!    zero termination limit) is rejected at [`StaticRouter::new`] construction
//!    time, not at invocation time.

#![forbid(unsafe_code)]

use crate::{
    AuditEntry, AuditLog, CostClass, HomeostaticSignals, InvokeMemoryScope, InvokeRequest,
    SemanticClass, ToolSpec,
};
#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

// ── Route ID ──────────────────────────────────────────────────────────────────

/// Stable identifier for a route configuration.
///
/// The three baseline route IDs shipped by [`StaticRouter`] are:
/// - `RouteId::CHEAP_LOCAL`
/// - `RouteId::MID_TIER`
/// - `RouteId::FRONTIER`
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RouteId(pub String);

impl RouteId {
    /// The "cheap-local" baseline route ID string.
    pub const CHEAP_LOCAL: &'static str = "cheap-local";
    /// The "mid-tier" baseline route ID string.
    pub const MID_TIER: &'static str = "mid-tier";
    /// The "frontier" baseline route ID string.
    pub const FRONTIER: &'static str = "frontier";

    /// Construct a new route ID from any string-like value.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Returns the ID as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for RouteId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

// ── Model selector ────────────────────────────────────────────────────────────

/// Which LLM backend tier handles this cortex invocation.
///
/// Maps onto the same tier hierarchy as [`CostClass`], but belongs to the
/// router rather than the gate — the gate decides *whether* to invoke; the
/// router decides *how* (including which model).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelSelector {
    /// Low-cost, fast local model (e.g. a quantised 7B on-device model).
    CheapLocal,
    /// Intermediate cost/capability tradeoff (e.g. claude-3-haiku).
    MidTier,
    /// Full-capability frontier model (e.g. claude-opus-4).
    Frontier,
}

impl ModelSelector {
    /// Human-readable label used in audit entries and tooling.
    pub fn as_str(self) -> &'static str {
        match self {
            ModelSelector::CheapLocal => "cheap-local",
            ModelSelector::MidTier => "mid-tier",
            ModelSelector::Frontier => "frontier",
        }
    }
}

// ── Tool scope ────────────────────────────────────────────────────────────────

/// Tools the cortex is permitted to call during an invocation on this route.
///
/// `allowed_tools` is a list of tool identifiers matching `ToolDriver::id`.
/// An empty list means the cortex has no tool access.
///
/// At [`StaticRouter`] construction the router validates that every name is
/// non-empty.  When `known_tools` is supplied to [`StaticRouter::new`], tool
/// names not in that list are rejected as unknown-tool references (exit
/// criterion 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolScope {
    /// Human-readable label for this scope (carried into audit entries).
    pub name: String,
    /// Tool identifiers permitted on this route.
    pub allowed_tools: Vec<String>,
}

impl ToolScope {
    /// Construct a new tool scope.
    pub fn new(name: impl Into<String>, allowed_tools: Vec<String>) -> Self {
        Self {
            name: name.into(),
            allowed_tools,
        }
    }

    /// Returns `true` when `tool_name` is explicitly permitted by this scope.
    pub fn allows(&self, tool_name: &str) -> bool {
        self.allowed_tools.iter().any(|t| t == tool_name)
    }

    /// Filter a slice of [`ToolSpec`]s to only those permitted by this scope.
    ///
    /// The order of returned specs matches the order in `available`, not the
    /// order in `allowed_tools`, to preserve stable serialisation order.
    pub fn filter_tools(&self, available: &[ToolSpec]) -> Vec<ToolSpec> {
        available
            .iter()
            .filter(|t| self.allows(&t.name))
            .cloned()
            .collect()
    }
}

// ── Memory scope ──────────────────────────────────────────────────────────────

/// Which memory tiers the cortex may read from and write to during this
/// invocation (S5.3.4), and whether the KV-cache gating controller (E5.4)
/// is active for block-level eviction decisions.
///
/// `identity` **must always be `true`** on every route — a route with
/// `identity: false` is rejected at [`StaticRouter`] construction time with
/// [`RouteError::IdentityMemoryDisabled`] (exit criterion 2 + S5.3.4).
///
/// `kv_controller` enables the E5.4 learned gate instead of plain LRU for
/// block eviction decisions during this invocation.  Disabled by default on
/// all baseline routes; opt-in per route (S5.4.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryScope {
    /// Identity memory (stable user/agent facts) — **must be `true`** per S5.3.4.
    pub identity: bool,
    /// L1 working memory (active token context).
    pub l1: bool,
    /// L2 warm ARC cache.
    pub l2: bool,
    /// L3 persistent archive.
    pub l3: bool,
    /// Use the KV-cache gating controller (E5.4) for block-level eviction.
    ///
    /// When `true`, [`crate::kv_gate::gate_working_context`] is invoked before
    /// each cortex turn to decide which blocks to retain.  On any controller
    /// fault the decision falls back to LRU and a
    /// [`crate::AuditEntry::KvControllerFaulted`] entry is written.
    ///
    /// Default: `false` on all baseline routes.
    pub kv_controller: bool,
}

impl MemoryScope {
    /// Minimal scope: identity + L1 only (cheap-local default).
    pub fn minimal() -> Self {
        Self {
            identity: true,
            l1: true,
            l2: false,
            l3: false,
            kv_controller: false,
        }
    }

    /// Mid scope: identity + L1 + L2 (mid-tier default).
    pub fn mid() -> Self {
        Self {
            identity: true,
            l1: true,
            l2: true,
            l3: false,
            kv_controller: false,
        }
    }

    /// Full scope: all tiers (frontier default).
    pub fn full() -> Self {
        Self {
            identity: true,
            l1: true,
            l2: true,
            l3: true,
            kv_controller: false,
        }
    }

    /// Full scope with the KV-cache controller enabled (frontier + E5.4).
    pub fn full_with_kv_controller() -> Self {
        Self {
            kv_controller: true,
            ..Self::full()
        }
    }

    /// Convert to the IPC wire representation used in [`InvokeRequest`].
    pub fn to_invoke_scope(&self) -> InvokeMemoryScope {
        InvokeMemoryScope {
            identity: self.identity,
            l1: self.l1,
            l2: self.l2,
            l3: self.l3,
        }
    }
}

// ── Prompt scaffold ───────────────────────────────────────────────────────────

/// Prompt prefix and suffix applied to the task description sent to the cortex.
///
/// The scaffold is applied by [`build_routed_request`]: the final description
/// sent to the cortex is `{system_prefix}{task_description}{system_suffix}`.
/// Both strings may be empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptScaffold {
    /// Text prepended to the task description.
    pub system_prefix: String,
    /// Text appended to the task description.
    pub system_suffix: String,
}

impl PromptScaffold {
    /// A no-op scaffold (empty prefix and suffix).
    pub fn empty() -> Self {
        Self {
            system_prefix: String::new(),
            system_suffix: String::new(),
        }
    }

    /// Apply this scaffold around a base task description.
    pub fn apply(&self, base: &str) -> String {
        if self.system_prefix.is_empty() && self.system_suffix.is_empty() {
            base.to_string()
        } else {
            format!("{}{}{}", self.system_prefix, base, self.system_suffix)
        }
    }
}

// ── Termination policy ────────────────────────────────────────────────────────

/// Conditions under which the cortex invocation must terminate.
///
/// Both `max_turns` and `max_tool_calls` must be > 0; a route with zero values
/// is rejected at construction time (exit criterion 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminationPolicy {
    /// Maximum number of planning + acting turns.
    pub max_turns: u32,
    /// Maximum total tool calls across the invocation.
    pub max_tool_calls: u32,
}

impl TerminationPolicy {
    /// Tight limits for cheap-local invocations (3 turns / 3 calls).
    pub fn cheap_local() -> Self {
        Self {
            max_turns: 3,
            max_tool_calls: 3,
        }
    }

    /// Balanced limits for mid-tier invocations (8 turns / 8 calls).
    pub fn mid_tier() -> Self {
        Self {
            max_turns: 8,
            max_tool_calls: 8,
        }
    }

    /// Generous limits for frontier invocations (20 turns / 20 calls).
    pub fn frontier() -> Self {
        Self {
            max_turns: 20,
            max_tool_calls: 20,
        }
    }
}

// ── Route ─────────────────────────────────────────────────────────────────────

/// A fully-specified route configuration (S5.3.1).
///
/// Routes are the unit of cortex configuration: every cortex invocation uses
/// exactly one route.  The route determines the model, tool access, memory
/// scope, prompt framing, and termination conditions.
#[derive(Debug, Clone)]
pub struct Route {
    /// Stable identifier for this route.
    pub id: RouteId,
    /// LLM backend tier for this route.
    pub model: ModelSelector,
    /// Tool access constraints.
    pub tool_scope: ToolScope,
    /// Memory tier access constraints.
    pub memory_scope: MemoryScope,
    /// Prompt prefix/suffix applied to the task description.
    pub prompt_scaffold: PromptScaffold,
    /// Invocation termination conditions.
    pub termination: TerminationPolicy,
}

// ── Route validation error ────────────────────────────────────────────────────

/// Errors detected at [`StaticRouter`] construction time (exit criterion 2).
///
/// Every variant corresponds to a structural defect in a route configuration
/// that would cause incorrect or unsafe behaviour at invocation time.
/// By surfacing these errors at startup, callers can fail fast rather than
/// silently misbehave in production.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteError {
    /// The route has an empty `RouteId`.
    EmptyRouteId,
    /// The route's `MemoryScope.identity` is `false`, violating S5.3.4.
    ///
    /// Identity memory must be accessible on every route.
    IdentityMemoryDisabled {
        /// ID of the offending route.
        route_id: String,
    },
    /// A tool name in the `ToolScope` is an empty string.
    EmptyToolName {
        /// ID of the offending route.
        route_id: String,
        /// Zero-based index into `ToolScope.allowed_tools`.
        index: usize,
    },
    /// The `TerminationPolicy.max_turns` is zero.
    ZeroMaxTurns {
        /// ID of the offending route.
        route_id: String,
    },
    /// The `TerminationPolicy.max_tool_calls` is zero.
    ZeroMaxToolCalls {
        /// ID of the offending route.
        route_id: String,
    },
    /// A tool referenced in `ToolScope.allowed_tools` is not in `known_tools`.
    ///
    /// Only raised when `known_tools` is non-empty (opt-in registry validation).
    UnknownTool {
        /// ID of the offending route.
        route_id: String,
        /// The unrecognised tool name.
        tool_name: String,
    },
}

impl core::fmt::Display for RouteError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            RouteError::EmptyRouteId => write!(f, "route has empty ID"),
            RouteError::IdentityMemoryDisabled { route_id } => write!(
                f,
                "route '{route_id}': identity memory must be enabled (S5.3.4)"
            ),
            RouteError::EmptyToolName { route_id, index } => write!(
                f,
                "route '{route_id}': tool scope entry [{index}] has an empty name"
            ),
            RouteError::ZeroMaxTurns { route_id } => {
                write!(f, "route '{route_id}': termination.max_turns must be > 0")
            }
            RouteError::ZeroMaxToolCalls { route_id } => write!(
                f,
                "route '{route_id}': termination.max_tool_calls must be > 0"
            ),
            RouteError::UnknownTool {
                route_id,
                tool_name,
            } => write!(
                f,
                "route '{route_id}': tool '{tool_name}' is not in the known-tools list"
            ),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for RouteError {}

// ── Validation helper ─────────────────────────────────────────────────────────

/// Validate a single [`Route`] against an optional set of known tool names.
///
/// Called by [`StaticRouter::new`] for each route it manages.
///
/// Pass an empty slice for `known_tools` to skip tool-registry validation
/// (useful in tests or when constructing routes before a registry is available).
/// Pass the actual registered tool IDs to enable full validation.
pub fn validate_route(route: &Route, known_tools: &[String]) -> Result<(), RouteError> {
    if route.id.0.is_empty() {
        return Err(RouteError::EmptyRouteId);
    }

    if !route.memory_scope.identity {
        return Err(RouteError::IdentityMemoryDisabled {
            route_id: route.id.0.clone(),
        });
    }

    for (i, tool_name) in route.tool_scope.allowed_tools.iter().enumerate() {
        if tool_name.is_empty() {
            return Err(RouteError::EmptyToolName {
                route_id: route.id.0.clone(),
                index: i,
            });
        }
        if !known_tools.is_empty() && !known_tools.contains(tool_name) {
            return Err(RouteError::UnknownTool {
                route_id: route.id.0.clone(),
                tool_name: tool_name.clone(),
            });
        }
    }

    if route.termination.max_turns == 0 {
        return Err(RouteError::ZeroMaxTurns {
            route_id: route.id.0.clone(),
        });
    }

    if route.termination.max_tool_calls == 0 {
        return Err(RouteError::ZeroMaxToolCalls {
            route_id: route.id.0.clone(),
        });
    }

    Ok(())
}

// ── Router trait (S5.3.5 hookpoint) ──────────────────────────────────────────

/// Abstraction over route resolution (S5.3.5).
///
/// The default implementation is [`StaticRouter`]; a learned replacement may
/// be installed without changing any caller.  The only contract is that the
/// returned [`Route`] reference is valid for the lifetime of the router, and
/// that the resolution is deterministic for the same inputs.
pub trait Router: Send + Sync {
    /// Resolve the route for the given event context and gate cost class.
    ///
    /// - `semantic_class`: the event's semantic classification (reserved for
    ///   future per-class specialisation; currently ignored by [`StaticRouter`]).
    /// - `cost_class`: the cost tier selected by the Striatal Gate (E5.2).
    fn resolve(&self, semantic_class: SemanticClass, cost_class: CostClass) -> &Route;
}

// ── Static router (S5.3.2) ────────────────────────────────────────────────────

/// Static route table implementation of [`Router`] (S5.3.2).
///
/// Three baseline routes are held by value.  The `CostClass` returned by the
/// Striatal Gate (E5.2) maps directly to one of the three routes:
///
/// | `CostClass`    | Route ID      |
/// |----------------|---------------|
/// | `CheapLocal`   | `cheap-local` |
/// | `MidTier`      | `mid-tier`    |
/// | `Frontier`     | `frontier`    |
///
/// `SemanticClass` is accepted for forward compatibility but currently does not
/// differentiate routing (all classes map to the same cost-class route).
#[derive(Debug)]
pub struct StaticRouter {
    cheap_local: Route,
    mid_tier: Route,
    frontier: Route,
}

impl StaticRouter {
    /// Construct a `StaticRouter` with the three provided routes.
    ///
    /// Routes are validated immediately (exit criterion 2).
    ///
    /// Pass an empty slice for `known_tools` to skip tool-name registry
    /// validation.  Pass the actual registered tool IDs to enable full
    /// validation (recommended in production).
    pub fn new(
        cheap_local: Route,
        mid_tier: Route,
        frontier: Route,
        known_tools: &[String],
    ) -> Result<Self, RouteError> {
        validate_route(&cheap_local, known_tools)?;
        validate_route(&mid_tier, known_tools)?;
        validate_route(&frontier, known_tools)?;

        Ok(Self {
            cheap_local,
            mid_tier,
            frontier,
        })
    }

    /// Construct a `StaticRouter` using the three default baseline routes.
    ///
    /// Tool-name validation uses the built-in tool IDs (`clock`, `echo`,
    /// `text-io`) so the validation is complete without requiring a live
    /// [`ToolRegistry`] reference.
    pub fn with_defaults() -> Result<Self, RouteError> {
        let (cl, mt, fr) = default_routes();
        let known_tools: Vec<String> = vec!["clock".into(), "echo".into(), "text-io".into()];
        Self::new(cl, mt, fr, &known_tools)
    }

    /// Returns a reference to the cheap-local baseline route.
    pub fn cheap_local_route(&self) -> &Route {
        &self.cheap_local
    }

    /// Returns a reference to the mid-tier baseline route.
    pub fn mid_tier_route(&self) -> &Route {
        &self.mid_tier
    }

    /// Returns a reference to the frontier baseline route.
    pub fn frontier_route(&self) -> &Route {
        &self.frontier
    }
}

impl Router for StaticRouter {
    /// Map a `CostClass` to the corresponding route.
    ///
    /// `semantic_class` is accepted for forward compatibility but ignored by
    /// this implementation.
    fn resolve(&self, _semantic_class: SemanticClass, cost_class: CostClass) -> &Route {
        match cost_class {
            CostClass::CheapLocal => &self.cheap_local,
            CostClass::MidTier => &self.mid_tier,
            CostClass::Frontier => &self.frontier,
        }
    }
}

// ── Default routes ────────────────────────────────────────────────────────────

/// Build the three baseline routes shipped by [`StaticRouter::with_defaults`].
///
/// Routes are constructed without known-tool validation so callers can use
/// them before a [`ToolRegistry`] is available.
pub fn default_routes() -> (Route, Route, Route) {
    let cheap_local = Route {
        id: RouteId::new(RouteId::CHEAP_LOCAL),
        model: ModelSelector::CheapLocal,
        tool_scope: ToolScope::new("core", vec!["clock".to_string(), "echo".to_string()]),
        memory_scope: MemoryScope::minimal(),
        prompt_scaffold: PromptScaffold {
            system_prefix: "[cheap-local] ".to_string(),
            system_suffix: String::new(),
        },
        termination: TerminationPolicy::cheap_local(),
    };

    let mid_tier = Route {
        id: RouteId::new(RouteId::MID_TIER),
        model: ModelSelector::MidTier,
        tool_scope: ToolScope::new(
            "standard",
            vec![
                "clock".to_string(),
                "echo".to_string(),
                "text-io".to_string(),
            ],
        ),
        memory_scope: MemoryScope::mid(),
        prompt_scaffold: PromptScaffold {
            system_prefix: "[mid-tier] ".to_string(),
            system_suffix: String::new(),
        },
        termination: TerminationPolicy::mid_tier(),
    };

    let frontier = Route {
        id: RouteId::new(RouteId::FRONTIER),
        model: ModelSelector::Frontier,
        tool_scope: ToolScope::new(
            "full",
            vec![
                "clock".to_string(),
                "echo".to_string(),
                "text-io".to_string(),
            ],
        ),
        memory_scope: MemoryScope::full(),
        prompt_scaffold: PromptScaffold {
            system_prefix: "[frontier] ".to_string(),
            system_suffix: String::new(),
        },
        termination: TerminationPolicy::frontier(),
    };

    (cheap_local, mid_tier, frontier)
}

// ── Audit helper ──────────────────────────────────────────────────────────────

// ── Homeostatic modulation (E5.7, S5.7.5) ────────────────────────────────────

/// The outcome of a modulated route resolution (E5.7).
///
/// When [`StaticRouter::resolve_with_modulation`] determines that homeostatic
/// signals require a cheaper route than the gate's cost class would select,
/// `was_modulated` is `true` and `effective_route` points to the downgraded
/// route; `requested_route` always points to what the gate asked for.
///
/// Both fields are references into the `StaticRouter`, so the struct has the
/// router's lifetime.
#[derive(Debug)]
pub struct ModulationDecision<'r> {
    /// The route the gate's cost class would have selected (pre-modulation).
    pub requested_route: &'r Route,
    /// The route that will actually be used (post-modulation).
    ///
    /// Equal to `requested_route` when no modulation was applied.
    pub effective_route: &'r Route,
    /// Whether the effective route differs from the requested route.
    pub was_modulated: bool,
    /// Human-readable explanation of the modulation, if applied.
    ///
    /// `None` when `was_modulated = false`.
    pub modulation_reason: Option<String>,
}

impl StaticRouter {
    /// Resolve a route and apply homeostatic modulation (E5.7, S5.7.5).
    ///
    /// Modulation rules (applied in priority order):
    ///
    /// | Condition | Effect |
    /// |-----------|--------|
    /// | `financial_budget < 0.20` OR `power_budget < 0.20` | Force `cheap-local` |
    /// | `financial_budget < 0.40` OR `power_budget < 0.40` | Downgrade `frontier` → `mid-tier` |
    /// | `thermal_load > 0.80` | Downgrade `frontier` → `mid-tier` |
    /// | otherwise | No modulation — route as selected by the gate |
    ///
    /// Under severe resource pressure (rule 1), even a `CheapLocal` gate
    /// decision is passed through unchanged because it is already the cheapest
    /// available route.
    ///
    /// # Return value
    ///
    /// A [`ModulationDecision`] carrying both the pre- and post-modulation
    /// routes and a human-readable reason string when modulation was applied.
    pub fn resolve_with_modulation(
        &self,
        semantic_class: SemanticClass,
        cost_class: CostClass,
        signals: &HomeostaticSignals,
    ) -> ModulationDecision<'_> {
        let requested_route = self.resolve(semantic_class, cost_class);

        // Rule 1: Severe resource depletion — force cheap-local.
        if signals.financial_budget < 0.20 || signals.power_budget < 0.20 {
            let effective_route = self.resolve(semantic_class, CostClass::CheapLocal);
            let was_modulated = effective_route.id != requested_route.id;
            return ModulationDecision {
                requested_route,
                effective_route,
                was_modulated,
                modulation_reason: if was_modulated {
                    Some(format!(
                        "severe resource pressure (financial_budget={:.2}, power_budget={:.2}) \
                         — forced to cheap-local",
                        signals.financial_budget, signals.power_budget
                    ))
                } else {
                    None
                },
            };
        }

        // Rule 2: Moderate resource depletion — downgrade frontier → mid-tier.
        if (signals.financial_budget < 0.40 || signals.power_budget < 0.40)
            && cost_class == CostClass::Frontier
        {
            let effective_route = self.resolve(semantic_class, CostClass::MidTier);
            return ModulationDecision {
                requested_route,
                effective_route,
                was_modulated: true,
                modulation_reason: Some(format!(
                    "moderate resource pressure (financial_budget={:.2}, power_budget={:.2}) \
                     — downgraded frontier → mid-tier",
                    signals.financial_budget, signals.power_budget
                )),
            };
        }

        // Rule 3: Thermal stress — downgrade frontier → mid-tier.
        if signals.thermal_load > 0.80 && cost_class == CostClass::Frontier {
            let effective_route = self.resolve(semantic_class, CostClass::MidTier);
            return ModulationDecision {
                requested_route,
                effective_route,
                was_modulated: true,
                modulation_reason: Some(format!(
                    "thermal stress (thermal_load={:.2}) — downgraded frontier → mid-tier",
                    signals.thermal_load
                )),
            };
        }

        // No modulation applied.
        ModulationDecision {
            requested_route,
            effective_route: requested_route,
            was_modulated: false,
            modulation_reason: None,
        }
    }
}

/// Record a modulated router decision in the audit log.
///
/// Extends [`record_router_decision`] with the modulation outcome so the
/// `anima why` CLI can report both what the gate asked for and what the
/// router delivered.
pub fn record_modulated_router_decision(
    audit: &mut AuditLog,
    agent_id: &str,
    event_id: &str,
    decision: &ModulationDecision<'_>,
    tools_available: usize,
    tools_permitted: usize,
) {
    // Always record the effective route.
    record_router_decision(
        audit,
        agent_id,
        event_id,
        decision.effective_route,
        tools_available,
        tools_permitted,
    );
    // If modulation changed the route, also log the modulation event.
    if decision.was_modulated {
        audit.push(AuditEntry::RouterModulated {
            agent_id: agent_id.to_string(),
            event_id: event_id.to_string(),
            requested_route_id: decision.requested_route.id.0.clone(),
            effective_route_id: decision.effective_route.id.0.clone(),
            reason: decision.modulation_reason.clone().unwrap_or_default(),
        });
    }
}

// ── Audit helper ──────────────────────────────────────────────────────────────

/// Called by [`build_routed_request`] (and by the vita dispatch loop) after
/// a route is resolved so every routing decision is permanently traceable.
pub fn record_router_decision(
    audit: &mut AuditLog,
    agent_id: &str,
    event_id: &str,
    route: &Route,
    tools_available: usize,
    tools_permitted: usize,
) {
    audit.push(AuditEntry::RouterDecision {
        agent_id: agent_id.to_string(),
        event_id: event_id.to_string(),
        route_id: route.id.0.clone(),
        model_selector: route.model.as_str().to_string(),
        tool_scope_name: route.tool_scope.name.clone(),
        tools_available,
        tools_permitted,
        memory_scope_identity: route.memory_scope.identity,
        memory_scope_l1: route.memory_scope.l1,
        memory_scope_l2: route.memory_scope.l2,
        memory_scope_l3: route.memory_scope.l3,
        max_turns: route.termination.max_turns,
        max_tool_calls: route.termination.max_tool_calls,
    });
}

// ── InvokeRequest builder (S5.3.3) ────────────────────────────────────────────

/// Build a [`InvokeRequest`] scoped to the given route (S5.3.3).
///
/// The returned request:
///
/// - Contains **only** the tools permitted by `route.tool_scope` (filtered
///   from `all_tools`).
/// - Carries the route's [`MemoryScope`] in `memory_scope` so the cortex
///   cannot access tiers outside the scope.
/// - Carries the route's [`TerminationPolicy`] in `max_turns` / `max_tool_calls`.
/// - Always includes the identity snapshot per S5.3.4.
/// - Applies the route's [`PromptScaffold`] to the `description`.
pub fn build_routed_request(
    task_id: impl Into<String>,
    agent_id: impl Into<String>,
    description: impl Into<String>,
    route: &Route,
    all_tools: &[ToolSpec],
    identity: serde_json::Value,
) -> InvokeRequest {
    let permitted_tools = route.tool_scope.filter_tools(all_tools);
    let scaffolded_description = route.prompt_scaffold.apply(&description.into());

    InvokeRequest {
        task_id: task_id.into(),
        agent_id: agent_id.into(),
        description: scaffolded_description,
        tools: permitted_tools,
        identity,
        route_id: Some(route.id.0.clone()),
        memory_scope: Some(route.memory_scope.to_invoke_scope()),
        max_turns: Some(route.termination.max_turns),
        max_tool_calls: Some(route.termination.max_tool_calls),
    }
}

// ── Per-tier backend map (E9 S9.5) ────────────────────────────────────────────

/// A router-aware backend map binding each [`ModelSelector`] tier to a concrete
/// [`LlmBackend`] (E9 S9.5).
///
/// The Striatal Gate (E5.2) chooses a [`CostClass`] per decision and the
/// Thalamic Router (E5.3) maps that onto a [`ModelSelector`].  Historically the
/// [`crate::LifecycleManager`] held a *single* `Arc<dyn LlmBackend>` and ignored
/// the tier; this map closes that gap so the cheap-local / mid-tier / frontier
/// tiers dispatch to the providers the operator actually bound (via the wizard
/// or `ANIMA_{CHEAP,MID,FRONTIER}_BACKEND`).
///
/// # Backward compatibility
///
/// Construct with [`TierBackends::uniform`] to point all three tiers at one
/// backend — behaviour then matches the legacy single-backend path exactly.
/// The map is installed *additively* on the lifecycle via
/// [`crate::LifecycleManager::with_tier_backends`]; when it is absent the
/// dispatch path is byte-for-byte unchanged.
#[cfg(feature = "std")]
#[derive(Clone)]
pub struct TierBackends {
    cheap_local: std::sync::Arc<dyn crate::LlmBackendRef>,
    mid_tier: std::sync::Arc<dyn crate::LlmBackendRef>,
    frontier: std::sync::Arc<dyn crate::LlmBackendRef>,
}

#[cfg(feature = "std")]
impl TierBackends {
    /// Build a tier map with an explicit backend per tier.
    pub fn new(
        cheap_local: std::sync::Arc<dyn crate::LlmBackendRef>,
        mid_tier: std::sync::Arc<dyn crate::LlmBackendRef>,
        frontier: std::sync::Arc<dyn crate::LlmBackendRef>,
    ) -> Self {
        Self {
            cheap_local,
            mid_tier,
            frontier,
        }
    }

    /// Build a tier map where every tier shares one backend.
    ///
    /// This is the backward-compatible default: a [`crate::LifecycleManager`]
    /// configured with `TierBackends::uniform(backend)` dispatches identically
    /// to one configured with the single-backend constructor.
    pub fn uniform(backend: std::sync::Arc<dyn crate::LlmBackendRef>) -> Self {
        Self {
            cheap_local: std::sync::Arc::clone(&backend),
            mid_tier: std::sync::Arc::clone(&backend),
            frontier: backend,
        }
    }

    /// Return the backend bound to `selector`.
    pub fn backend_for(
        &self,
        selector: ModelSelector,
    ) -> &std::sync::Arc<dyn crate::LlmBackendRef> {
        match selector {
            ModelSelector::CheapLocal => &self.cheap_local,
            ModelSelector::MidTier => &self.mid_tier,
            ModelSelector::Frontier => &self.frontier,
        }
    }

    /// Return the backend bound to the tier the gate's [`CostClass`] selects.
    ///
    /// This mirrors the gate→router→backend handshake: a `CheapLocal` decision
    /// dispatches to the cheap backend, `Frontier` to the frontier backend, etc.
    pub fn backend_for_cost_class(
        &self,
        cost_class: CostClass,
    ) -> &std::sync::Arc<dyn crate::LlmBackendRef> {
        self.backend_for(model_selector_for_cost_class(cost_class))
    }
}

#[cfg(feature = "std")]
impl std::fmt::Debug for TierBackends {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TierBackends")
            .field("cheap_local", &self.cheap_local.id())
            .field("mid_tier", &self.mid_tier.id())
            .field("frontier", &self.frontier.id())
            .finish()
    }
}

/// Map a gate [`CostClass`] onto the router's [`ModelSelector`] tier.
///
/// This is the canonical 1:1 correspondence used throughout E5.2/E5.3: the gate
/// decides the cost tier and the router selects the model of the same tier.
pub fn model_selector_for_cost_class(cost_class: CostClass) -> ModelSelector {
    match cost_class {
        CostClass::CheapLocal => ModelSelector::CheapLocal,
        CostClass::MidTier => ModelSelector::MidTier,
        CostClass::Frontier => ModelSelector::Frontier,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helper fixtures ───────────────────────────────────────────────────────

    /// All three built-in tools as ToolSpec structs.
    fn all_builtin_tools() -> Vec<ToolSpec> {
        vec![
            ToolSpec {
                name: "clock".into(),
                description: "Wall-clock time".into(),
            },
            ToolSpec {
                name: "echo".into(),
                description: "Echo payload".into(),
            },
            ToolSpec {
                name: "text-io".into(),
                description: "Text IO".into(),
            },
        ]
    }

    /// A ToolSpec list that includes a non-standard tool.
    fn extended_tools() -> Vec<ToolSpec> {
        let mut tools = all_builtin_tools();
        tools.push(ToolSpec {
            name: "search".into(),
            description: "Web search".into(),
        });
        tools
    }

    // ── S5.3.2 — Default routes ────────────────────────────────────────────────

    #[test]
    fn static_router_with_defaults_succeeds() {
        let router = StaticRouter::with_defaults();
        assert!(
            router.is_ok(),
            "StaticRouter::with_defaults() must succeed: {:?}",
            router.err()
        );
    }

    #[test]
    fn default_cheap_local_route_has_correct_id() {
        let router = StaticRouter::with_defaults().unwrap();
        assert_eq!(router.cheap_local_route().id.as_str(), RouteId::CHEAP_LOCAL);
    }

    #[test]
    fn default_mid_tier_route_has_correct_id() {
        let router = StaticRouter::with_defaults().unwrap();
        assert_eq!(router.mid_tier_route().id.as_str(), RouteId::MID_TIER);
    }

    #[test]
    fn default_frontier_route_has_correct_id() {
        let router = StaticRouter::with_defaults().unwrap();
        assert_eq!(router.frontier_route().id.as_str(), RouteId::FRONTIER);
    }

    // ── S5.3.2 — Cost class maps to correct route ─────────────────────────────

    #[test]
    fn cost_class_cheap_local_resolves_to_cheap_local_route() {
        let router = StaticRouter::with_defaults().unwrap();
        let route = router.resolve(SemanticClass::UserQuery, CostClass::CheapLocal);
        assert_eq!(route.id.as_str(), RouteId::CHEAP_LOCAL);
    }

    #[test]
    fn cost_class_mid_tier_resolves_to_mid_tier_route() {
        let router = StaticRouter::with_defaults().unwrap();
        let route = router.resolve(SemanticClass::SystemEvent, CostClass::MidTier);
        assert_eq!(route.id.as_str(), RouteId::MID_TIER);
    }

    #[test]
    fn cost_class_frontier_resolves_to_frontier_route() {
        let router = StaticRouter::with_defaults().unwrap();
        let route = router.resolve(SemanticClass::OperatorCommand, CostClass::Frontier);
        assert_eq!(route.id.as_str(), RouteId::FRONTIER);
    }

    // ── E5.3 exit criterion 1 — each baseline route exercises correct tool subset

    /// Cheap-local route: cortex sees only `clock` and `echo`, not `text-io`.
    #[test]
    fn cheap_local_route_permits_only_clock_and_echo() {
        let router = StaticRouter::with_defaults().unwrap();
        let route = router.cheap_local_route();
        let request = build_routed_request(
            "task-cl",
            "",
            "Test cheap-local tool scoping",
            route,
            &all_builtin_tools(),
            serde_json::json!({}),
        );

        let tool_names: Vec<&str> = request.tools.iter().map(|t| t.name.as_str()).collect();
        assert!(
            tool_names.contains(&"clock"),
            "cheap-local must include clock"
        );
        assert!(
            tool_names.contains(&"echo"),
            "cheap-local must include echo"
        );
        assert!(
            !tool_names.contains(&"text-io"),
            "cheap-local must NOT include text-io, got: {tool_names:?}"
        );
        assert_eq!(
            tool_names.len(),
            2,
            "cheap-local route must have exactly 2 tools"
        );
    }

    /// Mid-tier route: cortex sees `clock`, `echo`, AND `text-io`.
    #[test]
    fn mid_tier_route_permits_clock_echo_and_text_io() {
        let router = StaticRouter::with_defaults().unwrap();
        let route = router.mid_tier_route();
        let request = build_routed_request(
            "task-mt",
            "",
            "Test mid-tier tool scoping",
            route,
            &all_builtin_tools(),
            serde_json::json!({}),
        );

        let tool_names: Vec<&str> = request.tools.iter().map(|t| t.name.as_str()).collect();
        assert!(tool_names.contains(&"clock"), "mid-tier must include clock");
        assert!(tool_names.contains(&"echo"), "mid-tier must include echo");
        assert!(
            tool_names.contains(&"text-io"),
            "mid-tier must include text-io"
        );
        assert_eq!(
            tool_names.len(),
            3,
            "mid-tier route must have exactly 3 tools"
        );
    }

    /// Frontier route: cortex sees all three built-in tools.
    #[test]
    fn frontier_route_permits_all_builtin_tools() {
        let router = StaticRouter::with_defaults().unwrap();
        let route = router.frontier_route();
        let request = build_routed_request(
            "task-fr",
            "",
            "Test frontier tool scoping",
            route,
            &all_builtin_tools(),
            serde_json::json!({}),
        );

        let tool_names: Vec<&str> = request.tools.iter().map(|t| t.name.as_str()).collect();
        assert!(tool_names.contains(&"clock"));
        assert!(tool_names.contains(&"echo"));
        assert!(tool_names.contains(&"text-io"));
        assert_eq!(
            tool_names.len(),
            3,
            "frontier route must have exactly 3 tools"
        );
    }

    /// Non-route tools are stripped even when offered in the available set.
    #[test]
    fn router_strips_out_of_scope_tools_from_extended_set() {
        let router = StaticRouter::with_defaults().unwrap();
        // cheap-local only allows clock + echo
        let route = router.cheap_local_route();
        let request = build_routed_request(
            "task-strip",
            "",
            "Test tool stripping",
            route,
            &extended_tools(), // includes "search" which is not in scope
            serde_json::json!({}),
        );

        let tool_names: Vec<&str> = request.tools.iter().map(|t| t.name.as_str()).collect();
        assert!(
            !tool_names.contains(&"search"),
            "out-of-scope tools must be stripped: got {tool_names:?}"
        );
        assert_eq!(tool_names.len(), 2);
    }

    // ── E5.3 exit criterion 1 — memory scope is correctly set ────────────────

    /// Cheap-local memory scope: identity + L1, no L2 or L3.
    #[test]
    fn cheap_local_route_has_minimal_memory_scope() {
        let router = StaticRouter::with_defaults().unwrap();
        let route = router.cheap_local_route();
        let request = build_routed_request(
            "task-ms-cl",
            "",
            "Memory scope test",
            route,
            &all_builtin_tools(),
            serde_json::json!({}),
        );

        let scope = request.memory_scope.expect("memory_scope must be set");
        assert!(scope.identity, "cheap-local must have identity memory");
        assert!(scope.l1, "cheap-local must have L1 access");
        assert!(!scope.l2, "cheap-local must NOT have L2 access");
        assert!(!scope.l3, "cheap-local must NOT have L3 access");
    }

    /// Mid-tier memory scope: identity + L1 + L2, no L3.
    #[test]
    fn mid_tier_route_has_mid_memory_scope() {
        let router = StaticRouter::with_defaults().unwrap();
        let route = router.mid_tier_route();
        let request = build_routed_request(
            "task-ms-mt",
            "",
            "Memory scope test",
            route,
            &all_builtin_tools(),
            serde_json::json!({}),
        );

        let scope = request.memory_scope.expect("memory_scope must be set");
        assert!(scope.identity, "mid-tier must have identity memory");
        assert!(scope.l1, "mid-tier must have L1 access");
        assert!(scope.l2, "mid-tier must have L2 access");
        assert!(!scope.l3, "mid-tier must NOT have L3 access");
    }

    /// Frontier memory scope: all tiers.
    #[test]
    fn frontier_route_has_full_memory_scope() {
        let router = StaticRouter::with_defaults().unwrap();
        let route = router.frontier_route();
        let request = build_routed_request(
            "task-ms-fr",
            "",
            "Memory scope test",
            route,
            &all_builtin_tools(),
            serde_json::json!({}),
        );

        let scope = request.memory_scope.expect("memory_scope must be set");
        assert!(scope.identity, "frontier must have identity memory");
        assert!(scope.l1, "frontier must have L1 access");
        assert!(scope.l2, "frontier must have L2 access");
        assert!(scope.l3, "frontier must have L3 access");
    }

    // ── S5.3.4 — Identity memory always included ──────────────────────────────

    #[test]
    fn identity_memory_is_present_in_every_baseline_route_request() {
        let router = StaticRouter::with_defaults().unwrap();
        let identity = serde_json::json!({"user": {"name": "Alice"}});

        for (label, route) in [
            ("cheap-local", router.cheap_local_route()),
            ("mid-tier", router.mid_tier_route()),
            ("frontier", router.frontier_route()),
        ] {
            let request = build_routed_request(
                format!("task-id-{label}"),
                "",
                "Identity test",
                route,
                &all_builtin_tools(),
                identity.clone(),
            );

            // Identity JSON must be present and non-null.
            assert!(
                !request.identity.is_null(),
                "route '{label}' must carry identity memory"
            );
            let scope = request.memory_scope.as_ref().expect("scope must be set");
            assert!(
                scope.identity,
                "route '{label}' memory_scope.identity must be true"
            );
        }
    }

    // ── Termination policy ────────────────────────────────────────────────────

    #[test]
    fn cheap_local_route_has_tight_termination_policy() {
        let router = StaticRouter::with_defaults().unwrap();
        let route = router.cheap_local_route();
        let request = build_routed_request(
            "task-tp-cl",
            "",
            "Termination policy test",
            route,
            &all_builtin_tools(),
            serde_json::json!({}),
        );

        assert_eq!(request.max_turns, Some(3));
        assert_eq!(request.max_tool_calls, Some(3));
    }

    #[test]
    fn frontier_route_has_generous_termination_policy() {
        let router = StaticRouter::with_defaults().unwrap();
        let route = router.frontier_route();
        let request = build_routed_request(
            "task-tp-fr",
            "",
            "Termination policy test",
            route,
            &all_builtin_tools(),
            serde_json::json!({}),
        );

        assert_eq!(request.max_turns, Some(20));
        assert_eq!(request.max_tool_calls, Some(20));
    }

    // ── E5.3 exit criterion 2 — misconfiguration rejected at startup ──────────

    /// A route with empty RouteId is rejected.
    #[test]
    fn route_with_empty_id_is_rejected_at_construction() {
        let (_, mt, fr) = default_routes();
        let bad_cl = Route {
            id: RouteId::new(""), // empty
            model: ModelSelector::CheapLocal,
            tool_scope: ToolScope::new("core", vec!["clock".into()]),
            memory_scope: MemoryScope::minimal(),
            prompt_scaffold: PromptScaffold::empty(),
            termination: TerminationPolicy::cheap_local(),
        };
        let result = StaticRouter::new(bad_cl, mt, fr, &[]);
        assert_eq!(result.unwrap_err(), RouteError::EmptyRouteId);
    }

    /// A route with `identity: false` violates S5.3.4 and is rejected.
    #[test]
    fn route_with_identity_disabled_is_rejected_at_construction() {
        let (cl, mt, _) = default_routes();
        let bad_fr = Route {
            id: RouteId::new("bad-frontier"),
            model: ModelSelector::Frontier,
            tool_scope: ToolScope::new("full", vec!["clock".into()]),
            memory_scope: MemoryScope {
                identity: false, // violates S5.3.4
                l1: true,
                l2: true,
                l3: true,
                kv_controller: false,
            },
            prompt_scaffold: PromptScaffold::empty(),
            termination: TerminationPolicy::frontier(),
        };
        let result = StaticRouter::new(cl, mt, bad_fr, &[]);
        assert_eq!(
            result.unwrap_err(),
            RouteError::IdentityMemoryDisabled {
                route_id: "bad-frontier".into()
            }
        );
    }

    /// A route with an empty tool name is rejected.
    #[test]
    fn route_with_empty_tool_name_is_rejected_at_construction() {
        let (cl, _, fr) = default_routes();
        let bad_mt = Route {
            id: RouteId::new("bad-mid"),
            model: ModelSelector::MidTier,
            tool_scope: ToolScope::new("bad", vec!["clock".into(), "".into()]), // empty name
            memory_scope: MemoryScope::mid(),
            prompt_scaffold: PromptScaffold::empty(),
            termination: TerminationPolicy::mid_tier(),
        };
        let result = StaticRouter::new(cl, bad_mt, fr, &[]);
        assert_eq!(
            result.unwrap_err(),
            RouteError::EmptyToolName {
                route_id: "bad-mid".into(),
                index: 1,
            }
        );
    }

    /// A route with max_turns == 0 is rejected.
    #[test]
    fn route_with_zero_max_turns_is_rejected_at_construction() {
        let (_, mt, fr) = default_routes();
        let bad_cl = Route {
            id: RouteId::new("zero-turns"),
            model: ModelSelector::CheapLocal,
            tool_scope: ToolScope::new("core", vec!["clock".into()]),
            memory_scope: MemoryScope::minimal(),
            prompt_scaffold: PromptScaffold::empty(),
            termination: TerminationPolicy {
                max_turns: 0,
                max_tool_calls: 3,
            }, // zero
        };
        let result = StaticRouter::new(bad_cl, mt, fr, &[]);
        assert_eq!(
            result.unwrap_err(),
            RouteError::ZeroMaxTurns {
                route_id: "zero-turns".into()
            }
        );
    }

    /// A route with max_tool_calls == 0 is rejected.
    #[test]
    fn route_with_zero_max_tool_calls_is_rejected_at_construction() {
        let (cl, _, fr) = default_routes();
        let bad_mt = Route {
            id: RouteId::new("zero-calls"),
            model: ModelSelector::MidTier,
            tool_scope: ToolScope::new("core", vec!["clock".into()]),
            memory_scope: MemoryScope::mid(),
            prompt_scaffold: PromptScaffold::empty(),
            termination: TerminationPolicy {
                max_turns: 8,
                max_tool_calls: 0,
            }, // zero
        };
        let result = StaticRouter::new(cl, bad_mt, fr, &[]);
        assert_eq!(
            result.unwrap_err(),
            RouteError::ZeroMaxToolCalls {
                route_id: "zero-calls".into()
            }
        );
    }

    /// A route referencing an unknown tool is rejected when known_tools is supplied.
    #[test]
    fn route_with_unknown_tool_is_rejected_when_known_tools_provided() {
        let (cl, mt, _) = default_routes();
        let bad_fr = Route {
            id: RouteId::new("unknown-tool-frontier"),
            model: ModelSelector::Frontier,
            tool_scope: ToolScope::new(
                "full",
                vec!["clock".into(), "ghost-tool".into()], // "ghost-tool" is not registered
            ),
            memory_scope: MemoryScope::full(),
            prompt_scaffold: PromptScaffold::empty(),
            termination: TerminationPolicy::frontier(),
        };
        let known_tools: Vec<String> = vec!["clock".into(), "echo".into(), "text-io".into()];
        let result = StaticRouter::new(cl, mt, bad_fr, &known_tools);
        assert_eq!(
            result.unwrap_err(),
            RouteError::UnknownTool {
                route_id: "unknown-tool-frontier".into(),
                tool_name: "ghost-tool".into(),
            }
        );
    }

    /// Without known_tools, unknown tool references are not rejected.
    #[test]
    fn route_with_unknown_tool_is_accepted_when_no_known_tools_provided() {
        let (cl, mt, _) = default_routes();
        let fr_with_custom = Route {
            id: RouteId::new("custom-frontier"),
            model: ModelSelector::Frontier,
            tool_scope: ToolScope::new("custom", vec!["custom-tool".into()]),
            memory_scope: MemoryScope::full(),
            prompt_scaffold: PromptScaffold::empty(),
            termination: TerminationPolicy::frontier(),
        };
        // Empty known_tools → no registry validation.
        let result = StaticRouter::new(cl, mt, fr_with_custom, &[]);
        assert!(
            result.is_ok(),
            "unknown tool must be accepted when known_tools is empty"
        );
    }

    // ── Prompt scaffold ───────────────────────────────────────────────────────

    #[test]
    fn prompt_scaffold_prefix_is_applied_to_description() {
        let router = StaticRouter::with_defaults().unwrap();
        let route = router.cheap_local_route();
        let request = build_routed_request(
            "task-scaffold",
            "",
            "hello world",
            route,
            &all_builtin_tools(),
            serde_json::json!({}),
        );
        assert!(
            request.description.starts_with("[cheap-local] "),
            "description must start with the cheap-local prefix, got: {:?}",
            request.description
        );
        assert!(
            request.description.contains("hello world"),
            "original description must be preserved after scaffold"
        );
    }

    // ── Route ID is carried into InvokeRequest ────────────────────────────────

    #[test]
    fn routed_request_carries_route_id() {
        let router = StaticRouter::with_defaults().unwrap();
        for (cost_class, expected_id) in [
            (CostClass::CheapLocal, RouteId::CHEAP_LOCAL),
            (CostClass::MidTier, RouteId::MID_TIER),
            (CostClass::Frontier, RouteId::FRONTIER),
        ] {
            let route = router.resolve(SemanticClass::UserQuery, cost_class);
            let request = build_routed_request(
                "task-id-check",
                "",
                "route id check",
                route,
                &all_builtin_tools(),
                serde_json::json!({}),
            );
            assert_eq!(
                request.route_id.as_deref(),
                Some(expected_id),
                "request must carry the route ID"
            );
        }
    }

    // ── Audit log integration ─────────────────────────────────────────────────

    #[test]
    fn record_router_decision_emits_audit_entry_with_all_fields() {
        use crate::AuditEntry;

        let router = StaticRouter::with_defaults().unwrap();
        let route = router.mid_tier_route();
        let all_tools = all_builtin_tools();
        let permitted = route.tool_scope.filter_tools(&all_tools);

        let mut audit = crate::AuditLog::new();
        record_router_decision(
            &mut audit,
            "test-agent",
            "evt-audit-router",
            route,
            all_tools.len(),
            permitted.len(),
        );

        assert_eq!(audit.len(), 1);
        match &audit.entries()[0] {
            AuditEntry::RouterDecision {
                agent_id,
                event_id,
                route_id,
                model_selector,
                tools_available,
                tools_permitted,
                memory_scope_identity,
                memory_scope_l1,
                memory_scope_l2,
                memory_scope_l3,
                max_turns,
                max_tool_calls,
                ..
            } => {
                assert_eq!(agent_id, "test-agent");
                assert_eq!(event_id, "evt-audit-router");
                assert_eq!(route_id, RouteId::MID_TIER);
                assert_eq!(model_selector, "mid-tier");
                assert_eq!(*tools_available, 3);
                assert_eq!(*tools_permitted, 3); // mid-tier permits all 3
                assert!(*memory_scope_identity);
                assert!(*memory_scope_l1);
                assert!(*memory_scope_l2, "mid-tier must have L2");
                assert!(!*memory_scope_l3, "mid-tier must NOT have L3");
                assert_eq!(*max_turns, 8);
                assert_eq!(*max_tool_calls, 8);
            }
            other => panic!("expected RouterDecision, got {other:?}"),
        }
    }

    // ── ToolScope helpers ─────────────────────────────────────────────────────

    #[test]
    fn tool_scope_allows_returns_true_for_permitted_tool() {
        let scope = ToolScope::new("test", vec!["clock".into(), "echo".into()]);
        assert!(scope.allows("clock"));
        assert!(scope.allows("echo"));
        assert!(!scope.allows("text-io"));
        assert!(!scope.allows(""));
    }

    #[test]
    fn tool_scope_filter_preserves_order_of_available_tools() {
        let scope = ToolScope::new("test", vec!["echo".into(), "clock".into()]);
        let available = vec![
            ToolSpec {
                name: "clock".into(),
                description: "c".into(),
            },
            ToolSpec {
                name: "echo".into(),
                description: "e".into(),
            },
            ToolSpec {
                name: "text-io".into(),
                description: "t".into(),
            },
        ];
        let filtered = scope.filter_tools(&available);
        // Order matches available[], not allowed_tools[]
        assert_eq!(filtered[0].name, "clock");
        assert_eq!(filtered[1].name, "echo");
        assert_eq!(filtered.len(), 2);
    }

    // ── PromptScaffold ────────────────────────────────────────────────────────

    #[test]
    fn empty_prompt_scaffold_returns_base_unchanged() {
        let scaffold = PromptScaffold::empty();
        assert_eq!(scaffold.apply("hello"), "hello");
    }

    #[test]
    fn prompt_scaffold_applies_prefix_and_suffix() {
        let scaffold = PromptScaffold {
            system_prefix: "PREFIX:".into(),
            system_suffix: ":SUFFIX".into(),
        };
        assert_eq!(scaffold.apply("body"), "PREFIX:body:SUFFIX");
    }

    // ── MemoryScope → InvokeMemoryScope conversion ───────────────────────────

    #[test]
    fn memory_scope_to_invoke_scope_round_trips_correctly() {
        let scope = MemoryScope::full();
        let invoke_scope = scope.to_invoke_scope();
        assert_eq!(invoke_scope.identity, scope.identity);
        assert_eq!(invoke_scope.l1, scope.l1);
        assert_eq!(invoke_scope.l2, scope.l2);
        assert_eq!(invoke_scope.l3, scope.l3);
    }

    // ── E5.7 Homeostatic modulation stress harness ────────────────────────────
    //
    // Exit criterion 1: "A reproducible stress harness drives each signal
    // across its full range and the resulting gate/router/controller behaviour
    // is logged and asserted against a behavioural specification."

    fn default_router() -> StaticRouter {
        StaticRouter::with_defaults().expect("default router should construct without error")
    }

    fn neutral_signals() -> HomeostaticSignals {
        HomeostaticSignals::neutral()
    }

    fn stressed_signals(financial: f32, power: f32, thermal: f32) -> HomeostaticSignals {
        HomeostaticSignals {
            thermal_load: thermal,
            compute_pressure: thermal,
            memory_pressure: 0.0,
            power_budget: power,
            financial_budget: financial,
            attention_demand: 0.0,
        }
    }

    // ── No-modulation baseline ────────────────────────────────────────────────

    #[test]
    fn neutral_signals_do_not_trigger_modulation_for_any_cost_class() {
        let router = default_router();
        let signals = neutral_signals();
        for cost in [
            CostClass::CheapLocal,
            CostClass::MidTier,
            CostClass::Frontier,
        ] {
            let decision = router.resolve_with_modulation(SemanticClass::UserQuery, cost, &signals);
            assert!(
                !decision.was_modulated,
                "neutral signals should never trigger modulation for {:?}",
                cost
            );
            assert_eq!(decision.effective_route.id, decision.requested_route.id);
            assert!(decision.modulation_reason.is_none());
        }
    }

    // ── Rule 1: severe financial depletion (< 0.20) ──────────────────────────

    #[test]
    fn severe_financial_depletion_forces_frontier_to_cheap_local() {
        let router = default_router();
        let signals = stressed_signals(0.10, 1.0, 0.0); // financial=0.10
        let decision =
            router.resolve_with_modulation(SemanticClass::UserQuery, CostClass::Frontier, &signals);
        assert!(
            decision.was_modulated,
            "frontier must be downgraded under severe financial pressure"
        );
        assert_eq!(decision.effective_route.id.as_str(), RouteId::CHEAP_LOCAL);
        assert!(decision.modulation_reason.is_some());
    }

    #[test]
    fn severe_financial_depletion_forces_mid_tier_to_cheap_local() {
        let router = default_router();
        let signals = stressed_signals(0.10, 1.0, 0.0);
        let decision =
            router.resolve_with_modulation(SemanticClass::UserQuery, CostClass::MidTier, &signals);
        assert!(decision.was_modulated);
        assert_eq!(decision.effective_route.id.as_str(), RouteId::CHEAP_LOCAL);
    }

    #[test]
    fn severe_financial_depletion_does_not_double_downgrade_cheap_local() {
        let router = default_router();
        let signals = stressed_signals(0.10, 1.0, 0.0);
        let decision = router.resolve_with_modulation(
            SemanticClass::UserQuery,
            CostClass::CheapLocal,
            &signals,
        );
        // Already cheap-local — effective == requested (was_modulated = false).
        assert!(
            !decision.was_modulated,
            "cheap-local under severe pressure should not report was_modulated"
        );
        assert_eq!(decision.effective_route.id.as_str(), RouteId::CHEAP_LOCAL);
    }

    // ── Rule 1: severe power depletion (< 0.20) ──────────────────────────────

    #[test]
    fn severe_power_depletion_forces_frontier_to_cheap_local() {
        let router = default_router();
        let signals = stressed_signals(1.0, 0.10, 0.0); // power=0.10
        let decision =
            router.resolve_with_modulation(SemanticClass::UserQuery, CostClass::Frontier, &signals);
        assert!(
            decision.was_modulated,
            "frontier must be downgraded under severe power depletion"
        );
        assert_eq!(decision.effective_route.id.as_str(), RouteId::CHEAP_LOCAL);
    }

    // ── Rule 2: moderate financial pressure (0.20..0.40) ─────────────────────

    #[test]
    fn moderate_financial_pressure_downgraded_frontier_to_mid_tier() {
        let router = default_router();
        let signals = stressed_signals(0.30, 1.0, 0.0); // financial=0.30
        let decision =
            router.resolve_with_modulation(SemanticClass::UserQuery, CostClass::Frontier, &signals);
        assert!(
            decision.was_modulated,
            "frontier must be downgraded under moderate financial pressure"
        );
        assert_eq!(decision.effective_route.id.as_str(), RouteId::MID_TIER);
    }

    #[test]
    fn moderate_financial_pressure_does_not_affect_mid_tier_or_cheap_local() {
        let router = default_router();
        let signals = stressed_signals(0.30, 1.0, 0.0);
        for cost in [CostClass::MidTier, CostClass::CheapLocal] {
            let decision = router.resolve_with_modulation(SemanticClass::UserQuery, cost, &signals);
            assert!(
                !decision.was_modulated,
                "moderate financial pressure should not affect {:?}",
                cost
            );
        }
    }

    // ── Rule 2: moderate power pressure (0.20..0.40) ─────────────────────────

    #[test]
    fn moderate_power_pressure_downgrades_frontier_to_mid_tier() {
        let router = default_router();
        let signals = stressed_signals(1.0, 0.30, 0.0); // power=0.30
        let decision =
            router.resolve_with_modulation(SemanticClass::UserQuery, CostClass::Frontier, &signals);
        assert!(decision.was_modulated);
        assert_eq!(decision.effective_route.id.as_str(), RouteId::MID_TIER);
    }

    // ── Rule 3: high thermal load (> 0.80) ───────────────────────────────────

    #[test]
    fn high_thermal_load_downgrades_frontier_to_mid_tier() {
        let router = default_router();
        let signals = stressed_signals(1.0, 1.0, 0.85); // thermal=0.85
        let decision =
            router.resolve_with_modulation(SemanticClass::UserQuery, CostClass::Frontier, &signals);
        assert!(
            decision.was_modulated,
            "thermal stress must downgrade frontier"
        );
        assert_eq!(decision.effective_route.id.as_str(), RouteId::MID_TIER);
        let reason = decision.modulation_reason.unwrap();
        assert!(
            reason.contains("thermal"),
            "reason should mention thermal: {reason}"
        );
    }

    #[test]
    fn thermal_at_threshold_boundary_does_not_trigger_modulation() {
        let router = default_router();
        // 0.80 is the boundary — rule requires strictly > 0.80
        let signals = stressed_signals(1.0, 1.0, 0.80);
        let decision =
            router.resolve_with_modulation(SemanticClass::UserQuery, CostClass::Frontier, &signals);
        assert!(
            !decision.was_modulated,
            "thermal_load == 0.80 should not trigger rule 3 (strictly > 0.80 required)"
        );
    }

    #[test]
    fn high_thermal_does_not_affect_mid_tier_or_cheap_local() {
        let router = default_router();
        let signals = stressed_signals(1.0, 1.0, 0.90);
        for cost in [CostClass::MidTier, CostClass::CheapLocal] {
            let decision = router.resolve_with_modulation(SemanticClass::UserQuery, cost, &signals);
            assert!(
                !decision.was_modulated,
                "thermal stress should not affect {:?}",
                cost
            );
        }
    }

    // ── Full-range stress harness (exit criterion 1) ──────────────────────────

    #[test]
    fn stress_harness_sweeps_financial_budget_across_full_range() {
        let router = default_router();
        // Sweep financial_budget from 0.0 to 1.0 in 11 steps and verify
        // the modulation rules engage and disengage at the documented boundaries.
        let samples: Vec<f32> = (0..=10).map(|i| i as f32 / 10.0).collect();
        for &fb in &samples {
            let signals = stressed_signals(fb, 1.0, 0.0);
            let decision = router.resolve_with_modulation(
                SemanticClass::UserQuery,
                CostClass::Frontier,
                &signals,
            );
            if fb < 0.20 {
                assert_eq!(
                    decision.effective_route.id.as_str(),
                    RouteId::CHEAP_LOCAL,
                    "financial_budget={fb:.1}: expected cheap-local (severe)"
                );
            } else if fb < 0.40 {
                assert_eq!(
                    decision.effective_route.id.as_str(),
                    RouteId::MID_TIER,
                    "financial_budget={fb:.1}: expected mid-tier (moderate)"
                );
            } else {
                assert_eq!(
                    decision.effective_route.id.as_str(),
                    RouteId::FRONTIER,
                    "financial_budget={fb:.1}: expected frontier (no modulation)"
                );
            }
        }
    }

    #[test]
    fn stress_harness_sweeps_power_budget_across_full_range() {
        let router = default_router();
        let samples: Vec<f32> = (0..=10).map(|i| i as f32 / 10.0).collect();
        for &pb in &samples {
            let signals = stressed_signals(1.0, pb, 0.0);
            let decision = router.resolve_with_modulation(
                SemanticClass::UserQuery,
                CostClass::Frontier,
                &signals,
            );
            if pb < 0.20 {
                assert_eq!(
                    decision.effective_route.id.as_str(),
                    RouteId::CHEAP_LOCAL,
                    "power_budget={pb:.1}: expected cheap-local (severe)"
                );
            } else if pb < 0.40 {
                assert_eq!(
                    decision.effective_route.id.as_str(),
                    RouteId::MID_TIER,
                    "power_budget={pb:.1}: expected mid-tier (moderate)"
                );
            } else {
                assert_eq!(
                    decision.effective_route.id.as_str(),
                    RouteId::FRONTIER,
                    "power_budget={pb:.1}: expected frontier (no modulation)"
                );
            }
        }
    }

    #[test]
    fn stress_harness_sweeps_thermal_load_across_full_range() {
        let router = default_router();
        let samples: Vec<f32> = (0..=10).map(|i| i as f32 / 10.0).collect();
        for &tl in &samples {
            let signals = stressed_signals(1.0, 1.0, tl);
            let decision = router.resolve_with_modulation(
                SemanticClass::UserQuery,
                CostClass::Frontier,
                &signals,
            );
            if tl > 0.80 {
                assert_eq!(
                    decision.effective_route.id.as_str(),
                    RouteId::MID_TIER,
                    "thermal_load={tl:.1}: expected mid-tier (thermal stress)"
                );
                assert!(decision.was_modulated);
            } else {
                // No thermal modulation for Frontier at 0.80 or below.
                assert_eq!(
                    decision.effective_route.id.as_str(),
                    RouteId::FRONTIER,
                    "thermal_load={tl:.1}: expected frontier (no modulation)"
                );
            }
        }
    }

    // ── RouterModulated audit entry ───────────────────────────────────────────

    #[test]
    fn record_modulated_decision_emits_router_modulated_entry_when_modulated() {
        let router = default_router();
        let signals = stressed_signals(0.10, 1.0, 0.0); // severe financial
        let decision =
            router.resolve_with_modulation(SemanticClass::UserQuery, CostClass::Frontier, &signals);
        let mut audit = AuditLog::new();
        record_modulated_router_decision(&mut audit, "agent-1", "evt-1", &decision, 3, 2);

        // Expect RouterDecision + RouterModulated
        let modulated_entries: Vec<_> = audit
            .entries()
            .iter()
            .filter(|e| matches!(e, AuditEntry::RouterModulated { .. }))
            .collect();
        assert_eq!(
            modulated_entries.len(),
            1,
            "one RouterModulated entry expected"
        );
        if let AuditEntry::RouterModulated {
            requested_route_id,
            effective_route_id,
            ..
        } = modulated_entries[0]
        {
            assert_eq!(requested_route_id, RouteId::FRONTIER);
            assert_eq!(effective_route_id, RouteId::CHEAP_LOCAL);
        }
    }

    #[test]
    fn record_modulated_decision_does_not_emit_router_modulated_when_not_modulated() {
        let router = default_router();
        let signals = neutral_signals();
        let decision =
            router.resolve_with_modulation(SemanticClass::UserQuery, CostClass::MidTier, &signals);
        let mut audit = AuditLog::new();
        record_modulated_router_decision(&mut audit, "agent-1", "evt-1", &decision, 3, 3);

        let modulated_count = audit
            .entries()
            .iter()
            .filter(|e| matches!(e, AuditEntry::RouterModulated { .. }))
            .count();
        assert_eq!(
            modulated_count, 0,
            "no RouterModulated entry when signals are neutral"
        );
    }

    // ── E9 S9.5 — Per-tier backend map (TierBackends) ─────────────────────────

    /// Build a `TierBackends` whose three tiers carry distinguishable backend
    /// ids so selection can be asserted by `id()`.
    fn distinct_tier_backends() -> TierBackends {
        use scheduler::MockLlmBackend;
        TierBackends::new(
            std::sync::Arc::new(MockLlmBackend::with_id("cheap")),
            std::sync::Arc::new(MockLlmBackend::with_id("mid")),
            std::sync::Arc::new(MockLlmBackend::with_id("frontier")),
        )
    }

    #[test]
    fn model_selector_for_cost_class_is_one_to_one() {
        assert_eq!(
            model_selector_for_cost_class(CostClass::CheapLocal),
            ModelSelector::CheapLocal
        );
        assert_eq!(
            model_selector_for_cost_class(CostClass::MidTier),
            ModelSelector::MidTier
        );
        assert_eq!(
            model_selector_for_cost_class(CostClass::Frontier),
            ModelSelector::Frontier
        );
    }

    #[test]
    fn tier_backends_selects_distinct_backend_per_selector() {
        let tiers = distinct_tier_backends();
        assert_eq!(tiers.backend_for(ModelSelector::CheapLocal).id(), "cheap");
        assert_eq!(tiers.backend_for(ModelSelector::MidTier).id(), "mid");
        assert_eq!(tiers.backend_for(ModelSelector::Frontier).id(), "frontier");
    }

    #[test]
    fn cheap_local_cost_class_dispatches_to_cheap_backend() {
        let tiers = distinct_tier_backends();
        assert_eq!(
            tiers.backend_for_cost_class(CostClass::CheapLocal).id(),
            "cheap",
            "a CheapLocal decision must dispatch to the cheap backend"
        );
    }

    #[test]
    fn frontier_cost_class_dispatches_to_frontier_backend() {
        let tiers = distinct_tier_backends();
        assert_eq!(
            tiers.backend_for_cost_class(CostClass::Frontier).id(),
            "frontier",
            "a Frontier decision must dispatch to the frontier backend"
        );
    }

    #[test]
    fn mid_tier_cost_class_dispatches_to_mid_backend() {
        let tiers = distinct_tier_backends();
        assert_eq!(tiers.backend_for_cost_class(CostClass::MidTier).id(), "mid");
    }

    #[test]
    fn uniform_tier_backends_routes_every_tier_to_one_backend() {
        use scheduler::MockLlmBackend;
        let tiers = TierBackends::uniform(std::sync::Arc::new(MockLlmBackend::with_id("solo")));
        for cc in [
            CostClass::CheapLocal,
            CostClass::MidTier,
            CostClass::Frontier,
        ] {
            assert_eq!(
                tiers.backend_for_cost_class(cc).id(),
                "solo",
                "uniform map must route {cc:?} to the single backend"
            );
        }
    }

    #[test]
    fn tier_backends_debug_lists_backend_ids() {
        let tiers = distinct_tier_backends();
        let debug = format!("{tiers:?}");
        assert!(debug.contains("cheap"));
        assert!(debug.contains("mid"));
        assert!(debug.contains("frontier"));
    }
}
