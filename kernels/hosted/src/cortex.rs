//! Cortex invocation seam for the hosted kernel (E7 S7.4 wiring).
//!
//! This module wires [`vita::ChatCortexBridge`] — the Rust-native tool-calling
//! cortex loop — into the hosted kernel so the agent can be driven from the CLI
//! via the `ask` subcommand.  Two pieces live here:
//!
//! 1. [`RegistryToolDispatcher`] — adapts a [`praxis::ToolRegistry`] to the
//!    [`vita::ToolDispatcher`] trait the bridge calls when the model emits a
//!    tool call.  It builds a [`praxis::ToolEnvelope`] from the `(name, args)`
//!    pair, dispatches it through the registry, and returns the UTF-8 result
//!    string (or an `Err(String)` the bridge feeds back to the model).
//!
//! 2. [`build_chat_cortex`] — constructs a [`vita::ChatCortexBridge`] over a
//!    [`llm_backends::chat::ChatBackend`].  The backend is a CI-safe fixture by
//!    default; a live tool-calling backend is only constructed when explicitly
//!    configured.
//!
//! # CI safety
//!
//! The shipped fixture `ChatBackend` returns text only (no `tool_calls`), so in
//! CI / fixture mode the `ask` flow returns a deterministic text answer without
//! dispatching any tools.  Live tool-calling backends drive real tool use.

use std::sync::Arc;

use llm_backends::chat::ChatBackend;
use llm_backends::compat::OpenAiCompatibleBackend;
use llm_backends::{BackendCapabilities, ProviderConfig};
use praxis::{Bus, ToolEnvelope, ToolRegistry};
use vita::{ChatCortexBridge, ToolDispatcher, ToolSpec};

/// Stable correlation id base for cortex-originated tool envelopes.
const CORTEX_CORRELATION_BASE: u64 = 0xC0_DE;

/// Adapts a [`praxis::ToolRegistry`] to the [`vita::ToolDispatcher`] trait.
///
/// The bridge calls [`ToolDispatcher::dispatch`] with the tool name the model
/// asked for and a JSON string of arguments; this adapter wraps both into a
/// [`ToolEnvelope`] and routes it through `registry.dispatch`, returning the
/// UTF-8 result on success or a human-readable error string on failure.
pub struct RegistryToolDispatcher {
    registry: Arc<ToolRegistry>,
}

impl RegistryToolDispatcher {
    /// Wraps a shared tool registry.
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self { registry }
    }

    /// Derives the cortex [`ToolSpec`] set from the registry's registered tools.
    ///
    /// Each tool's `id` becomes the spec `name`; the description is pulled from
    /// the tool's JSON `schema` (`description` field) when present, otherwise a
    /// generic placeholder is used.  This is what populates
    /// [`vita::InvokeRequest::tools`].
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.registry
            .list()
            .into_iter()
            .map(|id| {
                let description = self
                    .registry
                    .lookup(&id)
                    .and_then(|driver| {
                        serde_json::from_str::<serde_json::Value>(driver.schema())
                            .ok()
                            .and_then(|v| {
                                v.get("description")
                                    .and_then(|d| d.as_str())
                                    .map(str::to_string)
                            })
                    })
                    .unwrap_or_else(|| format!("Tool `{id}`."));
                ToolSpec {
                    name: id,
                    description,
                }
            })
            .collect()
    }
}

impl ToolDispatcher for RegistryToolDispatcher {
    fn dispatch(&self, tool_name: &str, args: &str) -> Result<String, String> {
        let payload = args.as_bytes().to_vec();
        let envelope = ToolEnvelope::new(Bus::Mcp, tool_name, payload, CORTEX_CORRELATION_BASE);
        match self.registry.dispatch(&envelope) {
            Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
            Err(e) => Err(format!("tool `{tool_name}` failed: {e:?}")),
        }
    }
}

/// Builds a [`ChatCortexBridge`] over a chat backend.
///
/// The backend is selected as follows:
/// - When `ANIMA_COMPAT_LIVE=1` *and* `ANIMA_COMPAT_URL` is set, a live
///   OpenAI-compatible tool-calling backend is constructed from the
///   `ANIMA_COMPAT_*` env prefix.
/// - Otherwise a CI-safe fixture backend is returned.  Callers may seed the
///   fixture map with `(prompt, completion-tokens)` pairs so deterministic
///   flows (and tests) get predictable text answers.
///
/// The returned bridge carries the supplied turn / tool-call limits.
pub fn build_chat_cortex(
    fixtures: impl IntoIterator<Item = (String, Vec<String>)>,
    max_turns: u32,
    max_tool_calls: u32,
) -> ChatCortexBridge {
    let backend: Arc<dyn ChatBackend> = build_chat_backend(fixtures);
    ChatCortexBridge::new(backend).with_limits(max_turns, max_tool_calls)
}

