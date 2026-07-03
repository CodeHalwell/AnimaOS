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
use crate::hf_transformers::HfTransformersBackend;
use crate::native::{LiteRtLmBackend, LlamaCppNativeBackend};
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
    // ── E8 S8.3 — Native in-process runtimes ─────────────────────────────────
    /// llama.cpp in-process FFI runtime (GGUF models, fixture mode by default).
    LlamaCppNative,
    /// LiteRT-LM on-device runtime (MediaPipe Task bundles, fixture mode by default).
    LiteRtLm,
    /// A fully operator-supplied config (E8 S8.0).
    Custom(ProviderConfig),
    // ── E8 S8.2 — Hugging Face transformers sidecar ───────────────────────────
    /// Hugging Face `transformers` external Python subprocess backend (E8 S8.2.2).
    HfTransformers,
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
            "hf-transformers" | "hf_transformers" | "transformers" => Some(Self::HfTransformers),
            // E8 S8.3 — native in-process runtimes.
            "llamacpp-native" | "llama-cpp-native" | "llama_cpp_native" => {
                Some(Self::LlamaCppNative)
            }
            "litert-lm" | "litert_lm" | "liteRT" | "litert" => Some(Self::LiteRtLm),
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
            // E8 S8.3 — native in-process runtimes (fixture-mode by default).
            BackendKind::LlamaCppNative => Arc::new(LlamaCppNativeBackend::new()),
            BackendKind::LiteRtLm => Arc::new(LiteRtLmBackend::new()),
            // E8 S8.0 — operator-supplied config.
            BackendKind::Custom(config) => Arc::new(OpenAiCompatibleBackend::from_config(config)),
            // E8 S8.2.2 — HF transformers sidecar (fixture-mode by default).
            BackendKind::HfTransformers => Arc::new(HfTransformersBackend::from_env()),
        }
    }

    /// Builds a backend for `kind`, selecting a **live** client for the frontier
    /// providers when their API key is present, and a fixture otherwise (IO-1).
    ///
    /// This is the difference the review flagged: `fixture()` always returns a
    /// canned-reply stub, so routing a configured `ANTHROPIC_API_KEY` through it
    /// produced silent sentinel output. `resolve` uses the real client when the
    /// key is set, and warns loudly when a frontier provider is selected without
    /// a key rather than silently degrading to fixtures.
    pub fn resolve(kind: BackendKind) -> Arc<dyn LlmBackend> {
        match kind {
            BackendKind::Anthropic => match std::env::var("ANTHROPIC_API_KEY") {
                Ok(key) if !key.is_empty() => {
                    let model = std::env::var("ANIMA_ANTHROPIC_MODEL")
                        .unwrap_or_else(|_| "claude-3-5-sonnet-latest".to_string());
                    Arc::new(AnthropicBackend::live(model, key))
                }
                _ => {
                    eprintln!(
                        "llm-backends: Anthropic selected but ANTHROPIC_API_KEY is unset — \
                         using fixture mode (no real completions)"
                    );
                    Arc::new(AnthropicBackend::new())
                }
            },
            BackendKind::OpenAi => match std::env::var("OPENAI_API_KEY") {
                Ok(key) if !key.is_empty() => Arc::new(OpenAiCompatibleBackend::openai(key)),
                _ => {
                    eprintln!(
                        "llm-backends: OpenAI selected but OPENAI_API_KEY is unset — \
                         using fixture mode (no real completions)"
                    );
                    Arc::new(OpenAiBackend::new())
                }
            },
            other => BackendFactory::fixture(other),
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

// ── E9 S9.5 / E8 §4 — Per-tier backend binding ─────────────────────────────────

/// The three router tiers an operator can bind to distinct providers (E9 S9.5).
///
/// Mirrors `vita::ModelSelector` without taking a dependency on `vita`: the
/// hosted kernel maps these three [`BackendKind`]s onto the
/// `vita::router::TierBackends` map (cheap-local / mid-tier / frontier).
///
/// The binding is **configurable** — built from environment variables with
/// sensible, CI-hermetic defaults (see [`TierBackendChoices::from_env`]) — and
/// is never hard-coded, matching the design in
/// `docs/13-local-llm-providers.md §4`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierBackendChoices {
    /// Provider for the cheap-local (fast, on-device) tier.
    pub cheap_local: BackendKind,
    /// Provider for the mid-tier (balanced) tier.
    pub mid_tier: BackendKind,
    /// Provider for the frontier (full-capability) tier.
    pub frontier: BackendKind,
}

impl TierBackendChoices {
    /// Build an explicit set of tier choices.
    pub fn new(cheap_local: BackendKind, mid_tier: BackendKind, frontier: BackendKind) -> Self {
        Self {
            cheap_local,
            mid_tier,
            frontier,
        }
    }

    /// All three tiers bound to a single provider (backward-compatible default).
    pub fn uniform(kind: BackendKind) -> Self {
        Self {
            cheap_local: kind.clone(),
            mid_tier: kind.clone(),
            frontier: kind,
        }
    }

    /// Resolve tier bindings from the environment with sensible defaults.
    ///
    /// Per-tier overrides (parsed via [`BackendKind::parse`]; unrecognised
    /// values fall back to that tier's default):
    ///
    /// | Tier        | Env var                  | Default                                   |
    /// |-------------|--------------------------|-------------------------------------------|
    /// | cheap-local | `ANIMA_CHEAP_BACKEND`    | `ollama` if `ANIMA_OLLAMA*` hints, else mock |
    /// | mid-tier    | `ANIMA_MID_BACKEND`      | the cheap-local choice (open decision left to operator) |
    /// | frontier    | `ANIMA_FRONTIER_BACKEND` | `anthropic` if `ANTHROPIC_API_KEY` set, else `openai` if `OPENAI_API_KEY` set, else mock |
    ///
    /// As a convenience, a global `ANIMA_BACKEND` (the legacy single-backend
    /// selector) seeds every tier when set and no per-tier override is given, so
    /// existing single-backend deployments keep working unchanged.
    ///
    /// The mid-tier default deliberately tracks the cheap-local choice rather
    /// than hard-coding a provider — the open mid-tier decision is left to the
    /// operator (`docs/13 §4`).
    pub fn from_env() -> Self {
        let global = std::env::var("ANIMA_BACKEND")
            .ok()
            .and_then(|s| BackendKind::parse(&s));

        let cheap_default = global.clone().unwrap_or_else(default_cheap_local_kind);
        let cheap_local = tier_kind_from_env("ANIMA_CHEAP_BACKEND", cheap_default);

        // Mid-tier tracks the cheap-local choice by default (open decision).
        let mid_default = global.clone().unwrap_or_else(|| cheap_local.clone());
        let mid_tier = tier_kind_from_env("ANIMA_MID_BACKEND", mid_default);

        let frontier_default = global.unwrap_or_else(default_frontier_kind);
        let frontier = tier_kind_from_env("ANIMA_FRONTIER_BACKEND", frontier_default);

        Self {
            cheap_local,
            mid_tier,
            frontier,
        }
    }

    /// Materialise each tier choice into a fixture-mode [`LlmBackend`].
    ///
    /// Returns `(cheap_local, mid_tier, frontier)` ready to assemble into a
    /// `vita::router::TierBackends`.
    pub fn into_fixture_backends(
        self,
    ) -> (
        Arc<dyn LlmBackend>,
        Arc<dyn LlmBackend>,
        Arc<dyn LlmBackend>,
    ) {
        (
            BackendFactory::fixture(self.cheap_local),
            BackendFactory::fixture(self.mid_tier),
            BackendFactory::fixture(self.frontier),
        )
    }

    /// Materialise each tier choice, using a **live** client for a frontier
    /// provider whose API key is present (IO-1) and fixtures otherwise.
    ///
    /// Returns `(cheap_local, mid_tier, frontier)`.
    pub fn into_backends(
        self,
    ) -> (
        Arc<dyn LlmBackend>,
        Arc<dyn LlmBackend>,
        Arc<dyn LlmBackend>,
    ) {
        (
            BackendFactory::resolve(self.cheap_local),
            BackendFactory::resolve(self.mid_tier),
            BackendFactory::resolve(self.frontier),
        )
    }
}

/// Parse a tier override env var, falling back to `default` when unset or when
/// the value is not a recognised provider name.
fn tier_kind_from_env(var: &str, default: BackendKind) -> BackendKind {
    std::env::var(var)
        .ok()
        .and_then(|s| BackendKind::parse(&s))
        .unwrap_or(default)
}

/// Default cheap-local provider: `ollama` when an Ollama endpoint/model hint is
/// present in the environment, otherwise the CI-safe `mock`.
fn default_cheap_local_kind() -> BackendKind {
    let ollama_hint = std::env::var("ANIMA_OLLAMA_URL").is_ok()
        || std::env::var("ANIMA_OLLAMA_MODEL").is_ok()
        || std::env::var("OLLAMA_HOST").is_ok();
    if ollama_hint {
        BackendKind::Ollama
    } else {
        BackendKind::Mock
    }
}

/// Default frontier provider: `anthropic` when `ANTHROPIC_API_KEY` is set, else
/// `openai` when `OPENAI_API_KEY` is set (both resolve to a live client via
/// [`BackendFactory::resolve`]), otherwise the CI-safe `mock`.
fn default_frontier_kind() -> BackendKind {
    let has_key = |var: &str| std::env::var(var).map(|k| !k.is_empty()).unwrap_or(false);
    frontier_kind_for(has_key("ANTHROPIC_API_KEY"), has_key("OPENAI_API_KEY"))
}

/// Env-free frontier-provider precedence (Anthropic ≻ OpenAI ≻ mock), split out
/// so the selection is unit-testable without mutating process environment.
fn frontier_kind_for(has_anthropic_key: bool, has_openai_key: bool) -> BackendKind {
    if has_anthropic_key {
        BackendKind::Anthropic
    } else if has_openai_key {
        BackendKind::OpenAi
    } else {
        BackendKind::Mock
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::BackendCapabilities;

    #[test]
    fn frontier_default_prefers_anthropic_then_openai_then_mock() {
        assert_eq!(frontier_kind_for(true, true), BackendKind::Anthropic);
        assert_eq!(frontier_kind_for(true, false), BackendKind::Anthropic);
        // OpenAI-only deployments now auto-select the live OpenAI frontier
        // instead of silently staying on the mock backend.
        assert_eq!(frontier_kind_for(false, true), BackendKind::OpenAi);
        assert_eq!(frontier_kind_for(false, false), BackendKind::Mock);
    }

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

    // ── E9 S9.5 — TierBackendChoices ──────────────────────────────────────────

    #[test]
    fn tier_choices_uniform_binds_all_three_tiers_to_one_provider() {
        let choices = TierBackendChoices::uniform(BackendKind::Mock);
        assert_eq!(choices.cheap_local, BackendKind::Mock);
        assert_eq!(choices.mid_tier, BackendKind::Mock);
        assert_eq!(choices.frontier, BackendKind::Mock);
    }

    #[test]
    fn tier_choices_new_keeps_distinct_tiers() {
        let choices = TierBackendChoices::new(
            BackendKind::Ollama,
            BackendKind::LmStudio,
            BackendKind::Anthropic,
        );
        assert_eq!(choices.cheap_local, BackendKind::Ollama);
        assert_eq!(choices.mid_tier, BackendKind::LmStudio);
        assert_eq!(choices.frontier, BackendKind::Anthropic);
    }

    #[test]
    fn tier_choices_into_fixture_backends_materialises_each_tier() {
        let choices = TierBackendChoices::new(
            BackendKind::Mock,
            BackendKind::OpenAi,
            BackendKind::Anthropic,
        );
        let (cheap, mid, frontier) = choices.into_fixture_backends();
        assert_eq!(cheap.id(), "mock");
        assert_eq!(mid.id(), "openai");
        assert_eq!(frontier.id(), "anthropic");
    }

    #[test]
    fn tier_kind_from_env_falls_back_to_default_when_unset() {
        // Use a var name that is exceedingly unlikely to be set in any
        // environment so the fallback path is exercised deterministically.
        let kind = tier_kind_from_env(
            "ANIMA_TIER_KIND_FROM_ENV_DEFINITELY_UNSET_XYZ",
            BackendKind::LmStudio,
        );
        assert_eq!(kind, BackendKind::LmStudio);
    }

    // ── E8 S8.3 — native in-process runtime tests ─────────────────────────────

    #[test]
    fn factory_returns_llamacpp_native_backend() {
        let b = BackendFactory::fixture(BackendKind::LlamaCppNative);
        assert_eq!(b.id(), "llama-cpp-native");
    }

    #[test]
    fn factory_returns_litert_lm_backend() {
        let b = BackendFactory::fixture(BackendKind::LiteRtLm);
        assert_eq!(b.id(), "litert-lm");
    }

    #[test]
    fn backend_kind_parse_recognises_llamacpp_native_variants() {
        assert_eq!(
            BackendKind::parse("llamacpp-native"),
            Some(BackendKind::LlamaCppNative)
        );
        assert_eq!(
            BackendKind::parse("llama-cpp-native"),
            Some(BackendKind::LlamaCppNative)
        );
        assert_eq!(
            BackendKind::parse("llama_cpp_native"),
            Some(BackendKind::LlamaCppNative)
        );
        assert_eq!(
            BackendKind::parse("LLAMACPP-NATIVE"),
            Some(BackendKind::LlamaCppNative)
        );
    }

    #[test]
    fn backend_kind_parse_recognises_litert_lm_variants() {
        assert_eq!(BackendKind::parse("litert-lm"), Some(BackendKind::LiteRtLm));
        assert_eq!(BackendKind::parse("litert_lm"), Some(BackendKind::LiteRtLm));
        assert_eq!(BackendKind::parse("litert"), Some(BackendKind::LiteRtLm));
        assert_eq!(BackendKind::parse("LITERT-LM"), Some(BackendKind::LiteRtLm));
    }

    #[test]
    fn from_env_or_mock_recognises_llamacpp_native() {
        let b = BackendFactory::from_env_or_mock("llamacpp-native");
        assert_eq!(b.id(), "llama-cpp-native");
    }

    #[test]
    fn from_env_or_mock_recognises_litert_lm() {
        let b = BackendFactory::from_env_or_mock("litert-lm");
        assert_eq!(b.id(), "litert-lm");
    }

    #[test]
    fn from_env_is_ci_safe_and_returns_constructible_backends() {
        // Whatever the ambient environment, from_env() must yield three parseable
        // tier choices that materialise into real backends without panicking.
        let choices = TierBackendChoices::from_env();
        let (cheap, mid, frontier) = choices.into_fixture_backends();
        assert!(!cheap.id().is_empty());
        assert!(!mid.id().is_empty());
        assert!(!frontier.id().is_empty());
    }
}
