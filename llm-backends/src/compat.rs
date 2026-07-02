//! OpenAI-compatible HTTP backend umbrella (E8 S8.1).
//!
//! [`OpenAiCompatibleBackend`] is a single generalized client for any server
//! that speaks the OpenAI `/v1/chat/completions` API, including:
//!
//! - **vLLM** (`vllm()`)
//! - **LM Studio** (`lmstudio()`)
//! - **NVIDIA NIM** (`nvidia_nim()`)
//! - **Hugging Face TGI** (`hf_tgi()`)
//! - **llama.cpp server** (`llamacpp_server()`)
//!
//! Each preset is a thin constructor that reads its configuration from
//! environment variables with sensible defaults (see [`ProviderConfig::from_env_prefix`]).
//!
//! # Modes
//!
//! * **Fixture** (default, CI-safe): responses are replayed from an injected
//!   fixture map.  No network calls, no API key required.
//! * **Live**: sends real HTTP requests to the configured endpoint via blocking
//!   [`ureq`].  Enabled by setting `ANIMA_COMPAT_LIVE=1` (or the preset's own
//!   env var).  Never used in CI.
//!
//! # Tool calling
//!
//! When `capabilities.tools` is `true` and a non-empty `tools` slice is
//! passed to [`ChatBackend::chat_complete`], the backend serialises the tool
//! definitions into the OpenAI `/v1/chat/completions` `tools` field and parses
//! `tool_calls` from the response.  When `capabilities.tools` is `false`, the
//! tool definitions are appended as a prompt suffix (prompt-format emulation).

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::sync::OnceLock;
use std::time::Duration;

use scheduler::backend::{
    CancellationToken, CompletionFuture, LlmBackend, LlmBackendError, StreamingCompletion,
};

use crate::capabilities::{BackendCapabilities, ProviderConfig};
use crate::chat::{
    tools_to_prompt_suffix, ChatBackend, ChatMessage, ChatResponse, ChatRole, FinishReason,
    ToolCall, ToolSpec,
};

// ── Internal mode ─────────────────────────────────────────────────────────────

enum BackendMode {
    /// Replay pre-recorded token streams from an in-memory fixture map.
    Fixture(HashMap<String, Vec<String>>),
    /// Make live HTTP requests to the configured endpoint.
    Live,
}

// ── Main struct ───────────────────────────────────────────────────────────────

/// An LLM backend that talks to any OpenAI-compatible `/v1/chat/completions` server.
pub struct OpenAiCompatibleBackend {
    config: ProviderConfig,
    mode: BackendMode,
    agent: ureq::Agent,
    /// Cached `&'static str` for [`LlmBackend::id`].  Leaked at most once per
    /// backend instance (on first `id()` call) to satisfy the `&'static str`
    /// contract without re-leaking on every dispatch.
    id_cache: OnceLock<&'static str>,
}

impl OpenAiCompatibleBackend {
    // ── Constructors ──────────────────────────────────────────────────────────

    /// Creates a fixture-mode backend with the given config and fixture map.
    ///
    /// This is the CI-safe path — no network I/O ever occurs.
    pub fn fixture(
        config: ProviderConfig,
        fixtures: impl IntoIterator<Item = (String, Vec<String>)>,
    ) -> Self {
        let agent = Self::build_agent(config.request_timeout);
        Self {
            config,
            mode: BackendMode::Fixture(fixtures.into_iter().collect()),
            agent,
            id_cache: OnceLock::new(),
        }
    }

    /// Creates a live-mode backend that makes real HTTP requests.
    ///
    /// # CI safety
    ///
    /// Never call this from unit tests or CI jobs.  Gate calls behind
    /// `#[ignore]` or an environment-variable guard.
    pub fn live(config: ProviderConfig) -> Self {
        let agent = Self::build_agent(config.request_timeout);
        Self {
            config,
            mode: BackendMode::Live,
            agent,
            id_cache: OnceLock::new(),
        }
    }

