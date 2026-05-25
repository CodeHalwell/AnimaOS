//! Backend factory for runtime provider selection.
//!
//! [`BackendFactory`] maps a [`BackendKind`] to a concrete fixture-mode
//! [`LlmBackend`] implementation.  This is the primary entrypoint used by
//! `kernels/hosted` to wire up the right provider at startup.
//!
//! # Example
//!
//! ```rust
//! use llm_backends::factory::{BackendFactory, BackendKind};
//! use scheduler::backend::LlmBackend;
//!
//! let backend = BackendFactory::fixture(BackendKind::Anthropic);
//! assert_eq!(backend.id(), "anthropic");
//!
//! let backend = BackendFactory::fixture(BackendKind::OpenAi);
//! assert_eq!(backend.id(), "openai");
//! ```

use std::sync::Arc;

use scheduler::backend::LlmBackend;

use crate::anthropic::AnthropicBackend;
use crate::openai::OpenAiBackend;

/// Identifies a concrete LLM provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// Anthropic Claude family.
    Anthropic,
    /// OpenAI GPT family.
    OpenAi,
    /// Built-in deterministic mock (from [`scheduler`]).
    Mock,
}

impl BackendKind {
    /// Parses a case-insensitive provider name string.
    ///
    /// Recognised values: `"anthropic"`, `"openai"`, `"mock"`.
    /// Returns `None` for unrecognised strings.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "anthropic" => Some(Self::Anthropic),
            "openai" | "open_ai" => Some(Self::OpenAi),
            "mock" => Some(Self::Mock),
            _ => None,
        }
    }
}

/// Constructs [`LlmBackend`] instances for a given [`BackendKind`].
pub struct BackendFactory;

impl BackendFactory {
    /// Returns a fixture-mode backend for the given provider.
    ///
    /// All returned backends are wrapped in [`Arc`] so they can be shared
    /// across async tasks in the hosted kernel without cloning.
    pub fn fixture(kind: BackendKind) -> Arc<dyn LlmBackend> {
        match kind {
            BackendKind::Anthropic => Arc::new(AnthropicBackend::new()),
            BackendKind::OpenAi => Arc::new(OpenAiBackend::new()),
            BackendKind::Mock => Arc::new(scheduler::MockLlmBackend::new()),
        }
    }

    /// Convenience method: look up a provider by name and return a fixture
    /// backend.  Falls back to the `Mock` backend if the name is unrecognised.
    ///
    /// This is the preferred entrypoint for selecting a backend from an
    /// environment variable or configuration file:
    ///
    /// ```rust
    /// use llm_backends::factory::BackendFactory;
    ///
    /// // In practice, read from std::env::var("ANIMA_BACKEND") or similar.
    /// let backend = BackendFactory::from_env_or_mock("anthropic");
    /// assert_eq!(backend.id(), "anthropic");
    ///
    /// let fallback = BackendFactory::from_env_or_mock("unknown");
    /// assert_eq!(fallback.id(), "mock");
    /// ```
    pub fn from_env_or_mock(provider: &str) -> Arc<dyn LlmBackend> {
        let kind = BackendKind::parse(provider).unwrap_or(BackendKind::Mock);
        Self::fixture(kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_returns_anthropic_backend() {
        let b = BackendFactory::fixture(BackendKind::Anthropic);
        assert_eq!(b.id(), "anthropic");
    }

    #[test]
    fn factory_returns_openai_backend() {
        let b = BackendFactory::fixture(BackendKind::OpenAi);
        assert_eq!(b.id(), "openai");
    }

    #[test]
    fn factory_returns_mock_backend() {
        let b = BackendFactory::fixture(BackendKind::Mock);
        assert_eq!(b.id(), "mock");
    }

    #[test]
    fn from_env_or_mock_falls_back_to_mock_on_unknown_provider() {
        let b = BackendFactory::from_env_or_mock("totally-unknown");
        assert_eq!(b.id(), "mock");
    }

    #[test]
    fn backend_kind_parse_is_case_insensitive() {
        assert_eq!(
            BackendKind::parse("ANTHROPIC"),
            Some(BackendKind::Anthropic)
        );
        assert_eq!(BackendKind::parse("OpenAI"), Some(BackendKind::OpenAi));
        assert_eq!(BackendKind::parse("mock"), Some(BackendKind::Mock));
        assert_eq!(BackendKind::parse("unknown"), None);
    }

    #[test]
    fn backend_kind_parse_accepts_open_ai_with_underscore() {
        assert_eq!(BackendKind::parse("open_ai"), Some(BackendKind::OpenAi));
    }
}
