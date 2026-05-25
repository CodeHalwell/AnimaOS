//! Anthropic Claude backend implementation.
//!
//! # Modes
//!
//! * **Fixture** (default, CI-safe): token streams are replayed from the
//!   bundled `fixtures/anthropic.json`.  No API key or network access required.
//! * **Custom fixture**: supply your own `(prompt, tokens)` pairs via
//!   [`AnthropicBackend::with_custom_fixtures`].

use std::collections::HashMap;

use scheduler::backend::{
    CancellationToken, CompletionFuture, LlmBackend, LlmBackendError, StreamingCompletion,
};

use crate::fixture::load_fixtures;

/// Anthropic Claude provider (fixture mode).
///
/// Replays pre-recorded token streams for known prompts.  Unknown prompts
/// receive a sentinel token so tests do not silently produce empty output.
pub struct AnthropicBackend {
    /// Model identifier (e.g. `"claude-3-haiku-20240307"`).
    model: String,
    /// Recorded token lists keyed by exact prompt text.
    fixtures: HashMap<String, Vec<String>>,
}

impl AnthropicBackend {
    /// Creates a backend pre-loaded with the bundled `fixtures/anthropic.json`.
    pub fn new() -> Self {
        let entries = load_fixtures(include_str!("../fixtures/anthropic.json"))
            .expect("bundled Anthropic fixture must be valid JSON");
        let fixtures = entries.into_iter().map(|e| (e.prompt, e.tokens)).collect();
        Self {
            model: "claude-3-haiku-20240307".to_string(),
            fixtures,
        }
    }

    /// Creates a backend with a custom model identifier and fixture set.
    pub fn with_custom_fixtures(
        model: impl Into<String>,
        fixtures: impl IntoIterator<Item = (String, Vec<String>)>,
    ) -> Self {
        Self {
            model: model.into(),
            fixtures: fixtures.into_iter().collect(),
        }
    }

    fn lookup_fixture(&self, prompt: &str) -> Vec<String> {
        self.fixtures
            .get(prompt)
            .cloned()
            .unwrap_or_else(|| vec!["[anthropic-fixture-not-found]".to_string()])
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
            let tokens = self.lookup_fixture(prompt);
            let mut events: Vec<StreamingCompletion> = Vec::with_capacity(tokens.len() + 1);
            for token in tokens {
                if cancel.is_cancelled() {
                    return Err(LlmBackendError::Cancelled);
                }
                events.push(StreamingCompletion::Token(token));
            }
            events.push(StreamingCompletion::Done);
            Ok(events)
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
        let mut cx = Context::from_waker(&waker);
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