    /// Creates a backend from a [`ProviderConfig`], choosing fixture vs live
    /// based on whether `ANIMA_COMPAT_LIVE` is set to `"1"`.
    ///
    /// In fixture mode the map is empty so all prompts return the sentinel
    /// `"[fixture-not-found]"` token.
    pub fn from_config(config: ProviderConfig) -> Self {
        if std::env::var("ANIMA_COMPAT_LIVE").as_deref() == Ok("1") {
            Self::live(config)
        } else {
            Self::fixture(config, [])
        }
    }

    // ── Provider presets ──────────────────────────────────────────────────────

    /// vLLM — high-throughput OpenAI-compatible inference server.
    ///
    /// Reads: `ANIMA_VLLM_URL`, `ANIMA_VLLM_MODEL`, `ANIMA_VLLM_API_KEY`,
    /// `ANIMA_VLLM_CTX`, `ANIMA_VLLM_TIMEOUT`.
    pub fn vllm() -> Self {
        let config = ProviderConfig::from_env_prefix(
            "vllm",
            "ANIMA_VLLM",
            "http://vllm:8000/v1",
            "mistral-7b-instruct",
            32_768,
            BackendCapabilities::openai_compat(),
        );
        Self::from_config(config)
    }

    /// LM Studio — desktop OpenAI-compatible server.
    ///
    /// Reads: `ANIMA_LMSTUDIO_URL`, `ANIMA_LMSTUDIO_MODEL`, `ANIMA_LMSTUDIO_CTX`,
    /// `ANIMA_LMSTUDIO_TIMEOUT`.
    pub fn lmstudio() -> Self {
        let config = ProviderConfig::from_env_prefix(
            "lmstudio",
            "ANIMA_LMSTUDIO",
            "http://localhost:1234/v1",
            "local-model",
            8_192,
            BackendCapabilities::openai_compat(),
        );
        Self::from_config(config)
    }

    /// NVIDIA NIM — NIM microservice (OpenAI-compatible).
    ///
    /// Reads: `ANIMA_NIM_URL`, `ANIMA_NIM_MODEL`, `ANIMA_NIM_API_KEY`,
    /// `ANIMA_NIM_CTX`, `ANIMA_NIM_TIMEOUT`.
    pub fn nvidia_nim() -> Self {
        let config = ProviderConfig::from_env_prefix(
            "nvidia-nim",
            "ANIMA_NIM",
            "http://localhost:8000/v1",
            "meta/llama-3.1-8b-instruct",
            131_072,
            BackendCapabilities::full(),
        );
        Self::from_config(config)
    }

    /// Hugging Face Text Generation Inference — Messages API.
    ///
    /// Reads: `ANIMA_TGI_URL`, `ANIMA_TGI_MODEL`, `ANIMA_TGI_API_KEY`,
    /// `ANIMA_TGI_CTX`, `ANIMA_TGI_TIMEOUT`.
    pub fn hf_tgi() -> Self {
        let config = ProviderConfig::from_env_prefix(
            "hf-tgi",
            "ANIMA_TGI",
            "http://tgi:8080/v1",
            "tgi-model",
            8_192,
            BackendCapabilities::openai_compat(),
        );
        Self::from_config(config)
    }

    /// llama.cpp server — `llama-server --api`.
    ///
    /// Reads: `ANIMA_LLAMACPP_URL`, `ANIMA_LLAMACPP_MODEL`, `ANIMA_LLAMACPP_CTX`,
    /// `ANIMA_LLAMACPP_TIMEOUT`.
    pub fn llamacpp_server() -> Self {
        let config = ProviderConfig::from_env_prefix(
            "llamacpp-server",
            "ANIMA_LLAMACPP",
            "http://localhost:8080/v1",
            "local-gguf",
            8_192,
            // llamacpp-server supports tools from v0.0.2185+; conservative default.
            BackendCapabilities {
                tools: true,
                streaming: true,
                embeddings: false,
                json_mode: true,
                vision: false,
            },
        );
        Self::from_config(config)
    }

