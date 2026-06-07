//! E16 — Multi-Agent Coordination: A2A (Agent-to-Agent) substrate.
//!
//! Enables one agent to delegate a scoped sub-task to another named agent
//! over the Agent-to-Agent (A2A) bus defined in `praxis::Bus::A2a`.
//!
//! # Architecture
//!
//! ```text
//! parent cortex ──delegate call──► A2aDispatcher::dispatch("delegate", args)
//!                                        │
//!                                        ▼ look up target in AgentPool
//!                                        │  ├─ found → buffer AgentDelegated entry
//!                                        │  │          invoke endpoint
//!                                        │  │          buffer AgentDelegationCompleted
//!                                        │  └─ not found → buffer AgentDelegationFailed → Err
//!                                        │
//!  other tool calls ──────────────► inner ToolDispatcher (pass-through)
//! ```
//!
//! The `audit_buffer` is flushed to the main `AuditLog` by calling
//! [`A2aDispatcher::flush_audit`] after each cortex invocation, matching the
//! same pattern as [`crate::dispatch::EgressAwareDispatcher`].
//!
//! # Design notes
//!
//! * [`AgentEndpoint`] is synchronous and low-overhead — the canonical
//!   implementation for tests is [`MockAgentEndpoint`].
//! * [`AgentPool`] holds `Arc<dyn AgentEndpoint>` so multiple dispatchers can
//!   share the same pool without cloning the endpoint table.
//! * The unique `delegation_id` is a monotonic counter to correlate the
//!   `Delegated` / `Completed` / `Failed` audit pairs across log consumers.

#[cfg(feature = "std")]
use std::collections::HashMap;
#[cfg(feature = "std")]
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
#[cfg(feature = "std")]
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::audit::{AuditEntry, AuditLog};
use crate::cortex_bridge::ToolDispatcher;

// ── Protocol types ────────────────────────────────────────────────────────────

/// Request sent from a parent agent to a sub-agent over the A2A bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aRequest {
    /// Human-readable task description for the sub-agent.
    pub task: String,
    /// Optional context string passed to the sub-agent.
    #[serde(default)]
    pub context: String,
    /// Maximum planning + acting turns the sub-agent may use.
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
}

fn default_max_turns() -> u32 {
    4
}

/// Response returned by a sub-agent when its delegated task completes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aResponse {
    /// Short summary of what the sub-agent accomplished.
    pub summary: String,
    /// Number of tool calls the sub-agent made.
    pub tool_calls_made: usize,
    /// `true` when the sub-agent considers the task successful.
    pub success: bool,
    /// Wall-clock duration of the invocation in milliseconds.
    pub duration_ms: u64,
}

/// JSON payload expected by the `"delegate"` tool driver.
#[derive(Debug, Clone, Deserialize)]
pub struct DelegatePayload {
    /// Target agent identifier (must be registered in the [`AgentPool`]).
    pub agent: String,
    /// Human-readable task description for the sub-agent.
    pub task: String,
    /// Optional context passed to the sub-agent.
    #[serde(default)]
    pub context: String,
    /// Maximum planning + acting turns for the sub-agent.
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
}

// ── AgentEndpoint trait ───────────────────────────────────────────────────────

/// An agent reachable via the A2A bus.
///
/// Implementors must be `Send + Sync` so the pool can be shared across threads.
/// The canonical test double is [`MockAgentEndpoint`].
pub trait AgentEndpoint: Send + Sync {
    /// Stable identifier for this endpoint (matches the pool registry key).
    fn endpoint_id(&self) -> &str;
    /// Invoke the agent with the given request and return a result.
    fn invoke(&self, req: &A2aRequest) -> Result<A2aResponse, String>;
}

// ── MockAgentEndpoint ─────────────────────────────────────────────────────────

/// Deterministic, CI-hermetic agent endpoint for tests.
///
/// Returns a pre-configured summary and tool-call count without any I/O.
pub struct MockAgentEndpoint {
    id: String,
    summary: String,
    tool_calls: usize,
    should_fail: bool,
    fail_reason: String,
}

