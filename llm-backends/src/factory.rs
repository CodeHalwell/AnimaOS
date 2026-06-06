//! Backend factory for runtime provider selection.
//!
//! [`BackendFactory`] maps a [`BackendKind`] to a concrete fixture-mode
//! [`LlmBackend`] implementation.  This is the primary entrypoint used by
//! `kernels/hosted` to wire up the right provider at startup.
//!
//! E8 extends the factory with five new OpenAI-compatible provider kinds
//! (`Vllm`, `LmStudio`, `NvidiaNim`, `HfTgi`, `LlamaCppServer`) and a
//! [`BackendFactory::from_config`] constructor for operator-supplied configs.
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
use crate::capabilities::ProviderConfig;
use crate::compat::OpenAiCompatibleBackend;
use crate::ollama::OllamaBackend;
use crate::openai::OpenAiBackend;

/// Identifies a concrete LLM provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendKind {
    /// Anthropic Claude family.
    Anthropic,
    /// OpenAI GPT family.
    OpenAi,
    /// Built-in deterministic mock (from [`scheduler`]).
    Mock,
    /// Local Ollama daemon — GGUF inference on the host GPU.
    Ollama,
    // ── E8 S8.1 — OpenAI-compatible umbrella providers ────────────────────────
    /// vLLM high-throughput inference server (OpenAI-compatible).
    Vllm,
    /// LM Studio desktop app (OpenAI-compatible).
    LmStudio,
    /// NVIDIA NIM microservice (OpenAI-compatible).
    NvidiaNim,
    /// Hugging Face Text Generation Inference (Messages API).
    HfTgi,
    /// llama.cpp HTTP server (`llama-server --api`).
    LlamaCppServer,
    /// A fully operator-supplied config (E8 S8.0).
    Custom(ProviderConfig),
}

