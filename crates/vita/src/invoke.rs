// crates/vita/src/invoke.rs
//! IPC envelope types shared by the router on every target.
//!
//! These are the plain-data invocation types ([`ToolSpec`],
//! [`InvokeMemoryScope`], [`InvokeRequest`]) that the Thalamic Router builds
//! and the cortex consumes.  They are pure `serde` data carriers, so they
//! compile on `no_std + alloc` targets; the bridge that actually ships them
//! over a Unix Domain Socket lives in the std-only
//! [`cortex_bridge`](crate::cortex_bridge) module.

#![forbid(unsafe_code)]

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

use serde::{Deserialize, Serialize};

// ── IPC types (shared between the real and mock bridges) ─────────────────────

/// Description of a tool exposed to the cortex.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Unique tool identifier (must match `ToolDriver::id`).
    pub name: String,
    /// Human-readable description shown to the planner.
    pub description: String,
}

/// Memory tier access scope serialised into the cortex invocation request (E5.3).
///
/// The cortex uses this to understand which memory tiers it may read/write.
/// Identity memory (`identity: true`) is always present on every baseline route
/// per S5.3.4.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvokeMemoryScope {
    /// Identity memory accessible (always `true` on baseline routes).
    pub identity: bool,
    /// L1 working memory accessible.
    pub l1: bool,
    /// L2 warm ARC cache accessible.
    pub l2: bool,
    /// L3 persistent archive accessible.
    pub l3: bool,
}

impl InvokeMemoryScope {
    /// Minimal scope: identity + L1 only.
    pub fn minimal() -> Self {
        Self {
            identity: true,
            l1: true,
            l2: false,
            l3: false,
        }
    }

    /// Mid scope: identity + L1 + L2.
    pub fn mid() -> Self {
        Self {
            identity: true,
            l1: true,
            l2: true,
            l3: false,
        }
    }

    /// Full scope: all tiers accessible.
    pub fn full() -> Self {
        Self {
            identity: true,
            l1: true,
            l2: true,
            l3: true,
        }
    }
}

/// Request sent from vita to the cortex at the start of each invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvokeRequest {
    /// Stable per-invocation identifier (used for audit correlation).
    pub task_id: String,
    /// Identifier of the agent issuing this request (used in defence audit entries).
    ///
    /// Defaults to an empty string for backward compatibility with pre-E5.6 callers
    /// that do not supply an agent identity.
    #[serde(default)]
    pub agent_id: String,
    /// Natural-language description of the task to be performed.
    pub description: String,
    /// Tool subset the cortex is permitted to call during this invocation.
    pub tools: Vec<ToolSpec>,
    /// Current identity-memory snapshot (JSON object).
    pub identity: serde_json::Value,

    // ── E5.3 Thalamic Router fields ───────────────────────────────────────────
    /// Route identifier selected by the Thalamic Router.
    ///
    /// `None` for pre-E5.3 requests; one of `"cheap-local"`, `"mid-tier"`, or
    /// `"frontier"` for routed requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_id: Option<String>,
    /// Memory tier access scope for this invocation.
    ///
    /// The cortex must not attempt to access tiers not included here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_scope: Option<InvokeMemoryScope>,
    /// Maximum planning + acting turns for this invocation.
    ///
    /// `None` means use the cortex's own default (`AgentLoop.MAX_TOOL_CALLS`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    /// Maximum total tool calls for this invocation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_calls: Option<u32>,
}
