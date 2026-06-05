//! Integration tests — E7 Embodiment vertical slice.
//!
//! Covers:
//! - Phase 0 (S7.0): EgressGuard SSRF protection, egress audit entries, secret
//!   redaction.
//! - Phase 1 (S7.1): WebSearchTool with FixtureProvider dispatched via the
//!   MockCortexBridge's tool-dispatch loop.
//! - Phase 3 minimal (S7.3): LexicalScorer selects `web-search` for a search
//!   query; tier boundary is never widened by scoring.
//! - Cross-cutting: ToolSelection audit entry emitted; EgressRequested /
//!   EgressBlocked entries flushed from the dispatcher.
//!
//! All tests are fully hermetic (no network calls, no live LLM API keys).

use actuators::egress::EgressGuard;
use actuators::scorer::{FixtureScorer, LexicalScorer, ToolScorer};
use actuators::web_search::{SearchResult, WebSearchTool};
use praxis::registry::ToolRegistry;
use praxis::{length_robust_filter, ToolDriver};
use vita::cortex_bridge::{
    CortexBackend, FnDispatcher, InvokeMemoryScope, InvokeRequest, MockCortexBridge, ToolSpec,
};
use vita::dispatch::EgressAwareDispatcher;
use vita::{AuditEntry, AuditLog, ToolDispatcher};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn sample_fixture() -> Vec<SearchResult> {
    vec![
        SearchResult {
            title: "Rust Programming Language".to_string(),
            url: "https://www.rust-lang.org".to_string(),
            snippet: "A language empowering everyone to build reliable and efficient software."
                .to_string(),
        },
        SearchResult {
            title: "Rust by Example".to_string(),
            url: "https://doc.rust-lang.org/rust-by-example/".to_string(),
            snippet: "Collection of runnable Rust examples with explanations.".to_string(),
        },
    ]
}

