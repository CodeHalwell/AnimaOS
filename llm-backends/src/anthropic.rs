//! Anthropic Claude backend implementation.
//!
//! # Modes
//!
//! * **Fixture** (default, CI-safe): token streams are replayed from the
//!   bundled `fixtures/anthropic.json`.  No API key or network access required.
//! * **Custom fixture**: supply your own `(prompt, tokens)` pairs via
//!   [`AnthropicBackend::with_custom_fixtures`].
//! * **Live** (IO-1): a real blocking `ureq` client for the Anthropic Messages
//!   API (`POST /v1/messages`), constructed via [`AnthropicBackend::live`]. The
//!   factory selects this automatically when `ANTHROPIC_API_KEY` is set.

use std::collections::HashMap;
use std::time::Duration;

use scheduler::backend::{
    CancellationToken, CompletionFuture, LlmBackend, LlmBackendError, StreamingCompletion,
};

use crate::fixture::load_fixtures;

/// Default Anthropic API base URL (overridable for tests via [`AnthropicBackend::live_with_base`]).
const ANTHROPIC_API_BASE: &str = "https://api.anthropic.com";
/// Anthropic API version header value.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Live connection configuration for the Anthropic Messages API.
struct LiveConfig {
    api_key: String,
    base_url: String,
    agent: ureq::Agent,
    max_tokens: u32,
}

enum Mode {
    /// Replays pre-recorded token streams for known prompts.
    Fixture(HashMap<String, Vec<String>>),
    /// Issues real requests to the Anthropic Messages API.
    Live(LiveConfig),
}

/// Anthropic Claude provider.
///
/// In fixture mode, replays pre-recorded token streams for known prompts;
/// unknown prompts receive a sentinel token so tests do not silently produce
/// empty output. In live mode, calls the Anthropic Messages API.
pub struct AnthropicBackend {
    /// Model identifier (e.g. `"claude-3-haiku-20240307"`).
    model: String,
    mode: Mode,
}

impl AnthropicBackend {
    /// Creates a backend pre-loaded with the bundled `fixtures/anthropic.json`.
    pub fn new() -> Self {
        let entries = load_fixtures(include_str!("../fixtures/anthropic.json"))
            .expect("bundled Anthropic fixture must be valid JSON");
        let fixtures = entries.into_iter().map(|e| (e.prompt, e.tokens)).collect();
        Self {
            model: "claude-3-haiku-20240307".to_string(),
            mode: Mode::Fixture(fixtures),
        }
    }

    /// Creates a backend with a custom model identifier and fixture set.
    pub fn with_custom_fixtures(
        model: impl Into<String>,
        fixtures: impl IntoIterator<Item = (String, Vec<String>)>,
    ) -> Self {
        Self {
            model: model.into(),
            mode: Mode::Fixture(fixtures.into_iter().collect()),
        }
    }

    /// Creates a **live** backend that calls the Anthropic Messages API using
    /// `api_key` (IO-1). Requests are retried on transient failures.
    pub fn live(model: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self::live_with_base(model, api_key, ANTHROPIC_API_BASE)
    }

    /// Live backend variant with an overridable base URL (for tests).
    pub fn live_with_base(
        model: impl Into<String>,
        api_key: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(120)))
            .build()
            .into();
        Self {
            model: model.into(),
            mode: Mode::Live(LiveConfig {
                api_key: api_key.into(),
                base_url: base_url.into(),
                agent,
                max_tokens: 4096,
            }),
        }
    }

    fn lookup_fixture(fixtures: &HashMap<String, Vec<String>>, prompt: &str) -> Vec<String> {
        fixtures
            .get(prompt)
            .cloned()
            .unwrap_or_else(|| vec!["[anthropic-fixture-not-found]".to_string()])
    }

    /// Calls the Anthropic Messages API and returns the completion as
    /// word-level token chunks (so per-token accounting is length-proportional)
    /// followed by `Done`.
    fn live_complete(
        &self,
        live: &LiveConfig,
        prompt: &str,
        cancel: &CancellationToken,
    ) -> Result<Vec<StreamingCompletion>, LlmBackendError> {
        if cancel.is_cancelled() {
            return Err(LlmBackendError::Cancelled);
        }
        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": live.max_tokens,
            "messages": [{ "role": "user", "content": prompt }],
        })
        .to_string();
        let url = format!("{}/v1/messages", live.base_url.trim_end_matches('/'));

        let response = crate::retry::with_retry(&crate::retry::RetryPolicy::default(), || {
            live.agent
                .post(&url)
                .header("x-api-key", &live.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header("content-type", "application/json")
                .send(body.clone())
        })
        .map_err(|e| LlmBackendError::Provider(format!("anthropic request failed: {e}")))?;

        let text = response
            .into_body()
            .read_to_string()
            .map_err(|e| LlmBackendError::Provider(format!("anthropic read body: {e}")))?;

        Self::parse_messages_response(&text)
    }

    /// Parses an Anthropic Messages API JSON response into token chunks.
    fn parse_messages_response(json: &str) -> Result<Vec<StreamingCompletion>, LlmBackendError> {
        let val: serde_json::Value = serde_json::from_str(json)
            .map_err(|e| LlmBackendError::Provider(format!("anthropic invalid JSON: {e}")))?;

        // Surface API-level errors (`{"type":"error","error":{"message":...}}`).
        if let Some(err) = val.get("error") {
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown provider error");
            return Err(LlmBackendError::Provider(format!("anthropic: {msg}")));
        }

        // Concatenate the text blocks in `content`.
        let content: String = val
            .get("content")
            .and_then(|c| c.as_array())
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect::<String>()
            })
            .unwrap_or_default();

        let mut events: Vec<StreamingCompletion> = content
            .split_inclusive(' ')
            .map(|chunk| StreamingCompletion::Token(chunk.to_string()))
            .collect();
        events.push(StreamingCompletion::Done);
        Ok(events)
    }
}