    // ── Capability access ─────────────────────────────────────────────────────

    /// Returns the backend's capability descriptor.
    pub fn capabilities(&self) -> &BackendCapabilities {
        &self.config.capabilities
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    fn build_agent(timeout: Duration) -> ureq::Agent {
        ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .build()
            .into()
    }

    fn chat_endpoint(&self) -> String {
        format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        )
    }

    fn models_endpoint(&self) -> String {
        format!("{}/models", self.config.base_url.trim_end_matches('/'))
    }

    /// Performs a live `/v1/chat/completions` request (blocking via ureq).
    fn live_chat_complete(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        cancel: &CancellationToken,
    ) -> Result<ChatResponse, LlmBackendError> {
        if cancel.is_cancelled() {
            return Err(LlmBackendError::Cancelled);
        }

        // Build the message array.
        let msg_array: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| {
                let role = match m.role {
                    ChatRole::System => "system",
                    ChatRole::User => "user",
                    ChatRole::Assistant => "assistant",
                    ChatRole::Tool => "tool",
                };
                let mut v = serde_json::json!({
                    "role": role,
                    "content": m.content,
                });
                if let Some(id) = &m.tool_call_id {
                    v["tool_call_id"] = serde_json::Value::String(id.clone());
                }
                // Assistant turns that requested tools must replay those calls so
                // the following tool-result messages are valid for OpenAI-compatible
                // providers (correlated by id). Skipped when empty.
                if !m.tool_calls.is_empty() {
                    v["tool_calls"] = serde_json::Value::Array(
                        m.tool_calls
                            .iter()
                            .map(|tc| {
                                serde_json::json!({
                                    "id": tc.id,
                                    "type": "function",
                                    "function": {
                                        "name": tc.name,
                                        "arguments": tc.arguments,
                                    },
                                })
                            })
                            .collect(),
                    );
                }
                v
            })
            .collect();

        let mut body = serde_json::json!({
            "model": self.config.model,
            "messages": msg_array,
            "stream": false,
        });

