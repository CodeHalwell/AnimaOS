//! OpenAI GPT backend implementation.
//!
//! # Modes
//!
//! * **Fixture** (default, CI-safe): token streams are replayed from the
//!   bundled `fixtures/openai.json`.  No API key or network access required.
//! * **Custom fixture**: supply your own `(prompt, tokens)` pairs via
//!   [`OpenAiBackend::with_custom_fixtures`].

use std::collections::HashMap;

use scheduler::backend::{
    CancellationToken, CompletionFuture, LlmBackend, LlmBackendError, StreamingCompletion,
};

use crate::fixture::load_fixtures;

/// OpenAI GPT provider (fixture mode).
///
/// Replays pre-recorded token streams for known prompts.  Unknown prompts
/// receive a sentinel token so tests do not silently produce empty output.
pub struct OpenAiBackend {
    /// Model identifier (e.g. `"gpt-4o-mini"`).
    model: String,
    /// Recorded token lists keyed by exact prompt text.
    fixtures: HashMap<String, Vec<String>>,
}

impl OpenAiBackend {
    /// Creates a backend pre-loaded with the bundled `fixtures/openai.json`.
    pub fn new() -> Self {
        let entries = load_fixtures(include_str!("../fixtures/openai.json"))
            .expect("bundled OpenAI fixture must be valid JSON");
        let fixtures = entries.into_iter().map(|e| (e.prompt, e.tokens)).collect();
        Self {
            model: "gpt-4o-mini".to_string(),
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
            .unwrap_or_else(|| vec!["[openai-fixture-not-found]".to_string()])
    }
}

impl Default for OpenAiBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl LlmBackend for OpenAiBackend {
    fn id(&self) -> &'static str {
        "openai"
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    /// GPT-4o-mini supports a 128 000-token context window.
    fn max_context_tokens(&self) -> u32 {
        128_000
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
        let backend = OpenAiBackend::new();
        let cancel = CancellationToken::new();
        let events = block_on(backend.stream_completion("Hello, world", &cancel)).unwrap();

        assert!(matches!(events.last(), Some(StreamingCompletion::Done)));
        for ev in events.iter().take(events.len().saturating_sub(1)) {
            assert!(matches!(ev, StreamingCompletion::Token(_)));
        }
    }

    #[test]
    fn unknown_prompt_returns_sentinel() {
        let backend = OpenAiBackend::new();
        let cancel = CancellationToken::new();
        let events =
            block_on(backend.stream_completion("completely unknown prompt xyz", &cancel)).unwrap();
        let has_sentinel = events.iter().any(|e| match e {
            StreamingCompletion::Token(t) => t.contains("openai-fixture-not-found"),
            _ => false,
        });
        assert!(has_sentinel);
    }

    #[test]
    fn cancellation_before_first_token_returns_cancelled() {
        let backend = OpenAiBackend::new();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = block_on(backend.stream_completion("Hello, world", &cancel)).unwrap_err();
        assert_eq!(err, LlmBackendError::Cancelled);
    }

    #[test]
    fn model_id_is_gpt_4o_mini() {
        assert_eq!(OpenAiBackend::new().model_id(), "gpt-4o-mini");
    }

    #[test]
    fn max_context_tokens_is_128k() {
        assert_eq!(OpenAiBackend::new().max_context_tokens(), 128_000);
    }

    #[test]
    fn custom_fixtures_override_bundled_set() {
        let backend = OpenAiBackend::with_custom_fixtures(
            "gpt-4o",
            [("ping".to_string(), vec!["pong".to_string()])],
        );
        let cancel = CancellationToken::new();
        let events = block_on(backend.stream_completion("ping", &cancel)).unwrap();
        assert!(matches!(
            events.first(),
            Some(StreamingCompletion::Token(t)) if t == "pong"
        ));
    }

    /// Verify reproducible byte-for-byte output for the same fixture prompt.
    /// This satisfies E1.3 exit criterion 1 (deterministic mock streams).
    #[test]
    fn fixture_output_is_byte_for_byte_reproducible() {
        let backend = OpenAiBackend::new();
        let cancel_a = CancellationToken::new();
        let cancel_b = CancellationToken::new();
        let run_a = block_on(backend.stream_completion("Hello, world", &cancel_a)).unwrap();
        let run_b = block_on(backend.stream_completion("Hello, world", &cancel_b)).unwrap();
        assert_eq!(run_a, run_b, "fixture output must be deterministic");
    }
}
