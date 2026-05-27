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
    request_timeout: Duration,
}

impl OllamaBackend {
    /// Construct a backend pointed at an explicit Ollama URL + model tag.
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            max_ctx: 8_192,
            request_timeout: Duration::from_secs(300),
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
        }
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

            let agent = ureq::AgentBuilder::new()
                .timeout(self.request_timeout)
                .build();

            let body = serde_json::json!({
                "model": self.model,
                "prompt": prompt,
                "stream": true,
            });

            let response = agent
                .post(&self.endpoint())
                .set("Content-Type", "application/json")
                .send_string(&body.to_string())
                .map_err(|e| LlmBackendError::Provider(format!("ollama request failed: {e}")))?;

            let reader = BufReader::new(response.into_reader());
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
        // Ensure no env contamination from the test harness; only the variables
        // we care about need to be cleared.
        std::env::remove_var("ANIMA_OLLAMA_URL");
        std::env::remove_var("ANIMA_OLLAMA_MODEL");
        let backend = OllamaBackend::from_env();
        assert_eq!(backend.endpoint(), "http://ollama:11434/api/generate");
        assert_eq!(backend.model_id(), "llama3.2:3b");
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
