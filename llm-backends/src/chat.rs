//! Chat-message types and the [`ChatBackend`] extension trait.
//!
//! The core [`LlmBackend`] trait streams text from a raw prompt string — the
//! lowest-common-denominator surface shared with `no_std` targets.  Real
//! provider APIs (OpenAI, Anthropic, Ollama) use a *chat messages* structure
//! with explicit roles and optional tool definitions.  [`ChatBackend`] extends
//! [`LlmBackend`] with that richer interface, staying std-only so the core
//! trait remains `no_std`-clean.
//!
//! Every [`ChatBackend`] also exposes a synchronous [`ChatBackend::health`]
//! probe (E8 S8.0.3) so the hosted kernel can verify a local server is up
//! before routing real traffic to it.

use scheduler::backend::{CancellationToken, LlmBackend, LlmBackendError};
use serde::{Deserialize, Serialize};

// ── Message types ─────────────────────────────────────────────────────────────

/// Role of a participant in a chat conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    /// System-level instruction visible only to the model.
    System,
    /// Human turn.
    User,
    /// Model turn.
    Assistant,
    /// Tool result injected back into the conversation.
    Tool,
}

/// A single message in a chat conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Who sent this message.
    pub role: ChatRole,
    /// Text content (may be empty for assistant turns that only emit tool calls).
    pub content: String,
    /// For [`ChatRole::Tool`] messages: the `id` of the originating tool call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    /// Construct a system instruction.
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::System,
            content: content.into(),
            tool_call_id: None,
        }
    }

    /// Construct a user turn.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
            tool_call_id: None,
        }
    }

    /// Construct an assistant turn.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: content.into(),
            tool_call_id: None,
        }
    }

    /// Construct a tool-result turn.
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Tool,
            content: content.into(),
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

// ── Tool types ────────────────────────────────────────────────────────────────

/// A tool definition passed to the backend so the model can emit tool calls.
///
/// The `parameters` field must be a JSON Schema object.  Backends without
/// native tool support receive a prompt-format serialisation of this struct
/// instead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Stable tool name (e.g. `"web-search"`).
    pub name: String,
    /// Human-readable description used by the model to choose the tool.
    pub description: String,
    /// JSON Schema object describing the tool's input parameters.
    pub parameters: serde_json::Value,
}

/// A tool call emitted by the model in a chat response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Provider-assigned unique call ID, used to correlate the tool result.
    pub id: String,
    /// Name of the tool to call (must match a [`ToolSpec::name`]).
    pub name: String,
    /// JSON-encoded arguments object.
    pub arguments: String,
}

// ── Response types ────────────────────────────────────────────────────────────

/// Why a chat completion finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// Model reached a natural stopping point.
    Stop,
    /// Model emitted one or more tool calls.
    ToolCalls,
    /// Response was truncated at the context / token limit.
    MaxTokens,
    /// Cancelled by the caller.
    Cancelled,
    /// Provider returned an unexpected finish reason; the string is preserved.
    Other(String),
}

/// A complete chat response from the backend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatResponse {
    /// Text content of the assistant turn (may be empty when `tool_calls` is non-empty).
    pub content: String,
    /// Tool calls emitted in this turn (empty for text-only responses).
    pub tool_calls: Vec<ToolCall>,
    /// Why the model stopped generating.
    pub finish_reason: FinishReason,
    /// Model identifier echoed from the provider response.
    pub model: String,
    /// Approximate token usage (prompt + completion), if the provider reports it.
    pub usage_tokens: Option<u32>,
}

impl ChatResponse {
    /// Convenience: returns `true` if the model wants to call at least one tool.
    pub fn has_tool_calls(&self) -> bool {
        !self.tool_calls.is_empty()
    }

    /// Convenience: returns `true` if this is a plain text response.
    pub fn is_text(&self) -> bool {
        self.tool_calls.is_empty()
    }
}

// ── Extension trait ───────────────────────────────────────────────────────────