#[allow(dead_code)]
fn tool_specs_from_registry(registry: &ToolRegistry) -> Vec<ToolSpec> {
    registry
        .list()
        .iter()
        .map(|id| ToolSpec {
            name: id.clone(),
            description: registry
                .lookup(id)
                .map(|t| t.schema().to_string())
                .unwrap_or_else(|| r#"{"description":""}"#.to_string()),
        })
        .collect()
}

fn build_registry() -> ToolRegistry {
    let registry = ToolRegistry::new();
    let search_tool = WebSearchTool::with_fixture(sample_fixture());
    registry.register(search_tool);
    registry
}

// ── E7 S7.0 — Egress guard tests ──────────────────────────────────────────────

#[test]
fn egress_guard_allows_public_https_url() {
    let guard = EgressGuard::default();
    assert!(guard.check_url("https://www.rust-lang.org").is_allowed());
}

#[test]
fn egress_guard_blocks_private_ip_ssrf() {
    let guard = EgressGuard::default();
    assert!(guard.check_url("https://192.168.0.1/admin").is_denied());
    assert!(guard.check_url("https://10.0.0.1/").is_denied());
    assert!(guard.check_url("https://127.0.0.1:8080/").is_denied());
    assert!(guard.check_url("https://169.254.169.254/meta-data/").is_denied());
}

#[test]
fn egress_guard_blocks_http_scheme() {
    let guard = EgressGuard::default();
    assert!(guard.check_url("http://example.com/").is_denied());
}

#[test]
fn egress_aware_dispatcher_emits_egress_requested_for_web_search() {
    let registry = build_registry();
    let inner = FnDispatcher({
        let registry = registry.clone();
        move |name: &str, args: &str| {
            let envelope = praxis::ToolEnvelope::new(
                praxis::Bus::Mcp,
                name,
                args.as_bytes().to_vec(),
                0,
            );
            registry
                .dispatch(&envelope)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .map_err(|e| format!("{e:?}"))
        }
    });
    let dispatcher = EgressAwareDispatcher::new(inner, EgressGuard::default());

    // web-search dispatched with a valid query.
    let result = dispatcher.dispatch("web-search", r#"{"query":"rust programming"}"#);
    assert!(result.is_ok(), "dispatch should succeed: {result:?}");

    let buf = dispatcher.audit_buffer.lock().unwrap();
    assert_eq!(buf.len(), 1);
    assert!(
        matches!(&buf[0], AuditEntry::EgressRequested { tool_id, .. } if tool_id == "web-search"),
        "expected EgressRequested for web-search"
    );
}

#[test]
fn egress_aware_dispatcher_blocks_browser_call_to_private_ip() {
    let inner = FnDispatcher(|_name: &str, _args: &str| Ok("should-not-reach".to_string()));
    let dispatcher = EgressAwareDispatcher::new(inner, EgressGuard::default());

    let result = dispatcher.dispatch("browser", r#"{"url":"https://192.168.1.10/page"}"#);
    assert!(result.is_err(), "private IP should be blocked");
    let msg = result.unwrap_err();
    assert!(msg.contains("egress-blocked"), "error should say egress-blocked: {msg}");

    let buf = dispatcher.audit_buffer.lock().unwrap();
    assert_eq!(buf.len(), 1);
    assert!(matches!(&buf[0], AuditEntry::EgressBlocked { .. }));
}

#[test]
fn flush_audit_moves_egress_entries_to_main_log() {
    let inner = FnDispatcher(|_: &str, _: &str| Ok("ok".to_string()));
    let dispatcher = EgressAwareDispatcher::new(inner, EgressGuard::default());
    let _ = dispatcher.dispatch("web-search", r#"{"query":"test"}"#);

    let mut audit = AuditLog::new();
    dispatcher.flush_audit(&mut audit);

    assert_eq!(audit.len(), 1);
    assert!(dispatcher.audit_buffer.lock().unwrap().is_empty());
}

// ── E7 S7.0.4 — No secrets in audit log ──────────────────────────────────────

#[test]
fn api_key_in_url_args_is_redacted_in_audit_log() {
    let inner = FnDispatcher(|_: &str, _: &str| Ok("ok".to_string()));
    let dispatcher = EgressAwareDispatcher::new(inner, EgressGuard::default());

    // Simulate a browser call with an API key in the URL query string.
    let _ = dispatcher.dispatch(
        "browser",
        r#"{"url":"https://example.com/page?api_key=supersecret&q=test"}"#,
    );

    let buf = dispatcher.audit_buffer.lock().unwrap();
    for entry in buf.iter() {
        let json = serde_json::to_string(entry).unwrap_or_default();
        assert!(
            !json.contains("supersecret"),
            "raw API key must not appear in audit entry: {json}"
        );
        assert!(json.contains("[REDACTED]"), "redacted placeholder should be present");
    }
}

// ── E7 S7.1 — WebSearchTool end-to-end via MockCortexBridge ──────────────────

#[test]
fn mock_cortex_dispatches_web_search_tool_and_returns_results() {
    let registry = build_registry();
    let bridge = MockCortexBridge::default();

    let request = InvokeRequest {
        task_id: "e7-test-1".to_string(),
        agent_id: "test-agent".to_string(),
        description: "Search the web for Rust programming resources".to_string(),
        tools: vec![ToolSpec {
            name: "web-search".to_string(),
            description: "Search the web for information using a search engine query".to_string(),
        }],
        identity: serde_json::json!({}),
        route_id: Some("frontier".to_string()),
        memory_scope: Some(InvokeMemoryScope::full()),
        max_turns: Some(4),
        max_tool_calls: Some(4),
    };

    let dispatcher = FnDispatcher({
        let registry = registry.clone();
        move |name: &str, args: &str| {
            let envelope = praxis::ToolEnvelope::new(
                praxis::Bus::Mcp,
                name,
                args.as_bytes().to_vec(),
                0,
            );
            registry
                .dispatch(&envelope)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .map_err(|e| format!("{e:?}"))
        }
    });

    let mut audit = AuditLog::new();
    let result = bridge.invoke(request, &dispatcher, &mut audit).expect("cortex invocation failed");

    assert!(result.tool_calls_made > 0, "mock cortex should make at least one tool call");
    assert!(!result.episode_summary.is_empty());
}

#[test]
fn web_search_tool_returns_fixture_results_as_json() {
    let tool = WebSearchTool::with_fixture(sample_fixture());
    let payload = serde_json::json!({"query": "rust programming", "max_results": 2})
        .to_string()
        .into_bytes();
    let output = tool.invoke(&payload).expect("invoke should succeed");
    let results: Vec<SearchResult> = serde_json::from_slice(&output).expect("should parse");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].title, "Rust Programming Language");
    assert!(results[0].url.starts_with("https://"));
}

#[test]
fn web_search_tool_is_registered_in_tool_registry() {
    let registry = build_registry();
    let ids = registry.list();
    assert!(ids.contains(&"web-search".to_string()), "web-search should be in registry: {ids:?}");
}

// ── E7 S7.3 — Semantic tool selection ────────────────────────────────────────

const ALL_TOOLS: &[(&str, &str)] = &[
    ("clock", "Returns the current Unix timestamp in milliseconds"),
    ("echo", "Echoes the input payload back to the caller unchanged"),
    (
        "web-search",
        "Search the web for information using a search engine query. Returns ranked results.",
    ),
    ("text-io", "Read and write text files on the local filesystem"),
];

#[test]
fn lexical_scorer_selects_web_search_for_web_query() {
    let scorer = LexicalScorer;
    let kept = scorer.select("search the web for recent Rust releases", ALL_TOOLS, 0.5);
    let ids: Vec<&str> = kept.iter().map(|c| c.id.as_str()).collect();
    assert!(ids.contains(&"web-search"), "web-search should be selected for a web query: {ids:?}");
}

#[test]
fn lexical_scorer_selection_never_widens_tier_allow_list() {
    let tier_tools: &[(&str, &str)] = &[
        ("clock", "Returns the current Unix timestamp in milliseconds"),
        ("echo", "Echoes the input payload back to the caller"),
    ];
    // Even with very aggressive scorer, only tools in the tier list can appear.
    let scorer = FixtureScorer::new([("clock", 1.0_f32), ("echo", 0.9_f32), ("web-search", 99.0_f32)]);
    let kept = scorer.select("search the web", tier_tools, 0.1);
    let ids: Vec<&str> = kept.iter().map(|c| c.id.as_str()).collect();
    assert!(!ids.contains(&"web-search"), "web-search must not appear — not in tier list");
}

#[test]
fn tool_selection_audit_entry_carries_correct_counts() {
    let scorer = LexicalScorer;
    let kept = scorer.select("search the web", ALL_TOOLS, 0.5);

    let mut audit = AuditLog::new();
    audit.push(AuditEntry::ToolSelection {
        agent_id: "test-agent".to_string(),
        task_description: "search the web".to_string(),
        candidates_scored: ALL_TOOLS.len(),
        kept: kept.len(),
        tau_rel: 0.5,
    });

    let entry = audit.entries().last().unwrap();
    assert!(matches!(
        entry,
        AuditEntry::ToolSelection { candidates_scored, kept: k, tau_rel, .. }
            if *candidates_scored == ALL_TOOLS.len()
            && *k == kept.len()
            && (*tau_rel - 0.5).abs() < 1e-6
    ));
}

#[test]
fn length_robust_filter_applied_after_scoring_respects_tau_rel() {
    let scorer = FixtureScorer::new([
        ("web-search", 1.0_f32),
        ("clock", 0.3_f32),
        ("echo", 0.1_f32),
    ]);
    let candidates = scorer.score("search for info", ALL_TOOLS);
    // tau_rel = 0.5 → threshold = 0.5 * 1.0 = 0.5, keeps web-search (1.0) and clock (0.3 < 0.5 → out)
    let kept = length_robust_filter(&candidates, 0.5);
    let ids: Vec<&str> = kept.iter().map(|c| c.id.as_str()).collect();
    assert!(ids.contains(&"web-search"));
    assert!(!ids.contains(&"clock"), "clock score 0.3 < threshold 0.5 should be dropped");
    assert!(!ids.contains(&"echo"), "echo score 0.1 < threshold 0.5 should be dropped");
}

// ── Cross-cutting: full pipeline (select → dispatch → audit) ─────────────────

#[test]
fn full_e7_pipeline_selects_tool_dispatches_and_audits() {
    // 1. Tier allow-list (frontier): all four tools.
    let tier_tools = ALL_TOOLS;

    // 2. Score against task description.
    let scorer = LexicalScorer;
    let task_desc = "search the web for information about Rust concurrency";
    let kept_candidates = scorer.select(task_desc, tier_tools, 0.3);

    // 3. Emit ToolSelection audit entry.
    let mut audit = AuditLog::new();
    audit.push(AuditEntry::ToolSelection {
        agent_id: "agent-a".to_string(),
        task_description: task_desc.to_string(),
        candidates_scored: tier_tools.len(),
        kept: kept_candidates.len(),
        tau_rel: 0.3,
    });

    // 4. Build ToolSpec list for the cortex from kept candidates.
    let selected_specs: Vec<ToolSpec> = kept_candidates
        .iter()
        .map(|c| ToolSpec {
            name: c.id.clone(),
            description: tier_tools
                .iter()
                .find(|(id, _)| *id == c.id)
                .map(|(_, d)| d.to_string())
                .unwrap_or_default(),
        })
        .collect();

    // web-search should be in the selected set for this query.
    assert!(
        selected_specs.iter().any(|s| s.name == "web-search"),
        "web-search should be selected for a search query"
    );

    // 5. Dispatch via EgressAwareDispatcher.
    let registry = build_registry();
    let inner = FnDispatcher({
        let registry = registry.clone();
        move |name: &str, args: &str| {
            let envelope = praxis::ToolEnvelope::new(
                praxis::Bus::Mcp,
                name,
                args.as_bytes().to_vec(),
                0,
            );
            registry
                .dispatch(&envelope)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .map_err(|e| format!("{e:?}"))
        }
    });
    let dispatcher = EgressAwareDispatcher::new(inner, EgressGuard::default());
    let result = dispatcher.dispatch("web-search", r#"{"query":"Rust concurrency"}"#);
    assert!(result.is_ok(), "dispatch should succeed: {result:?}");

    // 6. Flush egress entries.
    dispatcher.flush_audit(&mut audit);

    // 7. Assert audit trail contains ToolSelection + EgressRequested.
    let entries = audit.entries();
    assert!(entries.iter().any(|e| matches!(e, AuditEntry::ToolSelection { .. })));
    assert!(entries.iter().any(|e| matches!(e, AuditEntry::EgressRequested { tool_id, .. } if tool_id == "web-search")));
}
