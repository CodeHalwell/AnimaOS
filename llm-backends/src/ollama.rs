//! Ollama backend — live local-inference path via an HTTP sidecar.
//!
//! [`OllamaBackend`] talks to a running `ollama serve` process (typically the
//! sibling compose service `ollama:11434`) over HTTP and streams the model's
//! token output back through the [`LlmBackend`] contract.  Ollama wraps
//! `llama.cpp` under the hood, so on an Ampere-class GPU (e.g. RTX 3090) the
//! 3-13 B workhorse and 270 M / sub-billion instinct models both run on the
//! card's tensor cores without AnimaOS having to ship any inference code of
//! its own.
//!
//! The backend is intentionally synchronous internally: the workspace has no
//! tokio runtime, and the hand-rolled `block_on` used by `kernels/hosted`
//! cannot drive a real async HTTP client.  Wrapping a blocking [`ureq`] call
//! inside an `async` block keeps the same future-returning shape as the
//! Anthropic / OpenAI backends while doing all I/O on the calling thread.

use std::io::{BufRead, BufReader};
use std::time::Duration;

use scheduler::backend::{
    CancellationToken, CompletionFuture, LlmBackend, LlmBackendError, StreamingCompletion,
};

/// HTTP client wrapper around a local Ollama daemon.
pub struct OllamaBackend {
    base_url: String,
    model: String,
    max_ctx: u32,
    /// Held for completeness / future tuning; the timeout is already baked
    /// into [`OllamaBackend::agent`].
    #[allow(dead_code)]
    request_timeout: Duration,
    /// Long-lived HTTP agent.  Sharing it across calls gives us
    /// connection pooling + keep-alive — important once the agenda starts
    /// firing many short prompts at the workhorse.
    agent: ureq::Agent,
}

impl OllamaBackend {
    /// Construct a backend pointed at an explicit Ollama URL + model tag.
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        let request_timeout = Duration::from_secs(300);
        Self {
            base_url: base_url.into(),
            model: model.into(),
            max_ctx: 8_192,
            request_timeout,
            agent: Self::build_agent(request_timeout),
        }
    }

    /// Construct from environment variables.
    ///
    /// | Variable               | Default                  |
    /// |------------------------|--------------------------|
    /// | `ANIMA_OLLAMA_URL`     | `http://ollama:11434`    |
    /// | `ANIMA_OLLAMA_MODEL`   | `llama3.2:3b`            |
    /// | `ANIMA_OLLAMA_CTX`     | `8192`                   |
    /// | `ANIMA_OLLAMA_TIMEOUT` | `300` (seconds)          |
    pub fn from_env() -> Self {
        let base_url =
            std::env::var("ANIMA_OLLAMA_URL").unwrap_or_else(|_| "http://ollama:11434".to_string());
        let model =
            std::env::var("ANIMA_OLLAMA_MODEL").unwrap_or_else(|_| "llama3.2:3b".to_string());
        let max_ctx = std::env::var("ANIMA_OLLAMA_CTX")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(8_192);
        let request_timeout = std::env::var("ANIMA_OLLAMA_TIMEOUT")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(300));
        Self {
            base_url,
            model,
            max_ctx,
            request_timeout,
            agent: Self::build_agent(request_timeout),
        }
    }

    fn build_agent(timeout: Duration) -> ureq::Agent {
        // ureq 3.x: configure the end-to-end (global) timeout via the
        // `ConfigBuilder` and convert the resulting `Config` into an `Agent`.
        ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .build()
            .into()
    }

    fn endpoint(&self) -> String {
        format!("{}/api/generate", self.base_url.trim_end_matches('/'))
    }
}

impl LlmBackend for OllamaBackend {
    fn id(&self) -> &'static str {
        "ollama"
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    fn max_context_tokens(&self) -> u32 {
        self.max_ctx
    }