        // Attach tool definitions when supported and provided.
        if self.config.capabilities.tools && !tools.is_empty() {
            let tool_array: Vec<serde_json::Value> = tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters,
                        }
                    })
                })
                .collect();
            body["tools"] = serde_json::Value::Array(tool_array);
            body["tool_choice"] = serde_json::Value::String("auto".to_string());
        }

        // Rebuild + send inside the retry closure so a transient connect/429/5xx
        // failure is retried with backoff instead of aborting the task (IO-2).
        let body_str = body.to_string();
        let response = crate::retry::with_retry(&crate::retry::RetryPolicy::default(), || {
            let mut request = self
                .agent
                .post(self.chat_endpoint())
                .header("Content-Type", "application/json");
            if let Some(key) = &self.config.api_key {
                request = request.header("Authorization", format!("Bearer {key}").as_str());
            }
            request.send(body_str.clone())
        })
        .map_err(|e| {
            LlmBackendError::Provider(format!("{}: request failed: {e}", self.config.id))
        })?;

        let text = response.into_body().read_to_string().map_err(|e| {
            LlmBackendError::Provider(format!("{}: read body: {e}", self.config.id))
        })?;

        self.parse_chat_response(&text)
    }

    /// Parses a `/v1/chat/completions` JSON response into a [`ChatResponse`].
    fn parse_chat_response(&self, json: &str) -> Result<ChatResponse, LlmBackendError> {
        let val: serde_json::Value = serde_json::from_str(json).map_err(|e| {
            LlmBackendError::Provider(format!("{}: invalid JSON: {e}", self.config.id))
        })?;

        // Surface provider-level errors.
        if let Some(err) = val.get("error") {
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown provider error");
            return Err(LlmBackendError::Provider(format!(
                "{}: {}",
                self.config.id, msg
            )));
        }

        let choice = val
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .ok_or_else(|| {
                LlmBackendError::Provider(format!("{}: no choices in response", self.config.id))
            })?;

        let message = choice.get("message").ok_or_else(|| {
            LlmBackendError::Provider(format!("{}: no message in choice", self.config.id))
        })?;

        let content = message
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();

        // Parse tool calls if present.
        let tool_calls: Vec<ToolCall> = message
            .get("tool_calls")
            .and_then(|tc| tc.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|tc| {
                        let id = tc.get("id")?.as_str()?.to_string();
                        let func = tc.get("function")?;
                        let name = func.get("name")?.as_str()?.to_string();
                        let arguments = func
                            .get("arguments")
                            .and_then(|a| a.as_str())
                            .unwrap_or("{}")
                            .to_string();
                        Some(ToolCall {
                            id,
                            name,
                            arguments,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let finish_reason = match choice
            .get("finish_reason")
            .and_then(|r| r.as_str())
            .unwrap_or("stop")
        {
            "stop" => FinishReason::Stop,
            "tool_calls" => FinishReason::ToolCalls,
            "length" => FinishReason::MaxTokens,
            other => FinishReason::Other(other.to_string()),
        };

        let model = val
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or(&self.config.model)
            .to_string();

        let usage_tokens = val
            .get("usage")
            .and_then(|u| u.get("total_tokens"))
            .and_then(|t| t.as_u64())
            .map(|t| t as u32);

        Ok(ChatResponse {
            content,
            tool_calls,
            finish_reason,
            model,
            usage_tokens,
        })
    }
}

// ── LlmBackend implementation ─────────────────────────────────────────────────

impl LlmBackend for OpenAiCompatibleBackend {
    fn id(&self) -> &'static str {
        // Leak the id string at most once per backend instance to satisfy the
        // `&'static str` contract.  Subsequent calls return the cached pointer
        // without leaking, so `id()` is safe on the hot dispatch path.
        self.id_cache
            .get_or_init(|| Box::leak(self.config.id.clone().into_boxed_str()))
    }

    fn model_id(&self) -> &str {
        &self.config.model
    }

    fn max_context_tokens(&self) -> u32 {
        self.config.max_context_tokens
    }

    fn stream_completion<'a>(
        &'a self,
        prompt: &'a str,
        cancel: &'a CancellationToken,
    ) -> CompletionFuture<'a> {
        Box::pin(async move {
            match &self.mode {
                BackendMode::Fixture(map) => {
                    if cancel.is_cancelled() {
                        return Err(LlmBackendError::Cancelled);
                    }
                    let tokens = map
                        .get(prompt)
                        .cloned()
                        .unwrap_or_else(|| vec!["[compat-fixture-not-found]".to_string()]);
                    let mut events: Vec<StreamingCompletion> = Vec::with_capacity(tokens.len() + 1);
                    for token in tokens {
                        if cancel.is_cancelled() {
                            return Err(LlmBackendError::Cancelled);
                        }
                        events.push(StreamingCompletion::Token(token));
                    }
                    events.push(StreamingCompletion::Done);
                    Ok(events)
                }
                BackendMode::Live => {
                    // For live mode, convert the prompt to a user message and
                    // use the chat endpoint.  Streaming is available on the
                    // `/v1/chat/completions` endpoint with `stream: true`, but
                    // since we wrap blocking ureq inside an async block anyway,
                    // we use `stream: false` for simplicity here.
                    if cancel.is_cancelled() {
                        return Err(LlmBackendError::Cancelled);
                    }
                    let msg = ChatMessage::user(prompt);
                    let resp = self.live_chat_complete(&[msg], &[], cancel)?;
                    let mut events = Vec::new();
                    // Emit word-level chunks rather than the whole answer as a
                    // single token, so the scheduler's per-token accounting and
                    // budget approximate the real length instead of counting the
                    // entire response as one token (IO-4). `split_inclusive`
                    // keeps the trailing spaces so the chunks concatenate back to
                    // the exact content.
                    for chunk in resp.content.split_inclusive(' ') {
                        events.push(StreamingCompletion::Token(chunk.to_string()));
                    }
                    events.push(StreamingCompletion::Done);
                    Ok(events)
                }
            }
        })
    }
}