impl MockAgentEndpoint {
    /// Create a successful mock endpoint with the given identifier and response.
    pub fn new(id: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            summary: summary.into(),
            tool_calls: 1,
            should_fail: false,
            fail_reason: String::new(),
        }
    }

    /// Create a mock endpoint that always fails with the given reason.
    pub fn failing(id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            summary: String::new(),
            tool_calls: 0,
            should_fail: true,
            fail_reason: reason.into(),
        }
    }

    /// Set the number of tool calls the mock reports.
    pub fn with_tool_calls(mut self, n: usize) -> Self {
        self.tool_calls = n;
        self
    }
}

impl AgentEndpoint for MockAgentEndpoint {
    fn endpoint_id(&self) -> &str {
        &self.id
    }

    fn invoke(&self, req: &A2aRequest) -> Result<A2aResponse, String> {
        if self.should_fail {
            return Err(self.fail_reason.clone());
        }
        Ok(A2aResponse {
            summary: format!("{} (task: {})", self.summary, req.task),
            tool_calls_made: self.tool_calls,
            success: true,
            duration_ms: 0,
        })
    }
}

// ── AgentPool ─────────────────────────────────────────────────────────────────

/// Registry of named agent endpoints reachable via the A2A bus.
///
/// # Thread safety
///
/// `AgentPool` is typically wrapped in `Arc` and shared across dispatcher
/// instances.  The pool is write-once at initialisation time; there is no
/// runtime registration API that would require interior mutability.
#[cfg(feature = "std")]
pub struct AgentPool {
    agents: HashMap<String, Arc<dyn AgentEndpoint>>,
}

#[cfg(feature = "std")]
impl AgentPool {
    /// Create an empty pool.
    pub fn new() -> Self {
        Self {
            agents: HashMap::new(),
        }
    }

    /// Register an endpoint under its `endpoint_id`.
    ///
    /// Replaces any existing endpoint with the same identifier.
    pub fn register(&mut self, endpoint: impl AgentEndpoint + 'static) {
        let id = endpoint.endpoint_id().to_string();
        self.agents.insert(id, Arc::new(endpoint));
    }

    /// Look up an endpoint by identifier.
    pub fn get(&self, id: &str) -> Option<Arc<dyn AgentEndpoint>> {
        self.agents.get(id).cloned()
    }

    /// Number of endpoints registered in the pool.
    pub fn len(&self) -> usize {
        self.agents.len()
    }

    /// `true` when the pool contains no endpoints.
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }

    /// Return a sorted list of registered endpoint identifiers.
    pub fn list(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.agents.keys().cloned().collect();
        ids.sort();
        ids
    }
}

#[cfg(feature = "std")]
impl Default for AgentPool {
    fn default() -> Self {
        Self::new()
    }
}

// ── A2aDispatcher ─────────────────────────────────────────────────────────────

/// Counter for generating unique delegation identifiers.
#[cfg(feature = "std")]
static DELEGATION_COUNTER: AtomicU64 = AtomicU64::new(1);