impl BackendKind {
    /// Parses a case-insensitive provider name string.
    ///
    /// Recognised values (in addition to the original four):
    /// `"vllm"`, `"lmstudio"` / `"lm-studio"`, `"nvidia-nim"` / `"nim"`,
    /// `"hf-tgi"` / `"tgi"`, `"llamacpp"` / `"llamacpp-server"`.
    ///
    /// Returns `None` for unrecognised strings.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "anthropic" => Some(Self::Anthropic),
            "openai" | "open_ai" => Some(Self::OpenAi),
            "mock" => Some(Self::Mock),
            "ollama" => Some(Self::Ollama),
            "vllm" => Some(Self::Vllm),
            "lmstudio" | "lm-studio" | "lm_studio" => Some(Self::LmStudio),
            "nvidia-nim" | "nvidia_nim" | "nim" => Some(Self::NvidiaNim),
            "hf-tgi" | "hf_tgi" | "tgi" => Some(Self::HfTgi),
            "llamacpp" | "llamacpp-server" | "llama-cpp" | "llama_cpp" => {
                Some(Self::LlamaCppServer)
            }
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
            // Ollama is a live HTTP backend; it reads its target URL + model
            // from environment variables so the factory surface stays uniform.
            BackendKind::Ollama => Arc::new(OllamaBackend::from_env()),
            // E8 S8.1 — OpenAI-compatible preset backends (all fixture-mode by default).
            BackendKind::Vllm => Arc::new(OpenAiCompatibleBackend::vllm()),
            BackendKind::LmStudio => Arc::new(OpenAiCompatibleBackend::lmstudio()),
            BackendKind::NvidiaNim => Arc::new(OpenAiCompatibleBackend::nvidia_nim()),
            BackendKind::HfTgi => Arc::new(OpenAiCompatibleBackend::hf_tgi()),
            BackendKind::LlamaCppServer => Arc::new(OpenAiCompatibleBackend::llamacpp_server()),
            // E8 S8.0 — operator-supplied config.
            BackendKind::Custom(config) => Arc::new(OpenAiCompatibleBackend::from_config(config)),
        }
    }

    /// Constructs a backend from an operator-supplied [`ProviderConfig`] (E8 S8.0).
    ///
    /// Chooses fixture vs live mode based on `ANIMA_COMPAT_LIVE`.
    pub fn from_config(config: ProviderConfig) -> Arc<dyn LlmBackend> {
        Arc::new(OpenAiCompatibleBackend::from_config(config))
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
    use crate::capabilities::BackendCapabilities;

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
    fn factory_returns_ollama_backend() {
        // Construction does no network I/O; it only reads env defaults.
        let b = BackendFactory::fixture(BackendKind::Ollama);
        assert_eq!(b.id(), "ollama");
    }

    #[test]
    fn backend_kind_parse_recognises_ollama() {
        assert_eq!(BackendKind::parse("ollama"), Some(BackendKind::Ollama));
        assert_eq!(BackendKind::parse("OLLAMA"), Some(BackendKind::Ollama));
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

    // ── E8 S8.1 tests ─────────────────────────────────────────────────────────

    #[test]
    fn factory_returns_vllm_backend() {
        let b = BackendFactory::fixture(BackendKind::Vllm);
        assert_eq!(b.id(), "vllm");
    }

    #[test]
    fn factory_returns_lmstudio_backend() {
        let b = BackendFactory::fixture(BackendKind::LmStudio);
        assert_eq!(b.id(), "lmstudio");
    }

    #[test]
    fn factory_returns_nvidia_nim_backend() {
        let b = BackendFactory::fixture(BackendKind::NvidiaNim);
        assert_eq!(b.id(), "nvidia-nim");
    }

    #[test]
    fn factory_returns_hf_tgi_backend() {
        let b = BackendFactory::fixture(BackendKind::HfTgi);
        assert_eq!(b.id(), "hf-tgi");
    }

    #[test]
    fn factory_returns_llamacpp_server_backend() {
        let b = BackendFactory::fixture(BackendKind::LlamaCppServer);
        assert_eq!(b.id(), "llamacpp-server");
    }

    #[test]
    fn backend_kind_parse_recognises_vllm() {
        assert_eq!(BackendKind::parse("vllm"), Some(BackendKind::Vllm));
        assert_eq!(BackendKind::parse("VLLM"), Some(BackendKind::Vllm));
    }

    #[test]
    fn backend_kind_parse_recognises_lmstudio_variants() {
        assert_eq!(BackendKind::parse("lmstudio"), Some(BackendKind::LmStudio));
        assert_eq!(BackendKind::parse("lm-studio"), Some(BackendKind::LmStudio));
        assert_eq!(BackendKind::parse("lm_studio"), Some(BackendKind::LmStudio));
    }

    #[test]
    fn backend_kind_parse_recognises_nvidia_nim_variants() {
        assert_eq!(
            BackendKind::parse("nvidia-nim"),
            Some(BackendKind::NvidiaNim)
        );
        assert_eq!(BackendKind::parse("nim"), Some(BackendKind::NvidiaNim));
    }

    #[test]
    fn backend_kind_parse_recognises_hf_tgi_variants() {
        assert_eq!(BackendKind::parse("hf-tgi"), Some(BackendKind::HfTgi));
        assert_eq!(BackendKind::parse("tgi"), Some(BackendKind::HfTgi));
    }

    #[test]
    fn backend_kind_parse_recognises_llamacpp_variants() {
        assert_eq!(
            BackendKind::parse("llamacpp"),
            Some(BackendKind::LlamaCppServer)
        );
        assert_eq!(
            BackendKind::parse("llamacpp-server"),
            Some(BackendKind::LlamaCppServer)
        );
        assert_eq!(
            BackendKind::parse("llama-cpp"),
            Some(BackendKind::LlamaCppServer)
        );
    }

    #[test]
    fn from_config_constructs_backend_with_correct_id() {
        let config = ProviderConfig::from_env_prefix(
            "my-local",
            "ANIMA_MYLOCAL",
            "http://localhost:9000/v1",
            "custom-model",
            16_384,
            BackendCapabilities::openai_compat(),
        );
        let b = BackendFactory::from_config(config);
        assert_eq!(b.id(), "my-local");
    }

    #[test]
    fn from_env_or_mock_recognises_vllm() {
        let b = BackendFactory::from_env_or_mock("vllm");
        assert_eq!(b.id(), "vllm");
    }
}
