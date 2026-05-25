//! Deterministic mock [`LlmBackend`] used for hosted-target development and tests.

use crate::backend::{
    CancellationToken, CompletionFuture, LlmBackend, LlmBackendError, StreamingCompletion,
};

/// Mock backend that emits one [`StreamingCompletion::Token`] per whitespace-separated
/// word of the prompt, followed by [`StreamingCompletion::Done`]. Deterministic outputs
/// make it suitable for unit, integration, and CI use without external dependencies.
#[derive(Debug, Default, Clone)]
pub struct MockLlmBackend {
    id: &'static str,
}

impl MockLlmBackend {
    /// Creates a new mock backend identified as `"mock"`.
    pub fn new() -> Self {
        Self { id: "mock" }
    }

    /// Creates a mock backend with a custom identifier (useful when running multiple
    /// instances in a single test).
    pub fn with_id(id: &'static str) -> Self {
        Self { id }
    }
}

impl LlmBackend for MockLlmBackend {
    fn id(&self) -> &'static str {
        self.id
    }

    fn stream_completion<'a>(
        &'a self,
        prompt: &'a str,
        cancel: &'a CancellationToken,
    ) -> CompletionFuture<'a> {
        Box::pin(async move {
            let mut emitted: Vec<StreamingCompletion> = Vec::new();
            for word in prompt.split_whitespace() {
                if cancel.is_cancelled() {
                    emitted.push(StreamingCompletion::Cancelled);
                    return Err(LlmBackendError::Cancelled);
                }
                emitted.push(StreamingCompletion::Token(format!("{word} ")));
            }
            emitted.push(StreamingCompletion::Done);
            Ok(emitted)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll, Waker};

    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut future = Box::pin(future);
        loop {
            match Pin::as_mut(&mut future).poll(&mut cx) {
                Poll::Ready(value) => return value,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    #[test]
    fn emits_one_token_per_word_then_done() {
        let backend = MockLlmBackend::new();
        let cancel = CancellationToken::new();
        let out = block_on(backend.stream_completion("hello world", &cancel)).unwrap();
        assert_eq!(
            out,
            vec![
                StreamingCompletion::Token("hello ".to_string()),
                StreamingCompletion::Token("world ".to_string()),
                StreamingCompletion::Done,
            ]
        );
    }

    #[test]
    fn pre_cancelled_stream_returns_cancelled_error() {
        let backend = MockLlmBackend::new();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = block_on(backend.stream_completion("anything", &cancel)).unwrap_err();
        assert_eq!(err, LlmBackendError::Cancelled);
    }
}