#[cfg(feature = "std")]
fn next_delegation_id() -> String {
    format!("dlg-{}", DELEGATION_COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Wraps any [`ToolDispatcher`] and intercepts `"delegate"` calls, routing
/// them through the [`AgentPool`] and recording the delegation in the audit
/// buffer.
///
/// All other tool calls are passed through unchanged to the inner dispatcher.
///
/// # Audit pattern
///
/// Matches [`crate::dispatch::EgressAwareDispatcher`]: audit entries accumulate
/// in an `Arc<Mutex<Vec<AuditEntry>>>` during dispatch and are flushed to the
/// main [`AuditLog`] by calling [`A2aDispatcher::flush_audit`] after each
/// cortex invocation.
#[cfg(feature = "std")]
pub struct A2aDispatcher<D: ToolDispatcher> {
    /// Inner dispatcher for non-delegation tool calls.
    pub inner: D,
    /// Pool of agent endpoints.
    pool: Arc<AgentPool>,
    /// Identifier of the calling (parent) agent.
    parent_agent_id: String,
    /// Buffer of audit entries accumulated during this invocation.
    pub audit_buffer: Arc<Mutex<Vec<AuditEntry>>>,
}

#[cfg(feature = "std")]
impl<D: ToolDispatcher> A2aDispatcher<D> {
    /// Create a new dispatcher.
    pub fn new(inner: D, pool: Arc<AgentPool>, parent_agent_id: impl Into<String>) -> Self {
        Self {
            inner,
            pool,
            parent_agent_id: parent_agent_id.into(),
            audit_buffer: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Flush accumulated audit entries into `log` and clear the buffer.
    pub fn flush_audit(&self, log: &mut AuditLog) {
        let mut buf = self.audit_buffer.lock().unwrap_or_else(|e| e.into_inner());
        for entry in buf.drain(..) {
            log.push(entry);
        }
    }

    fn push_entry(&self, entry: AuditEntry) {
        let mut buf = self.audit_buffer.lock().unwrap_or_else(|e| e.into_inner());
        buf.push(entry);
    }
}

#[cfg(feature = "std")]
impl<D: ToolDispatcher> ToolDispatcher for A2aDispatcher<D> {
    fn dispatch(&self, tool_name: &str, args: &str) -> Result<String, String> {
        if tool_name != "delegate" {
            return self.inner.dispatch(tool_name, args);
        }

        // Parse the delegation payload.
        let payload: DelegatePayload = serde_json::from_str(args)
            .map_err(|e| format!("delegate: invalid JSON payload: {e}"))?;

        let delegation_id = next_delegation_id();
        let target_id = payload.agent.clone();

        // Record the delegation attempt.
        self.push_entry(AuditEntry::AgentDelegated {
            parent_agent_id: self.parent_agent_id.clone(),
            target_agent_id: target_id.clone(),
            delegation_id: delegation_id.clone(),
            task: payload.task.clone(),
        });

        // Resolve the target endpoint.
        let endpoint = match self.pool.get(&payload.agent) {
            Some(ep) => ep,
            None => {
                let reason = format!("unknown agent: {}", payload.agent);
                self.push_entry(AuditEntry::AgentDelegationFailed {
                    parent_agent_id: self.parent_agent_id.clone(),
                    target_agent_id: target_id,
                    delegation_id,
                    reason: reason.clone(),
                });
                return Err(reason);
            }
        };

        // Invoke the endpoint and measure wall-clock duration.
        let req = A2aRequest {
            task: payload.task,
            context: payload.context,
            max_turns: payload.max_turns,
        };
        let t0 = Instant::now();
        let invoke_result = endpoint.invoke(&req);
        let duration_ms = t0.elapsed().as_millis() as u64;

        match invoke_result {
            Ok(resp) => {
                self.push_entry(AuditEntry::AgentDelegationCompleted {
                    parent_agent_id: self.parent_agent_id.clone(),
                    target_agent_id: target_id,
                    delegation_id,
                    success: resp.success,
                    tool_calls_made: resp.tool_calls_made,
                    duration_ms,
                    summary: resp.summary.clone(),
                });
                serde_json::to_string(&resp).map_err(|e| e.to_string())
            }
            Err(err) => {
                self.push_entry(AuditEntry::AgentDelegationFailed {
                    parent_agent_id: self.parent_agent_id.clone(),
                    target_agent_id: target_id,
                    delegation_id,
                    reason: err.clone(),
                });
                Err(err)
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use crate::cortex_bridge::FnDispatcher;

    fn passthrough() -> FnDispatcher<impl Fn(&str, &str) -> Result<String, String>> {
        FnDispatcher(|_name: &str, _args: &str| Ok("pass-through".to_string()))
    }

    fn pool_with_mock() -> Arc<AgentPool> {
        let mut pool = AgentPool::new();
        pool.register(MockAgentEndpoint::new("summarizer", "Summary done.").with_tool_calls(2));
        Arc::new(pool)
    }

    // ── AgentPool ──

    #[test]
    fn agent_pool_register_and_lookup() {
        let mut pool = AgentPool::new();
        assert!(pool.is_empty());
        pool.register(MockAgentEndpoint::new("agent-a", "ok"));
        assert_eq!(pool.len(), 1);
        assert!(pool.get("agent-a").is_some());
        assert!(pool.get("agent-b").is_none());
    }

    #[test]
    fn agent_pool_list_is_sorted() {
        let mut pool = AgentPool::new();
        pool.register(MockAgentEndpoint::new("z-agent", "ok"));
        pool.register(MockAgentEndpoint::new("a-agent", "ok"));
        pool.register(MockAgentEndpoint::new("m-agent", "ok"));
        assert_eq!(pool.list(), vec!["a-agent", "m-agent", "z-agent"]);
    }

    #[test]
    fn agent_pool_register_overwrites_existing_id() {
        let mut pool = AgentPool::new();
        pool.register(MockAgentEndpoint::new("agent", "first"));
        pool.register(MockAgentEndpoint::new("agent", "second"));
        assert_eq!(pool.len(), 1);
        let ep = pool.get("agent").unwrap();
        let req = A2aRequest {
            task: "t".into(),
            context: String::new(),
            max_turns: 1,
        };
        let resp = ep.invoke(&req).unwrap();
        assert!(resp.summary.contains("second"));
    }

    // ── MockAgentEndpoint ──

    #[test]
    fn mock_endpoint_returns_configured_summary() {
        let ep = MockAgentEndpoint::new("ep", "hello").with_tool_calls(3);
        let req = A2aRequest {
            task: "do thing".into(),
            context: String::new(),
            max_turns: 4,
        };
        let resp = ep.invoke(&req).unwrap();
        assert!(resp.success);
        assert_eq!(resp.tool_calls_made, 3);
        assert!(resp.summary.contains("hello"));
        assert!(resp.summary.contains("do thing"));
    }

    #[test]
    fn mock_failing_endpoint_returns_error() {
        let ep = MockAgentEndpoint::failing("ep", "service unavailable");
        let req = A2aRequest {
            task: "t".into(),
            context: String::new(),
            max_turns: 1,
        };
        let err = ep.invoke(&req).unwrap_err();
        assert_eq!(err, "service unavailable");
    }

    // ── A2aDispatcher ──

    #[test]
    fn non_delegate_calls_pass_through_to_inner() {
        let dispatcher = A2aDispatcher::new(passthrough(), pool_with_mock(), "parent");
        let result = dispatcher.dispatch("clock", "{}");
        assert_eq!(result.unwrap(), "pass-through");
        assert!(dispatcher.audit_buffer.lock().unwrap().is_empty());
    }

    #[test]
    fn delegate_call_invokes_named_agent_and_returns_response() {
        let dispatcher = A2aDispatcher::new(passthrough(), pool_with_mock(), "parent");
        let result = dispatcher.dispatch(
            "delegate",
            r#"{"agent":"summarizer","task":"Summarize the notes"}"#,
        );
        assert!(result.is_ok(), "dispatch should succeed: {result:?}");
        let resp: A2aResponse = serde_json::from_str(&result.unwrap()).unwrap();
        assert!(resp.success);
        assert_eq!(resp.tool_calls_made, 2);
        assert!(resp.summary.contains("Summary done."));
    }

    #[test]
    fn delegate_to_known_agent_emits_delegated_and_completed_entries() {
        let dispatcher = A2aDispatcher::new(passthrough(), pool_with_mock(), "parent-x");
        dispatcher
            .dispatch("delegate", r#"{"agent":"summarizer","task":"t"}"#)
            .unwrap();

        let buf = dispatcher.audit_buffer.lock().unwrap();
        assert_eq!(
            buf.len(),
            2,
            "expected AgentDelegated + AgentDelegationCompleted"
        );
        assert!(
            matches!(&buf[0], AuditEntry::AgentDelegated { parent_agent_id, target_agent_id, .. }
                if parent_agent_id == "parent-x" && target_agent_id == "summarizer")
        );
        assert!(matches!(
            &buf[1],
            AuditEntry::AgentDelegationCompleted {
                success: true,
                tool_calls_made: 2,
                ..
            }
        ));
    }

    #[test]
    fn delegation_ids_are_stable_within_audit_pair() {
        let dispatcher = A2aDispatcher::new(passthrough(), pool_with_mock(), "parent");
        dispatcher
            .dispatch("delegate", r#"{"agent":"summarizer","task":"t"}"#)
            .unwrap();

        let buf = dispatcher.audit_buffer.lock().unwrap();
        let id_delegated = match &buf[0] {
            AuditEntry::AgentDelegated { delegation_id, .. } => delegation_id.clone(),
            _ => panic!("expected AgentDelegated"),
        };
        let id_completed = match &buf[1] {
            AuditEntry::AgentDelegationCompleted { delegation_id, .. } => delegation_id.clone(),
            _ => panic!("expected AgentDelegationCompleted"),
        };
        assert_eq!(
            id_delegated, id_completed,
            "IDs must match within an audit pair"
        );
    }

    #[test]
    fn delegation_to_unknown_agent_fails_and_emits_failed_entry() {
        let dispatcher = A2aDispatcher::new(passthrough(), pool_with_mock(), "parent");
        let result = dispatcher.dispatch("delegate", r#"{"agent":"nonexistent","task":"t"}"#);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown agent"));

        let buf = dispatcher.audit_buffer.lock().unwrap();
        assert_eq!(
            buf.len(),
            2,
            "expected AgentDelegated + AgentDelegationFailed"
        );
        assert!(matches!(&buf[0], AuditEntry::AgentDelegated { .. }));
        assert!(
            matches!(&buf[1], AuditEntry::AgentDelegationFailed { target_agent_id, .. }
                if target_agent_id == "nonexistent")
        );
    }

    #[test]
    fn delegation_to_failing_endpoint_emits_failed_entry() {
        let mut pool = AgentPool::new();
        pool.register(MockAgentEndpoint::failing("broken", "timeout"));
        let pool = Arc::new(pool);

        let dispatcher = A2aDispatcher::new(passthrough(), pool, "parent");
        let result = dispatcher.dispatch("delegate", r#"{"agent":"broken","task":"t"}"#);
        assert!(result.is_err());

        let buf = dispatcher.audit_buffer.lock().unwrap();
        assert_eq!(buf.len(), 2);
        assert!(
            matches!(&buf[1], AuditEntry::AgentDelegationFailed { reason, .. } if reason == "timeout")
        );
    }

    #[test]
    fn invalid_json_payload_returns_error_without_audit_entry() {
        let dispatcher = A2aDispatcher::new(passthrough(), pool_with_mock(), "parent");
        let result = dispatcher.dispatch("delegate", "not-json");
        assert!(result.is_err());
        // No AgentDelegated entry should be buffered before the parse error.
        assert!(dispatcher.audit_buffer.lock().unwrap().is_empty());
    }

    #[test]
    fn flush_audit_moves_entries_to_main_log_and_clears_buffer() {
        let dispatcher = A2aDispatcher::new(passthrough(), pool_with_mock(), "parent");
        dispatcher
            .dispatch("delegate", r#"{"agent":"summarizer","task":"t"}"#)
            .unwrap();

        let mut log = AuditLog::new();
        dispatcher.flush_audit(&mut log);

        assert_eq!(log.len(), 2);
        assert!(dispatcher.audit_buffer.lock().unwrap().is_empty());
    }

    #[test]
    fn multiple_delegation_calls_each_get_unique_ids() {
        let dispatcher = A2aDispatcher::new(passthrough(), pool_with_mock(), "parent");
        dispatcher
            .dispatch("delegate", r#"{"agent":"summarizer","task":"t1"}"#)
            .unwrap();
        dispatcher
            .dispatch("delegate", r#"{"agent":"summarizer","task":"t2"}"#)
            .unwrap();

        let buf = dispatcher.audit_buffer.lock().unwrap();
        assert_eq!(buf.len(), 4);
        let id1 = match &buf[0] {
            AuditEntry::AgentDelegated { delegation_id, .. } => delegation_id.clone(),
            _ => panic!(),
        };
        let id2 = match &buf[2] {
            AuditEntry::AgentDelegated { delegation_id, .. } => delegation_id.clone(),
            _ => panic!(),
        };
        assert_ne!(id1, id2, "each delegation must have a unique ID");
    }

    #[test]
    fn a2a_request_context_and_max_turns_defaults() {
        let dispatcher = A2aDispatcher::new(passthrough(), pool_with_mock(), "parent");
        // Payload without optional fields — defaults must kick in.
        let result =
            dispatcher.dispatch("delegate", r#"{"agent":"summarizer","task":"bare task"}"#);
        assert!(result.is_ok());
        // If defaults failed to parse, serde would have returned an error.
    }
}
