//! Integration tests — E16 Multi-Agent Coordination (A2A substrate).
//!
//! Covers:
//! - S16.1: A2A protocol types (A2aRequest / A2aResponse / DelegatePayload).
//! - S16.2: AgentPool — registration, lookup, listing.
//! - S16.3: A2aDispatcher — delegate intercept, pass-through, audit buffering.
//! - S16.4: Audit integration — AgentDelegated / AgentDelegationCompleted /
//!          AgentDelegationFailed entries flushed into the main AuditLog.
//! - S16.5: End-to-end delegation chain with multiple sub-agents.
//!
//! All tests are fully hermetic (no network calls, no live LLM API keys).

use std::sync::Arc;

use vita::cortex_bridge::FnDispatcher;
use vita::cortex_bridge::ToolDispatcher;
use vita::{
    A2aDispatcher, A2aRequest, A2aResponse, AgentEndpoint, AgentPool, AuditEntry, AuditLog,
    MockAgentEndpoint,
};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn passthrough() -> FnDispatcher<impl Fn(&str, &str) -> Result<String, String>> {
    FnDispatcher(|_name: &str, _args: &str| Ok("pass-through".to_string()))
}

fn build_pool() -> Arc<AgentPool> {
    let mut pool = AgentPool::new();
    pool.register(
        MockAgentEndpoint::new("summarizer", "Documents summarized successfully.")
            .with_tool_calls(3),
    );
    pool.register(
        MockAgentEndpoint::new("researcher", "Research complete. Found 5 sources.")
            .with_tool_calls(5),
    );
    Arc::new(pool)
}

// ── S16.1 — Protocol types ────────────────────────────────────────────────────

#[test]
fn a2a_request_round_trips_through_json() {
    let req = A2aRequest {
        task: "Analyse the quarterly report".to_string(),
        context: "Focus on revenue trends".to_string(),
        max_turns: 6,
    };
    let json = serde_json::to_string(&req).unwrap();
    let decoded: A2aRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded.task, req.task);
    assert_eq!(decoded.context, req.context);
    assert_eq!(decoded.max_turns, req.max_turns);
}

#[test]
fn a2a_response_round_trips_through_json() {
    let resp = A2aResponse {
        summary: "Analysis complete.".to_string(),
        tool_calls_made: 4,
        success: true,
        duration_ms: 120,
    };
    let json = serde_json::to_string(&resp).unwrap();
    let decoded: A2aResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded.summary, resp.summary);
    assert_eq!(decoded.tool_calls_made, resp.tool_calls_made);
    assert!(decoded.success);
    assert_eq!(decoded.duration_ms, resp.duration_ms);
}

#[test]
fn delegate_payload_defaults_for_optional_fields() {
    // Minimal payload — context and max_turns use serde defaults.
    let json = r#"{"agent":"summarizer","task":"Summarize this"}"#;
    let payload: vita::DelegatePayload = serde_json::from_str(json).unwrap();
    assert_eq!(payload.agent, "summarizer");
    assert_eq!(payload.task, "Summarize this");
    assert_eq!(payload.context, "");
    assert_eq!(payload.max_turns, 4); // default_max_turns()
}

// ── S16.2 — AgentPool ─────────────────────────────────────────────────────────

#[test]
fn agent_pool_is_empty_on_creation() {
    let pool = AgentPool::new();
    assert!(pool.is_empty());
    assert_eq!(pool.len(), 0);
}

#[test]
fn agent_pool_register_and_lookup_endpoint() {
    let mut pool = AgentPool::new();
    pool.register(MockAgentEndpoint::new("my-agent", "done"));
    assert_eq!(pool.len(), 1);
    assert!(pool.get("my-agent").is_some());
    assert!(pool.get("missing").is_none());
}

#[test]
fn agent_pool_list_returns_sorted_ids() {
    let pool = build_pool();
    let ids = pool.list();
    assert_eq!(ids, vec!["researcher", "summarizer"]);
}