/// Constructs the chat backend that backs the cortex.
///
/// Live mode is opt-in (`ANIMA_COMPAT_LIVE=1`); the default is a fixture-mode
/// OpenAI-compatible backend so the hosted kernel and its tests stay hermetic.
fn build_chat_backend(
    fixtures: impl IntoIterator<Item = (String, Vec<String>)>,
) -> Arc<dyn ChatBackend> {
    let config = ProviderConfig::from_env_prefix(
        "anima-compat",
        "ANIMA_COMPAT",
        "http://localhost:11434/v1",
        "anima-cortex",
        8_192,
        BackendCapabilities::full(),
    );
    if std::env::var("ANIMA_COMPAT_LIVE").as_deref() == Ok("1")
        && std::env::var("ANIMA_COMPAT_URL").is_ok()
    {
        Arc::new(OpenAiCompatibleBackend::live(config))
    } else {
        Arc::new(OpenAiCompatibleBackend::fixture(config, fixtures))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scripted mock `ChatBackend` that returns a fixed text answer regardless
    /// of input — enough to drive a deterministic `ask` flow in tests.
    #[derive(Debug)]
    struct ScriptedChatBackend {
        answer: String,
    }

    impl scheduler::LlmBackend for ScriptedChatBackend {
        fn id(&self) -> &'static str {
            "scripted-mock"
        }
        fn stream_completion<'a>(
            &'a self,
            _prompt: &'a str,
            _cancel: &'a scheduler::backend::CancellationToken,
        ) -> scheduler::backend::CompletionFuture<'a> {
            let answer = self.answer.clone();
            Box::pin(async move {
                Ok(vec![
                    scheduler::backend::StreamingCompletion::Token(answer),
                    scheduler::backend::StreamingCompletion::Done,
                ])
            })
        }
        fn model_id(&self) -> &str {
            "scripted-mock-model"
        }
        fn max_context_tokens(&self) -> u32 {
            8_192
        }
    }

    impl ChatBackend for ScriptedChatBackend {
        fn chat_complete(
            &self,
            _messages: &[llm_backends::chat::ChatMessage],
            _tools: &[llm_backends::chat::ToolSpec],
            _cancel: &scheduler::backend::CancellationToken,
        ) -> Result<llm_backends::chat::ChatResponse, scheduler::backend::LlmBackendError> {
            Ok(llm_backends::chat::ChatResponse {
                content: self.answer.clone(),
                tool_calls: vec![],
                finish_reason: llm_backends::chat::FinishReason::Stop,
                model: "scripted-mock-model".to_string(),
                usage_tokens: None,
            })
        }
    }

    fn test_registry() -> Arc<ToolRegistry> {
        Arc::new(crate::build_default_tool_registry())
    }

    #[test]
    fn dispatcher_routes_known_tool_through_registry() {
        let dispatcher = RegistryToolDispatcher::new(test_registry());
        let args = serde_json::json!({ "url": "https://example.com/animaos" }).to_string();
        let result = dispatcher
            .dispatch("browse", &args)
            .expect("browse should dispatch through the registry");
        assert!(
            !result.is_empty(),
            "browse result should be non-empty, got: {result:?}"
        );
        assert!(
            result.contains("AnimaOS"),
            "browse result should reference the canned page, got: {result:?}"
        );
    }

    #[test]
    fn dispatcher_unknown_tool_returns_err() {
        let dispatcher = RegistryToolDispatcher::new(test_registry());
        let err = dispatcher
            .dispatch("definitely-not-a-tool", "{}")
            .expect_err("unknown tool must error");
        assert!(err.contains("definitely-not-a-tool"), "got: {err}");
    }

    #[test]
    fn tool_specs_cover_registered_tools_with_descriptions() {
        let dispatcher = RegistryToolDispatcher::new(test_registry());
        let specs = dispatcher.tool_specs();
        assert!(!specs.is_empty(), "tool specs should not be empty");
        // Every spec carries a non-empty description.
        for spec in &specs {
            assert!(!spec.name.is_empty());
            assert!(
                !spec.description.is_empty(),
                "spec {:?} missing desc",
                spec.name
            );
        }
        // web-search is registered by the default registry.
        assert!(
            specs.iter().any(|s| s.name == "web-search"),
            "expected web-search in specs: {:?}",
            specs.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn ask_flow_with_scripted_backend_is_deterministic() {
        use vita::{AuditLog, CortexBackend, InvokeRequest};

        let registry = test_registry();
        let dispatcher = RegistryToolDispatcher::new(Arc::clone(&registry));
        let backend: Arc<dyn ChatBackend> = Arc::new(ScriptedChatBackend {
            answer: "deterministic answer".to_string(),
        });
        let bridge = ChatCortexBridge::new(backend).with_limits(4, 4);

        let request = InvokeRequest {
            task_id: "test-ask-1".to_string(),
            agent_id: "anima".to_string(),
            description: "what is animaos?".to_string(),
            tools: dispatcher.tool_specs(),
            identity: serde_json::json!({ "operator_name": "Tester" }),
            route_id: None,
            memory_scope: None,
            max_turns: None,
            max_tool_calls: None,
        };
        let mut audit = AuditLog::new();
        let result = bridge
            .invoke(request, &dispatcher, &mut audit)
            .expect("scripted invoke should succeed");
        assert_eq!(result.output, "deterministic answer");
        assert_eq!(result.tool_calls_made, 0);
        assert_eq!(result.task_id, "test-ask-1");
    }

    #[test]
    fn build_chat_cortex_returns_usable_bridge_in_fixture_mode() {
        use vita::{AuditLog, CortexBackend, InvokeRequest};

        let registry = test_registry();
        let dispatcher = RegistryToolDispatcher::new(Arc::clone(&registry));
        let bridge = build_chat_cortex([("ping".to_string(), vec!["pong".to_string()])], 4, 4);
        let request = InvokeRequest {
            task_id: "test-fixture-1".to_string(),
            agent_id: "anima".to_string(),
            description: "ping".to_string(),
            tools: dispatcher.tool_specs(),
            identity: serde_json::Value::Null,
            route_id: None,
            memory_scope: None,
            max_turns: None,
            max_tool_calls: None,
        };
        let mut audit = AuditLog::new();
        let result = bridge
            .invoke(request, &dispatcher, &mut audit)
            .expect("fixture invoke should succeed");
        // The fixture backend returns the seeded completion for the "ping" key.
        assert!(result.output.contains("pong"), "got: {:?}", result.output);
    }
}