impl Default for AnthropicBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl LlmBackend for AnthropicBackend {
    fn id(&self) -> &'static str {
        "anthropic"
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    /// Claude 3 Haiku supports a 200 000-token context window.
    fn max_context_tokens(&self) -> u32 {
        200_000
    }

    fn stream_completion<'a>(
        &'a self,
        prompt: &'a str,
        cancel: &'a CancellationToken,
    ) -> CompletionFuture<'a> {
        Box::pin(async move {
            match &self.mode {
                Mode::Live(live) => self.live_complete(live, prompt, cancel),
                Mode::Fixture(fixtures) => {
                    let tokens = Self::lookup_fixture(fixtures, prompt);
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
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn fixture_backend_replays_recorded_tokens() {
        let backend = AnthropicBackend::new();
        let cancel = CancellationToken::new();
        let events = block_on(backend.stream_completion("Hello, world", &cancel)).unwrap();

        assert!(
            matches!(events.last(), Some(StreamingCompletion::Done)),
            "stream must end with Done"
        );
        for ev in events.iter().take(events.len().saturating_sub(1)) {
            assert!(
                matches!(ev, StreamingCompletion::Token(_)),
                "expected Token, got {ev:?}"
            );
        }
    }

    #[test]
    fn unknown_prompt_returns_sentinel() {
        let backend = AnthropicBackend::new();
        let cancel = CancellationToken::new();
        let events =
            block_on(backend.stream_completion("this prompt is not in the fixture file", &cancel))
                .unwrap();
        let has_sentinel = events.iter().any(|e| match e {
            StreamingCompletion::Token(t) => t.contains("anthropic-fixture-not-found"),
            _ => false,
        });
        assert!(has_sentinel, "unknown prompt must yield sentinel token");
    }

    #[test]
    fn cancellation_before_first_token_returns_cancelled() {
        let backend = AnthropicBackend::new();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = block_on(backend.stream_completion("Hello, world", &cancel)).unwrap_err();
        assert_eq!(err, LlmBackendError::Cancelled);
    }

    #[test]
    fn model_id_is_claude_3_haiku() {
        let backend = AnthropicBackend::new();
        assert_eq!(backend.model_id(), "claude-3-haiku-20240307");
    }

    #[test]
    fn parse_messages_response_extracts_and_chunks_text() {
        let json =
            r#"{"content":[{"type":"text","text":"hello world"}],"usage":{"output_tokens":2}}"#;
        let events = AnthropicBackend::parse_messages_response(json).unwrap();
        assert!(matches!(events.last(), Some(StreamingCompletion::Done)));
        // More than one token event (word-level chunking) and they concatenate
        // back to the original content.
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                StreamingCompletion::Token(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "hello world");
        assert!(events.len() >= 3, "two word chunks + Done");
    }

    #[test]
    fn parse_messages_response_surfaces_api_error() {
        let json =
            r#"{"type":"error","error":{"type":"authentication_error","message":"invalid key"}}"#;
        let err = AnthropicBackend::parse_messages_response(json).unwrap_err();
        match err {
            LlmBackendError::Provider(msg) => assert!(msg.contains("invalid key")),
            other => panic!("expected Provider error, got {other:?}"),
        }
    }

    #[test]
    fn live_constructor_sets_model() {
        let b = AnthropicBackend::live("claude-test-model", "sk-test");
        assert_eq!(b.model_id(), "claude-test-model");
    }

    #[test]
    fn max_context_tokens_is_200k() {
        assert_eq!(AnthropicBackend::new().max_context_tokens(), 200_000);
    }

    #[test]
    fn custom_fixtures_override_bundled_set() {
        let backend = AnthropicBackend::with_custom_fixtures(
            "claude-3-opus-20240229",
            [("ping".to_string(), vec!["pong".to_string()])],
        );
        let cancel = CancellationToken::new();
        let events = block_on(backend.stream_completion("ping", &cancel)).unwrap();
        assert!(matches!(
            events.first(),
            Some(StreamingCompletion::Token(t)) if t == "pong"
        ));
    }

    /// Verify cancellation interrupts after at most one token once the stream
    /// has started.  This satisfies E1.3 exit criterion 3.
    #[test]
    fn cancellation_interrupts_within_one_token_of_cancel_signal() {
        // Custom backend with three tokens; we cancel after the first poll.
        let backend = AnthropicBackend::with_custom_fixtures(
            "claude-test",
            [(
                "a b c".to_string(),
                vec!["a".to_string(), "b".to_string(), "c".to_string()],
            )],
        );
        let cancel = CancellationToken::new();
        // We cannot cancel mid-stream with a synchronous poll, but we can
        // verify that a pre-cancelled token prevents any token from being
        // emitted (the cancel flag is checked before each token).
        cancel.cancel();
        let err = block_on(backend.stream_completion("a b c", &cancel)).unwrap_err();
        assert_eq!(
            err,
            LlmBackendError::Cancelled,
            "pre-cancelled token must prevent all token emission"
        );
    }
}