#[test]
fn mock_agent_endpoint_invokes_correctly() {
    let ep = MockAgentEndpoint::new("ep", "Task done").with_tool_calls(2);
    let req = A2aRequest {
        task: "do work".to_string(),
        context: String::new(),
        max_turns: 3,
    };
    let resp = ep.invoke(&req).unwrap();
    assert!(resp.success);
    assert_eq!(resp.tool_calls_made, 2);
    assert!(resp.summary.contains("Task done"));
    assert!(resp.summary.contains("do work"));
}

#[test]
fn failing_mock_endpoint_returns_error() {
    let ep = MockAgentEndpoint::failing("ep", "service down");
    let req = A2aRequest {
        task: "t".to_string(),
        context: String::new(),
        max_turns: 1,
    };
    let err = ep.invoke(&req).unwrap_err();
    assert_eq!(err, "service down");
}

// ── S16.3 — A2aDispatcher ────────────────────────────────────────────────────

#[test]
fn non_delegate_tool_calls_pass_through_unchanged() {
    let dispatcher = A2aDispatcher::new(passthrough(), build_pool(), "parent");
    let result = dispatcher.dispatch("clock", "{}");
    assert_eq!(result.unwrap(), "pass-through");
    assert!(
        dispatcher.audit_buffer.lock().unwrap().is_empty(),
        "non-delegate calls must not produce A2A audit entries"
    );
}

#[test]
fn delegate_call_to_summarizer_returns_json_response() {
    let dispatcher = A2aDispatcher::new(passthrough(), build_pool(), "parent");
    let result = dispatcher.dispatch(
        "delegate",
        r#"{"agent":"summarizer","task":"Summarize the meeting notes"}"#,
    );
    assert!(result.is_ok(), "dispatch should succeed: {result:?}");

    let resp: A2aResponse = serde_json::from_str(&result.unwrap()).unwrap();
    assert!(resp.success);
    assert_eq!(resp.tool_calls_made, 3);
    assert!(resp.summary.contains("Documents summarized successfully."));
}

#[test]
fn delegate_call_to_researcher_returns_json_response() {
    let dispatcher = A2aDispatcher::new(passthrough(), build_pool(), "parent");
    let result = dispatcher.dispatch(
        "delegate",
        r#"{"agent":"researcher","task":"Research market trends"}"#,
    );
    assert!(result.is_ok());
    let resp: A2aResponse = serde_json::from_str(&result.unwrap()).unwrap();
    assert_eq!(resp.tool_calls_made, 5);
}

// ── S16.4 — Audit integration ─────────────────────────────────────────────────

#[test]
fn successful_delegation_emits_delegated_and_completed_entries() {
    let dispatcher = A2aDispatcher::new(passthrough(), build_pool(), "agent-a");
    dispatcher
        .dispatch(
            "delegate",
            r#"{"agent":"summarizer","task":"Summarize docs"}"#,
        )
        .unwrap();

    let buf = dispatcher.audit_buffer.lock().unwrap();
    assert_eq!(
        buf.len(),
        2,
        "expected AgentDelegated + AgentDelegationCompleted"
    );
    assert!(
        matches!(&buf[0], AuditEntry::AgentDelegated {
            parent_agent_id, target_agent_id, task, ..
        } if parent_agent_id == "agent-a"
            && target_agent_id == "summarizer"
            && task == "Summarize docs"),
        "first entry should be AgentDelegated: {buf:?}"
    );
    assert!(
        matches!(&buf[1], AuditEntry::AgentDelegationCompleted {
            parent_agent_id, target_agent_id, success, tool_calls_made, ..
        } if parent_agent_id == "agent-a"
            && target_agent_id == "summarizer"
            && *success
            && *tool_calls_made == 3),
        "second entry should be AgentDelegationCompleted: {buf:?}"
    );
}

