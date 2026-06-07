#![forbid(unsafe_code)]

//! Provider-specific LLM backend implementations for AnimaOS.
//!
//! This crate lives *outside* the core workspace so that provider dependencies
//! (HTTP clients, TLS stacks) are never pulled into the `no_std`-compatible
//! crates.  The core [`scheduler`] crate defines only the provider-neutral
//! [`LlmBackend`] trait; this crate supplies concrete implementations.
//!
//! # CI safety
//!
//! Both [`anthropic::AnthropicBackend`] and [`openai::OpenAiBackend`] default
//! to **fixture mode**: they replay pre-recorded token streams from the
//! bundled `fixtures/` directory without making any network calls.  Live mode
//! (which requires a valid API key and outbound HTTPS) is opt-in and never
//! used during CI.
//!
//! # Backend selection
//!
//! Use [`factory::BackendFactory`] to select a backend by name at runtime:
//!
//! ```rust
//! use llm_backends::factory::{BackendFactory, BackendKind};
//!
//! let backend = BackendFactory::fixture(BackendKind::Anthropic);
//! assert_eq!(backend.id(), "anthropic");
//! ```

pub mod anthropic;
pub mod capabilities;
pub mod chat;
pub mod compat;
pub mod factory;
// E8 S8.2 — Hugging Face provider enhancement.
pub mod hf_transformers;
pub mod hub;
// E8 S8.3 — native in-process runtimes.
pub mod native;
pub mod ollama;
pub mod openai;

pub use anthropic::AnthropicBackend;
pub use capabilities::{BackendCapabilities, ProviderConfig};
pub use chat::{
    ChatBackend, ChatMessage, ChatResponse, ChatRole, FinishReason, ToolCall, ToolSpec,
};
pub use compat::OpenAiCompatibleBackend;
pub use factory::{BackendFactory, BackendKind, TierBackendChoices};
pub use hf_transformers::HfTransformersBackend;
pub use hub::{HfHubClient, HfModelInfo, HubError};
pub use native::{
    LiteRtLmBackend, LlamaCppNativeBackend, NativeRuntime, NativeRuntimeConfig, NativeRuntimeError,
};
pub use ollama::OllamaBackend;
pub use openai::OpenAiBackend;

// ── Shared fixture parsing ────────────────────────────────────────────────────

/// Internal fixture-loading utilities shared by all provider modules.
pub(crate) mod fixture {
    use serde::Deserialize;

    /// A single recorded exchange in a fixture file.
    #[derive(Debug, Deserialize)]
    pub struct FixtureEntry {
        /// The exact prompt text the fixture was recorded for.
        pub prompt: String,
        /// Pre-recorded token sequence that should be emitted for this prompt.
        pub tokens: Vec<String>,
    }

    /// Top-level structure of a provider fixture JSON file.
    #[derive(Debug, Deserialize)]
    struct FixtureFile {
        fixtures: Vec<FixtureEntry>,
    }

    /// Deserialises a JSON fixture file into a list of entries.
    ///
    /// # Errors
    ///
    /// Returns a `serde_json::Error` if the JSON is malformed.
    pub fn load_fixtures(json: &str) -> Result<Vec<FixtureEntry>, serde_json::Error> {
        let file: FixtureFile = serde_json::from_str(json)?;
        Ok(file.fixtures)
    }
}