// ── ChatBackend implementation ────────────────────────────────────────────────

impl ChatBackend for OpenAiCompatibleBackend {
    fn chat_complete(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        cancel: &CancellationToken,
    ) -> Result<ChatResponse, LlmBackendError> {
        match &self.mode {
            BackendMode::Fixture(map) => {
                // Use the last user message as the fixture key.
                let key = messages
                    .iter()
                    .rev()
                    .find(|m| m.role == ChatRole::User)
                    .map(|m| m.content.as_str())
                    .unwrap_or("");
                let tokens = map
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| vec!["[compat-fixture-not-found]".to_string()]);
                // Append tool suffix to the first token to simulate awareness.
                let suffix = tools_to_prompt_suffix(tools);
                let content = format!("{}{}", tokens.join(""), suffix);
                Ok(ChatResponse {
                    content,
                    tool_calls: vec![],
                    finish_reason: FinishReason::Stop,
                    model: self.config.model.clone(),
                    usage_tokens: None,
                })
            }
            BackendMode::Live => self.live_chat_complete(messages, tools, cancel),
        }
    }

    /// Hits `/v1/models` to verify the server is reachable.
    ///
    /// In fixture mode always returns `true` (no network).
    /// In live mode attempts a GET; returns `false` on any error.
    fn health(&self) -> bool {
        match &self.mode {
            BackendMode::Fixture(_) => true,
            BackendMode::Live => {
                let mut req = self.agent.get(self.models_endpoint());
                if let Some(key) = &self.config.api_key {
                    req = req.header("Authorization", format!("Bearer {key}").as_str());
                }
                req.call().is_ok()
            }
        }
    }
}

// ── Streaming helper (live SSE path — future enhancement) ─────────────────────

