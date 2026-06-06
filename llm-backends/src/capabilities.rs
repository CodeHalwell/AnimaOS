//! Backend capability descriptors and provider configuration.
//!
//! [`BackendCapabilities`] lets the router and the tool-calling loop ask
//! *"can this backend do tools?"* before routing traffic to it.  [`ProviderConfig`]
//! centralises the per-provider knobs (URL, model, key, timeouts) so every
//! HTTP backend can be constructed uniformly from a single config struct.

use std::time::Duration;

/// What optional capabilities a backend supports.
///
/// The router consults these flags to decide whether to send tool definitions
/// to the backend or to fall back to prompt-format tool emulation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendCapabilities {
    /// Backend accepts `tools` and returns `tool_calls` in the response.
    pub tools: bool,
    /// Backend supports token-by-token streaming (SSE `data:` chunks).
    pub streaming: bool,
    /// Backend exposes an `/v1/embeddings` (or equivalent) endpoint.
    pub embeddings: bool,
    /// Backend honours a `response_format: { type: "json_object" }` field.
    pub json_mode: bool,
    /// Backend can process image inputs in the message list.
    pub vision: bool,
}

impl BackendCapabilities {
    /// Full capability set — used for providers known to support everything.
    pub fn full() -> Self {
        Self {
            tools: true,
            streaming: true,
            embeddings: true,
            json_mode: true,
            vision: true,
        }
    }

    /// Minimal capability set — text-only, no tools, no embeddings.
    pub fn text_only() -> Self {
        Self {
            tools: false,
            streaming: true,
            embeddings: false,
            json_mode: false,
            vision: false,
        }
    }

    /// OpenAI-API-compatible provider with tool support but no vision.
    pub fn openai_compat() -> Self {
        Self {
            tools: true,
            streaming: true,
            embeddings: true,
            json_mode: true,
            vision: false,
        }
    }
}

impl Default for BackendCapabilities {
    fn default() -> Self {
        Self::text_only()
    }
}

/// Configuration for a single provider instance.
///
/// The router binds a tier to a [`ProviderConfig`]; [`BackendFactory::from_config`]
/// turns it into a concrete [`LlmBackend`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConfig {
    /// Stable identifier surfaced in audit logs (e.g. `"vllm"`, `"lmstudio"`).
    pub id: String,
    /// Base URL of the API (e.g. `"http://vllm:8000/v1"`).
    pub base_url: String,
    /// Model tag passed in every request (e.g. `"mistral-7b-instruct"`).
    pub model: String,
    /// Optional Bearer token.  Redacted from all audit and log output.
    pub api_key: Option<String>,
    /// Maximum context window in tokens (used by the scheduler).
    pub max_context_tokens: u32,
    /// Per-request network timeout.
    pub request_timeout: Duration,
    /// Capability flags — drives tool-calling and embedding path selection.
    pub capabilities: BackendCapabilities,
}

impl ProviderConfig {
    /// Creates a config from an environment-variable prefix.
    ///
    /// Given prefix `ANIMA_VLLM`, reads:
    /// - `ANIMA_VLLM_URL` → `base_url`
    /// - `ANIMA_VLLM_MODEL` → `model`
    /// - `ANIMA_VLLM_API_KEY` → `api_key`
    /// - `ANIMA_VLLM_CTX` → `max_context_tokens`
    /// - `ANIMA_VLLM_TIMEOUT` → `request_timeout` in seconds
    ///
    /// Returns `None` for each variable that is absent; the caller supplies
    /// defaults through `default_base_url` and `default_model`.
    pub fn from_env_prefix(
        id: impl Into<String>,
        prefix: &str,
        default_base_url: &str,
        default_model: &str,
        default_ctx: u32,
        capabilities: BackendCapabilities,
    ) -> Self {
        let base_url =
            std::env::var(format!("{prefix}_URL")).unwrap_or_else(|_| default_base_url.to_string());
        let model =
            std::env::var(format!("{prefix}_MODEL")).unwrap_or_else(|_| default_model.to_string());
        let api_key = std::env::var(format!("{prefix}_API_KEY")).ok();
        let max_context_tokens = std::env::var(format!("{prefix}_CTX"))
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(default_ctx);
        let request_timeout = std::env::var(format!("{prefix}_TIMEOUT"))
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(300));

        Self {
            id: id.into(),
            base_url,
            model,
            api_key,
            max_context_tokens,
            request_timeout,
            capabilities,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_capabilities_has_tools_and_embeddings() {
        let caps = BackendCapabilities::full();
        assert!(caps.tools);
        assert!(caps.streaming);
        assert!(caps.embeddings);
        assert!(caps.json_mode);
        assert!(caps.vision);
    }

    #[test]
    fn text_only_capabilities_disables_tools_and_embeddings() {
        let caps = BackendCapabilities::text_only();
        assert!(!caps.tools);
        assert!(caps.streaming);
        assert!(!caps.embeddings);
        assert!(!caps.json_mode);
        assert!(!caps.vision);
    }

    #[test]
    fn openai_compat_capabilities_has_tools_but_no_vision() {
        let caps = BackendCapabilities::openai_compat();
        assert!(caps.tools);
        assert!(caps.embeddings);
        assert!(!caps.vision);
    }

    #[test]
    fn default_capabilities_is_text_only() {
        assert_eq!(
            BackendCapabilities::default(),
            BackendCapabilities::text_only()
        );
    }

    #[test]
    fn from_env_prefix_uses_defaults_when_vars_absent() {
        // Ensure vars are absent so CI doesn't see a stale env.
        for v in ["ANIMA_TEST_URL", "ANIMA_TEST_MODEL", "ANIMA_TEST_CTX"] {
            std::env::remove_var(v);
        }
        let cfg = ProviderConfig::from_env_prefix(
            "test",
            "ANIMA_TEST",
            "http://localhost:8000/v1",
            "test-model",
            4096,
            BackendCapabilities::text_only(),
        );
        assert_eq!(cfg.base_url, "http://localhost:8000/v1");
        assert_eq!(cfg.model, "test-model");
        assert_eq!(cfg.max_context_tokens, 4096);
        assert!(cfg.api_key.is_none());
    }
}