#[test]
fn delegation_ids_are_stable_and_match_across_pair() {
    let dispatcher = A2aDispatcher::new(passthrough(), build_pool(), "parent");
    dispatcher
        .dispatch("delegate", r#"{"agent":"summarizer","task":"t"}"#)
        .unwrap();

    let buf = dispatcher.audit_buffer.lock().unwrap();
    let id_start = match &buf[0] {
        AuditEntry::AgentDelegated { delegation_id, .. } => delegation_id.clone(),
        _ => panic!("expected AgentDelegated"),
    };
    let id_end = match &buf[1] {
        AuditEntry::AgentDelegationCompleted { delegation_id, .. } => delegation_id.clone(),
        _ => panic!("expected AgentDelegationCompleted"),
    };
    assert_eq!(
        id_start, id_end,
        "delegation_id must be stable within an audit pair"
    );
    assert!(
        id_start.starts_with("dlg-"),
        "delegation ID should have dlg- prefix"
    );
}

#[test]
fn delegation_to_unknown_agent_emits_delegated_then_failed() {
    let dispatcher = A2aDispatcher::new(passthrough(), build_pool(), "parent");
    let result = dispatcher.dispatch("delegate", r#"{"agent":"nonexistent","task":"t"}"#);
    assert!(result.is_err());

    let buf = dispatcher.audit_buffer.lock().unwrap();
    assert_eq!(
        buf.len(),
        2,
        "expected AgentDelegated + AgentDelegationFailed"
    );
    assert!(matches!(&buf[0], AuditEntry::AgentDelegated { .. }));
    assert!(matches!(
        &buf[1],
        AuditEntry::AgentDelegationFailed {
            target_agent_id,
            reason,
            ..
        } if target_agent_id == "nonexistent" && reason.contains("unknown agent")
    ));
}

#[test]
fn delegation_to_failing_endpoint_emits_delegated_then_failed() {
    let mut pool = AgentPool::new();
    pool.register(MockAgentEndpoint::failing("broken", "endpoint timeout"));
    let pool = Arc::new(pool);

    let dispatcher = A2aDispatcher::new(passthrough(), pool, "parent");
    let result = dispatcher.dispatch("delegate", r#"{"agent":"broken","task":"t"}"#);
    assert!(result.is_err());

    let buf = dispatcher.audit_buffer.lock().unwrap();
    assert_eq!(buf.len(), 2);
    assert!(matches!(&buf[0], AuditEntry::AgentDelegated { .. }));
    assert!(matches!(
        &buf[1],
        AuditEntry::AgentDelegationFailed { reason, .. }
        if reason == "endpoint timeout"
    ));
}

#[test]
fn invalid_json_payload_returns_error_and_no_audit_entries() {
    let dispatcher = A2aDispatcher::new(passthrough(), build_pool(), "parent");
    let result = dispatcher.dispatch("delegate", "not-valid-json");
    assert!(result.is_err());
    assert!(
        dispatcher.audit_buffer.lock().unwrap().is_empty(),
        "parse errors should not produce AgentDelegated entries"
    );
}

#[test]
fn flush_audit_moves_delegation_entries_to_main_log() {
    let dispatcher = A2aDispatcher::new(passthrough(), build_pool(), "parent");
    dispatcher
        .dispatch("delegate", r#"{"agent":"summarizer","task":"t"}"#)
        .unwrap();

    let mut log = AuditLog::new();
    dispatcher.flush_audit(&mut log);

    assert_eq!(log.len(), 2, "both entries should move to the main log");
    assert!(
        dispatcher.audit_buffer.lock().unwrap().is_empty(),
        "buffer should be empty after flush"
    );
}

// ── S16.5 — End-to-end delegation chain ─────────────────────────────────────

#[test]
fn parent_agent_delegates_to_multiple_sub_agents_sequentially() {
    let dispatcher = A2aDispatcher::new(passthrough(), build_pool(), "orchestrator");

    // First delegation: summarizer
    let r1 = dispatcher
        .dispatch(
            "delegate",
            r#"{"agent":"summarizer","task":"Summarize Q1 report"}"#,
        )
        .unwrap();
    let resp1: A2aResponse = serde_json::from_str(&r1).unwrap();
    assert!(resp1.success);

    // Second delegation: researcher
    let r2 = dispatcher
        .dispatch(
            "delegate",
            r#"{"agent":"researcher","task":"Research Q2 market data"}"#,
        )
        .unwrap();
    let resp2: A2aResponse = serde_json::from_str(&r2).unwrap();
    assert!(resp2.success);

    // Audit should contain 4 entries: 2 delegated + 2 completed
    let buf = dispatcher.audit_buffer.lock().unwrap();
    assert_eq!(
        buf.len(),
        4,
        "4 audit entries expected for 2 delegations: {buf:?}"
    );

    let delegated_count = buf
        .iter()
        .filter(|e| matches!(e, AuditEntry::AgentDelegated { .. }))
        .count();
    let completed_count = buf
        .iter()
        .filter(|e| matches!(e, AuditEntry::AgentDelegationCompleted { .. }))
        .count();
    assert_eq!(delegated_count, 2);
    assert_eq!(completed_count, 2);
}

#[test]
fn delegation_ids_are_unique_across_multiple_calls() {
    let dispatcher = A2aDispatcher::new(passthrough(), build_pool(), "parent");
    dispatcher
        .dispatch("delegate", r#"{"agent":"summarizer","task":"t1"}"#)
        .unwrap();
    dispatcher
        .dispatch("delegate", r#"{"agent":"researcher","task":"t2"}"#)
        .unwrap();

    let buf = dispatcher.audit_buffer.lock().unwrap();
    let ids: Vec<String> = buf
        .iter()
        .filter_map(|e| match e {
            AuditEntry::AgentDelegated { delegation_id, .. } => Some(delegation_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(
        ids[0], ids[1],
        "different delegations must have different IDs"
    );
}

#[test]
fn non_delegate_tools_do_not_interfere_with_a2a_audit() {
    let dispatcher = A2aDispatcher::new(passthrough(), build_pool(), "parent");

    // Mix regular tool calls with delegations.
    dispatcher.dispatch("clock", "{}").unwrap();
    dispatcher.dispatch("echo", r#"{"msg":"hi"}"#).unwrap();
    dispatcher
        .dispatch("delegate", r#"{"agent":"summarizer","task":"t"}"#)
        .unwrap();
    dispatcher.dispatch("clock", "{}").unwrap();

    let buf = dispatcher.audit_buffer.lock().unwrap();
    // Only the delegation should produce audit entries.
    assert_eq!(
        buf.len(),
        2,
        "only delegation entries should be in the A2A audit buffer"
    );
}

#[test]
fn full_a2a_pipeline_delegate_flush_verify() {
    // 1. Build the pool with two agents.
    let pool = build_pool();

    // 2. Create the dispatcher for the orchestrator agent.
    let dispatcher = A2aDispatcher::new(passthrough(), pool, "orchestrator");

    // 3. Delegate to both sub-agents.
    let r_sum = dispatcher
        .dispatch(
            "delegate",
            r#"{"agent":"summarizer","task":"Summarise the board meeting transcript","context":"Q3 2026"}"#,
        )
        .unwrap();
    let r_res = dispatcher
        .dispatch(
            "delegate",
            r#"{"agent":"researcher","task":"Find supporting literature","max_turns":8}"#,
        )
        .unwrap();

    // 4. Parse responses.
    let resp_sum: A2aResponse = serde_json::from_str(&r_sum).unwrap();
    let resp_res: A2aResponse = serde_json::from_str(&r_res).unwrap();
    assert!(resp_sum.success);
    assert!(resp_res.success);
    assert_eq!(resp_sum.tool_calls_made, 3);
    assert_eq!(resp_res.tool_calls_made, 5);

    // 5. Flush and verify the main audit log.
    let mut log = AuditLog::new();
    dispatcher.flush_audit(&mut log);

    assert_eq!(log.len(), 4, "2 delegations × 2 entries each");

    // Verify the audit chain: delegated → completed → delegated → completed
    let entries = log.entries();
    assert!(
        matches!(&entries[0], AuditEntry::AgentDelegated { target_agent_id, .. } if target_agent_id == "summarizer")
    );
    assert!(
        matches!(&entries[1], AuditEntry::AgentDelegationCompleted { target_agent_id, .. } if target_agent_id == "summarizer")
    );
    assert!(
        matches!(&entries[2], AuditEntry::AgentDelegated { target_agent_id, .. } if target_agent_id == "researcher")
    );
    assert!(
        matches!(&entries[3], AuditEntry::AgentDelegationCompleted { target_agent_id, .. } if target_agent_id == "researcher")
    );

    // 6. Verify the buffer is now empty.
    assert!(dispatcher.audit_buffer.lock().unwrap().is_empty());
}