/// Reads a live SSE stream from an `openai`-compatible `/v1/chat/completions`
/// endpoint with `stream: true` and accumulates tokens.
///
/// This is a streaming alternative to the blocking non-stream path used in
/// [`live_chat_complete`].  Currently unused (the non-stream path is simpler
/// for the sync trait); retained for future enablement.
#[allow(dead_code)]
fn parse_sse_stream(
    reader: BufReader<impl std::io::Read>,
    cancel: &CancellationToken,
) -> Result<Vec<StreamingCompletion>, LlmBackendError> {
    let mut events = Vec::new();
    for line in reader.lines() {
        if cancel.is_cancelled() {
            return Err(LlmBackendError::Cancelled);
        }
        let line = line.map_err(|e| LlmBackendError::Provider(format!("sse read: {e}")))?;
        if line.is_empty() || line == "data: [DONE]" {
            continue;
        }
        let data = line.strip_prefix("data: ").unwrap_or(&line);
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(data) {
            if let Some(token) = val
                .get("choices")
                .and_then(|c| c.as_array())
                .and_then(|a| a.first())
                .and_then(|ch| ch.get("delta"))
                .and_then(|d| d.get("content"))
                .and_then(|c| c.as_str())
            {
                if !token.is_empty() {
                    events.push(StreamingCompletion::Token(token.to_owned()));
                }
            }
        }
    }
    events.push(StreamingCompletion::Done);
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_fixture_backend(pairs: &[(&str, &str)]) -> OpenAiCompatibleBackend {
        let fixtures = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), vec![v.to_string()]));
        let config = ProviderConfig::from_env_prefix(
            "test",
            "ANIMA_TEST_COMPAT",
            "http://localhost:8000/v1",
            "test-model",
            4096,
            BackendCapabilities::openai_compat(),
        );
        OpenAiCompatibleBackend::fixture(config, fixtures)
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        use std::pin::Pin;
        use std::task::{Context, Poll, Waker};
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut f = Box::pin(f);
        loop {
            match Pin::as_mut(&mut f).poll(&mut cx) {
                Poll::Ready(v) => return v,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    #[test]
    fn vllm_preset_has_correct_id() {
        let b = OpenAiCompatibleBackend::vllm();
        assert_eq!(b.id(), "vllm");
    }

    #[test]
    fn lmstudio_preset_has_correct_id() {
        let b = OpenAiCompatibleBackend::lmstudio();
        assert_eq!(b.id(), "lmstudio");
    }

    #[test]
    fn nvidia_nim_preset_has_correct_id() {
        let b = OpenAiCompatibleBackend::nvidia_nim();
        assert_eq!(b.id(), "nvidia-nim");
    }

    #[test]
    fn hf_tgi_preset_has_correct_id() {
        let b = OpenAiCompatibleBackend::hf_tgi();
        assert_eq!(b.id(), "hf-tgi");
    }

    #[test]
    fn llamacpp_server_preset_has_correct_id() {
        let b = OpenAiCompatibleBackend::llamacpp_server();
        assert_eq!(b.id(), "llamacpp-server");
    }

    #[test]
    fn fixture_mode_replays_recorded_tokens() {
        let backend = make_fixture_backend(&[("ping", "pong")]);
        let cancel = CancellationToken::new();
        let events = block_on(backend.stream_completion("ping", &cancel)).unwrap();
        let has_pong = events
            .iter()
            .any(|e| matches!(e, StreamingCompletion::Token(t) if t == "pong"));
        assert!(has_pong);
        assert!(matches!(events.last(), Some(StreamingCompletion::Done)));
    }

    #[test]
    fn unknown_prompt_returns_sentinel_token() {
        let backend = make_fixture_backend(&[]);
        let cancel = CancellationToken::new();
        let events = block_on(backend.stream_completion("no-such-prompt", &cancel)).unwrap();
        let has_sentinel = events.iter().any(|e| {
            matches!(e, StreamingCompletion::Token(t) if t.contains("compat-fixture-not-found"))
        });
        assert!(has_sentinel);
    }

    #[test]
    fn cancellation_returns_cancelled_error() {
        let backend = make_fixture_backend(&[("hi", "hello")]);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = block_on(backend.stream_completion("hi", &cancel)).unwrap_err();
        assert_eq!(err, LlmBackendError::Cancelled);
    }

    #[test]
    fn fixture_mode_is_reproducible() {
        let backend = make_fixture_backend(&[("hello", "world")]);
        let c1 = CancellationToken::new();
        let c2 = CancellationToken::new();
        let r1 = block_on(backend.stream_completion("hello", &c1)).unwrap();
        let r2 = block_on(backend.stream_completion("hello", &c2)).unwrap();
        assert_eq!(r1, r2);
    }

    #[test]
    fn chat_complete_fixture_returns_expected_content() {
        let backend = make_fixture_backend(&[("What is 2+2?", "4")]);
        let cancel = CancellationToken::new();
        let messages = vec![ChatMessage::user("What is 2+2?")];
        let resp = backend.chat_complete(&messages, &[], &cancel).unwrap();
        assert!(resp.content.contains("4"));
        assert_eq!(resp.finish_reason, FinishReason::Stop);
    }

    #[test]
    fn chat_complete_fixture_with_tools_appends_suffix() {
        let backend = make_fixture_backend(&[("search", "results")]);
        let cancel = CancellationToken::new();
        let messages = vec![ChatMessage::user("search")];
        let tools = vec![ToolSpec {
            name: "web-search".to_string(),
            description: "Search the web".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let resp = backend.chat_complete(&messages, &tools, &cancel).unwrap();
        assert!(resp.content.contains("web-search"));
    }

    #[test]
    fn health_returns_true_in_fixture_mode() {
        let backend = make_fixture_backend(&[]);
        assert!(backend.health());
    }

    #[test]
    fn capabilities_reflect_config() {
        let backend = make_fixture_backend(&[]);
        assert!(backend.capabilities().tools);
        assert!(backend.capabilities().embeddings);
    }

    #[test]
    fn model_id_reflects_config_model() {
        let backend = make_fixture_backend(&[]);
        assert_eq!(backend.model_id(), "test-model");
    }

    #[test]
    fn max_context_tokens_reflects_config() {
        let backend = make_fixture_backend(&[]);
        assert_eq!(backend.max_context_tokens(), 4096);
    }

    #[test]
    fn parse_chat_response_extracts_text_content() {
        let config = ProviderConfig::from_env_prefix(
            "test",
            "ANIMA_TEST_COMPAT",
            "http://localhost/v1",
            "m",
            4096,
            BackendCapabilities::text_only(),
        );
        let backend = OpenAiCompatibleBackend::fixture(config, []);
        let json = serde_json::json!({
            "model": "test-model",
            "choices": [{
                "message": {"role": "assistant", "content": "Hello!"},
                "finish_reason": "stop"
            }],
            "usage": {"total_tokens": 10}
        })
        .to_string();
        let resp = backend.parse_chat_response(&json).unwrap();
        assert_eq!(resp.content, "Hello!");
        assert_eq!(resp.finish_reason, FinishReason::Stop);
        assert_eq!(resp.usage_tokens, Some(10));
        assert!(resp.tool_calls.is_empty());
    }

    #[test]
    fn parse_chat_response_extracts_tool_calls() {
        let config = ProviderConfig::from_env_prefix(
            "test",
            "ANIMA_TEST_COMPAT",
            "http://localhost/v1",
            "m",
            4096,
            BackendCapabilities::openai_compat(),
        );
        let backend = OpenAiCompatibleBackend::fixture(config, []);
        let json = serde_json::json!({
            "model": "test-model",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "web-search",
                            "arguments": r#"{"query":"rust"}"#
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })
        .to_string();
        let resp = backend.parse_chat_response(&json).unwrap();
        assert_eq!(resp.finish_reason, FinishReason::ToolCalls);
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "web-search");
        assert_eq!(resp.tool_calls[0].id, "call_1");
    }

    #[test]
    fn parse_chat_response_surfaces_provider_error() {
        let config = ProviderConfig::from_env_prefix(
            "test",
            "ANIMA_TEST_COMPAT",
            "http://localhost/v1",
            "m",
            4096,
            BackendCapabilities::text_only(),
        );
        let backend = OpenAiCompatibleBackend::fixture(config, []);
        let json = serde_json::json!({
            "error": {"message": "model not found", "type": "invalid_request_error"}
        })
        .to_string();
        let err = backend.parse_chat_response(&json).unwrap_err();
        assert!(matches!(err, LlmBackendError::Provider(msg) if msg.contains("model not found")));
    }

    #[test]
    fn sse_parser_accumulates_tokens_and_skips_done_sentinel() {
        use std::io::Cursor;
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\
                   data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\
                   data: [DONE]\n";
        let reader = BufReader::new(Cursor::new(sse));
        let cancel = CancellationToken::new();
        let events = parse_sse_stream(reader, &cancel).unwrap();
        let tokens: Vec<&str> = events
            .iter()
            .filter_map(|e| {
                if let StreamingCompletion::Token(t) = e {
                    Some(t.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(tokens, vec!["Hello", " world"]);
        assert!(matches!(events.last(), Some(StreamingCompletion::Done)));
    }

    #[test]
    fn chat_complete_fixture_uses_last_user_message_as_key() {
        let backend = make_fixture_backend(&[("what time?", "it is noon")]);
        let cancel = CancellationToken::new();
        let messages = vec![
            ChatMessage::system("You are helpful."),
            ChatMessage::user("what time?"),
        ];
        let resp = backend.chat_complete(&messages, &[], &cancel).unwrap();
        assert!(resp.content.contains("it is noon"));
    }
}
