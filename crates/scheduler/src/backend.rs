//! Provider-agnostic LLM backend abstraction.

// `core::future`, `core::pin`, and `core::task` are available in both std and
// no_std environments; they are the canonical home of Future/Pin/Waker since
// Rust 1.36.  We use them directly so this module compiles without std.
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, Ordering};

// In no_std+alloc mode, Vec/String/Box live in the `alloc` crate.
// In std mode they come from the prelude — no explicit import needed.
#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::String, vec::Vec};

// Arc lives in alloc::sync in no_std+alloc; std::sync in hosted builds.
#[cfg(not(feature = "std"))]
use alloc::sync::Arc;
#[cfg(feature = "std")]
use std::sync::Arc;

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
pub trait LlmBackend: Send + Sync {
    /// Returns the stable backend identifier (e.g., `"openai"`, `"anthropic"`).
    fn id(&self) -> &'static str;

    /// Runs a streaming completion with cooperative cancellation.
    fn stream_completion<'a>(
        &'a self,
        prompt: &'a str,
        cancel: &'a CancellationToken,
    ) -> CompletionFuture<'a>;

    // ── E1.3 extensions ───────────────────────────────────────────────────────

    /// Returns the fully-qualified model identifier string carried in audit logs
    /// and token-accounting records (e.g. `"claude-3-haiku-20240307"`).
    ///
    /// The default delegates to [`LlmBackend::id`] so existing impls compile
    /// unchanged; concrete providers should override this with their model string.
    fn model_id(&self) -> &str {
        self.id()
    }

    /// Returns the maximum context window (in tokens) supported by this backend.
    ///
    /// Defaults to [`u32::MAX`], which is safe for mock / fixture backends that
    /// do not enforce a hard limit.  Provider implementations should override
    /// this with the value advertised by the provider API.
    fn max_context_tokens(&self) -> u32 {
        u32::MAX
    }

    /// Returns an estimate of the token count for `text`.
    ///
    /// The default heuristic rounds `len(text)` up to the nearest multiple of
    /// four, yielding ≈4 bytes per token — accurate enough for scheduling
    /// decisions.  Provider implementations should substitute the provider's
    /// actual tokeniser.
    fn estimate_token_count(&self, text: &str) -> u32 {
        ((text.len() as u32).saturating_add(3)) / 4
    }
}

/// Cooperative cancellation flag for streaming requests.
///
/// Backed by an `Arc<AtomicBool>` so the token is `Clone`able and can be
/// shared between the producer and any number of consumers without requiring
/// `std::sync::Mutex`.  The `Arc` comes from `alloc` in no_std builds.
#[derive(Debug, Default, Clone)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    /// Creates a new, un-tripped token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Trips the token.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Returns true if the token has been tripped.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}