/// Extension of [`LlmBackend`] that adds a chat-messages interface and optional
/// tool-calling.
///
/// Implementors may choose to support only text chat (`tools` empty) or full
/// tool-calling.  The [`ChatBackend::capabilities`] method on the parent backend
/// advertises which paths are active.
///
/// # CI safety
///
/// All implementations must default to fixture/replay mode.  Live network calls
/// are opt-in and must be env-gated so CI never makes outbound requests.
pub trait ChatBackend: LlmBackend {
    /// Runs a synchronous (blocking) chat completion, optionally with tools.
    ///
    /// - `messages`: the full conversation so far.
    /// - `tools`: tool definitions to include (empty = text-only).
    /// - `cancel`: cooperative cancellation token.
    ///
    /// Returns a [`ChatResponse`] on success or a [`LlmBackendError`] on failure.
    fn chat_complete(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        cancel: &CancellationToken,
    ) -> Result<ChatResponse, LlmBackendError>;

    /// Synchronous health / readiness probe.
    ///
    /// Returns `true` when the backend is reachable and the configured model
    /// is loaded.  The default always returns `true` (safe for fixture and mock
    /// backends that perform no network I/O).  Live backends override this to
    /// hit the provider's health or models endpoint.
    fn health(&self) -> bool {
        true
    }
}

// ── Prompt-format tool emulation ──────────────────────────────────────────────

/// Serialises a slice of [`ToolSpec`]s into a prompt-format string for backends
/// that do not support native tool-calling.
///
/// The output is a compact JSON array appended after the last user message.
/// This is the fallback path when `BackendCapabilities::tools` is `false`.
pub fn tools_to_prompt_suffix(tools: &[ToolSpec]) -> String {
    if tools.is_empty() {
        return String::new();
    }
    let specs: Vec<serde_json::Value> = tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters,
            })
        })
        .collect();
    format!(
        "\n\nAvailable tools (JSON):\n{}",
        serde_json::to_string(&specs).unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_message_constructors_set_correct_roles() {
        assert_eq!(ChatMessage::system("hi").role, ChatRole::System);
        assert_eq!(ChatMessage::user("hi").role, ChatRole::User);
        assert_eq!(ChatMessage::assistant("hi").role, ChatRole::Assistant);
        let tr = ChatMessage::tool_result("id1", "result");
        assert_eq!(tr.role, ChatRole::Tool);
        assert_eq!(tr.tool_call_id, Some("id1".to_string()));
    }

    #[test]
    fn chat_response_has_tool_calls_returns_correct_flag() {
        let resp_text = ChatResponse {
            content: "hello".to_string(),
            tool_calls: vec![],
            finish_reason: FinishReason::Stop,
            model: "m".to_string(),
            usage_tokens: None,
        };
        assert!(resp_text.is_text());
        assert!(!resp_text.has_tool_calls());

        let resp_tool = ChatResponse {
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: "c1".to_string(),
                name: "clock".to_string(),
                arguments: "{}".to_string(),
            }],
            finish_reason: FinishReason::ToolCalls,
            model: "m".to_string(),
            usage_tokens: None,
        };
        assert!(resp_tool.has_tool_calls());
        assert!(!resp_tool.is_text());
    }

    #[test]
    fn tools_to_prompt_suffix_is_empty_for_no_tools() {
        assert_eq!(tools_to_prompt_suffix(&[]), "");
    }

    #[test]
    fn tools_to_prompt_suffix_includes_tool_names() {
        let tools = vec![ToolSpec {
            name: "web-search".to_string(),
            description: "Search the web".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let suffix = tools_to_prompt_suffix(&tools);
        assert!(suffix.contains("web-search"));
        assert!(suffix.contains("Search the web"));
    }

    #[test]
    fn tool_call_fields_are_accessible() {
        let tc = ToolCall {
            id: "call_abc".to_string(),
            name: "clock".to_string(),
            arguments: r#"{"tz":"UTC"}"#.to_string(),
        };
        assert_eq!(tc.name, "clock");
        assert_eq!(tc.id, "call_abc");
    }

    #[test]
    fn chat_message_serde_round_trips() {
        let msg = ChatMessage::user("ping");
        let json = serde_json::to_string(&msg).unwrap();
        let recovered: ChatMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered, msg);
    }

    #[test]
    fn tool_result_message_carries_tool_call_id() {
        let msg = ChatMessage::tool_result("call_xyz", "result text");
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("call_xyz"));
        let recovered: ChatMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.tool_call_id, Some("call_xyz".to_string()));
    }
}