    fn stream_completion<'a>(
        &'a self,
        prompt: &'a str,
        cancel: &'a CancellationToken,
    ) -> CompletionFuture<'a> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(LlmBackendError::Cancelled);
            }

            // Ollama honours `options.num_ctx` per-request, so the
            // `ANIMA_OLLAMA_CTX` env var actually shapes the model's
            // context window instead of only decorating `max_context_tokens()`.
            let body = serde_json::json!({
                "model": &self.model,
                "prompt": prompt,
                "stream": true,
                "options": {
                    "num_ctx": self.max_ctx,
                },
            });

            // ureq 3.x: `.header(..)` replaces `.set(..)`, and `.send(..)`
            // takes any `AsSendBody` (here the serialized JSON string) in
            // place of `.send_string(..)`.
            // Retry the connection/send on transient failures (IO-2); the
            // streamed body below is read once and is not retried mid-stream.
            let body_str = body.to_string();
            let mut response =
                crate::retry::with_retry(&crate::retry::RetryPolicy::default(), || {
                    self.agent
                        .post(self.endpoint())
                        .header("Content-Type", "application/json")
                        .send(body_str.clone())
                })
                .map_err(|e| LlmBackendError::Provider(format!("ollama request failed: {e}")))?;

            // `body_mut().as_reader()` is the streaming, unbounded equivalent
            // of ureq 2.x's `into_reader()` — appropriate for the unbounded
            // newline-delimited token stream Ollama emits.
            let reader = BufReader::new(response.body_mut().as_reader());
            let mut events: Vec<StreamingCompletion> = Vec::new();

            for line in reader.lines() {
                if cancel.is_cancelled() {
                    return Err(LlmBackendError::Cancelled);
                }
                let line = line
                    .map_err(|e| LlmBackendError::Provider(format!("ollama stream read: {e}")))?;
                if line.is_empty() {
                    continue;
                }
                let chunk: serde_json::Value = serde_json::from_str(&line).map_err(|e| {
                    LlmBackendError::Provider(format!("ollama chunk parse: {e} ({line})"))
                })?;
                // Ollama can terminate a stream with `{"error": "..."}` (often
                // with `done: true`).  Surface it so the caller doesn't see a
                // successful empty completion.
                if let Some(err) = chunk.get("error").and_then(|v| v.as_str()) {
                    return Err(LlmBackendError::Provider(format!(
                        "ollama stream error: {err}"
                    )));
                }
                if let Some(token) = chunk.get("response").and_then(|v| v.as_str()) {
                    if !token.is_empty() {
                        events.push(StreamingCompletion::Token(token.to_owned()));
                    }
                }
                if chunk.get("done").and_then(|v| v.as_bool()).unwrap_or(false) {
                    break;
                }
            }

            events.push(StreamingCompletion::Done);
            Ok(events)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_compose_internal_url() {
        // Clear every variable `from_env` reads so a runner with any of
        // these set in its environment can't make the test flaky.
        for var in [
            "ANIMA_OLLAMA_URL",
            "ANIMA_OLLAMA_MODEL",
            "ANIMA_OLLAMA_CTX",
            "ANIMA_OLLAMA_TIMEOUT",
        ] {
            std::env::remove_var(var);
        }
        let backend = OllamaBackend::from_env();
        assert_eq!(backend.endpoint(), "http://ollama:11434/api/generate");
        assert_eq!(backend.model_id(), "llama3.2:3b");
        assert_eq!(backend.max_context_tokens(), 8_192);
    }

    #[test]
    fn explicit_constructor_overrides_defaults() {
        let backend = OllamaBackend::new("http://localhost:11434", "qwen2.5:0.5b");
        assert_eq!(backend.id(), "ollama");
        assert_eq!(backend.model_id(), "qwen2.5:0.5b");
        assert_eq!(backend.endpoint(), "http://localhost:11434/api/generate");
    }

    #[test]
    fn endpoint_strips_trailing_slash() {
        let backend = OllamaBackend::new("http://ollama:11434/", "llama3.2:3b");
        assert_eq!(backend.endpoint(), "http://ollama:11434/api/generate");
    }

    #[test]
    fn max_context_tokens_defaults_to_8k() {
        let backend = OllamaBackend::new("http://x", "y");
        assert_eq!(backend.max_context_tokens(), 8_192);
    }
}
