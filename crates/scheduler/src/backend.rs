//! Provider-agnostic LLM backend abstraction.

use std::future::Future;
use std::pin::Pin;

/// Streaming completion item: a single emitted token or signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamingCompletion {
    /// Next chunk of text.
    Token(String),
    /// Stream completed normally.
    Done,
    /// Stream was cancelled by the runtime.
    Cancelled,
}

/// Errors raised by [`LlmBackend`] implementations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmBackendError {
    /// Provider returned an explicit error.
    Provider(String),
    /// Cancellation token tripped before completion.
    Cancelled,
}

/// A boxed future returning a vector of streamed completion events.
pub type CompletionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<StreamingCompletion>, LlmBackendError>> + Send + 'a>>;

/// Trait implemented by every concrete LLM provider integration.
pub trait LlmBackend: Send + Sync + std::fmt::Debug {
    /// Returns the stable backend identifier (e.g., `"openai"`, `"anthropic"`).
    fn id(&self) -> &'static str;

    /// Runs a streaming completion with cooperative cancellation.
    fn stream_completion<'a>(
        &'a self,
        prompt: &'a str,
        cancel: &'a CancellationToken,
    ) -> CompletionFuture<'a>;
}

/// Cooperative cancellation flag for streaming requests.
#[derive(Debug, Default, Clone)]
pub struct CancellationToken {
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl CancellationToken {
    /// Creates a new, un-tripped token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Trips the token.
    pub fn cancel(&self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Returns true if the token has been tripped.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::SeqCst)
    }
}
