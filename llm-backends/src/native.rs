//! Native in-process runtime abstraction (E8 S8.3).
//!
//! Provides an abstraction layer for running LLM inference in-process without
//! spawning a separate HTTP server. Two runtimes are defined:
//!
//! - **llama.cpp** ([`LlamaCppNativeBackend`]) — GGUF quantised-model inference
//!   via the llama.cpp C library, loaded directly into the process address space.
//!   No HTTP hop; shared-memory KV cache; suitable for the cheap-local tier on
//!   hosts that can load a quantised model (Phi-3.5, Llama-3.2, Gemma-2, …).
//! - **LiteRT-LM** ([`LiteRtLmBackend`]) — Google's on-device LLM runtime
//!   (MediaPipe LLM Inference API, formerly TensorFlow Lite LLM). Targets
//!   mobile and embedded hosts; integrates with XNNPACK and GPU delegates.
//!
//! # Modes
//!
//! Both backends default to **fixture mode** (CI-safe, no native library required).
//! Live mode is gated behind an environment variable:
//!
//! | Backend | Env var | Feature flag (future) |
//! |---------|---------|----------------------|
//! | llama.cpp | `ANIMA_LLAMACPP_NATIVE_LIVE=1` | `llama-native-live` |
//! | LiteRT-LM | `ANIMA_LITERT_LM_LIVE=1` | `litert-lm-live` |
//!
//! When the live env var is set but the feature flag is not compiled in, the
//! backend logs a warning and transparently falls back to fixture mode. This lets
//! operators test configuration without requiring a compile step.
//!
//! # Extension point
//!
//! [`NativeRuntime`] is the trait that live FFI implementations will satisfy. The
//! fixture shim implements the same trait. Callers of [`LlamaCppNativeBackend`] and
//! [`LiteRtLmBackend`] do not need to know which concrete runtime is active.

use std::collections::HashMap;
use std::sync::Arc;

use scheduler::backend::{
    CancellationToken, CompletionFuture, LlmBackend, LlmBackendError, StreamingCompletion,
};

// ── Configuration ─────────────────────────────────────────────────────────────

/// Configuration for a native in-process model runtime (E8 S8.3).
///
/// Covers the parameters common to both llama.cpp and LiteRT-LM backends.
/// Fields map directly to the libraries' primary knobs; library-specific
/// parameters (e.g. NUMA settings, GPU layers) are left to the live feature
/// implementations and are not surfaced here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRuntimeConfig {
    /// Path to the model file on disk.
    ///
    /// - llama.cpp: path to a GGUF quantised-model file (`*.gguf`)
    /// - LiteRT-LM: path to a MediaPipe Task bundle or `.tflite` flatbuffer
    pub model_path: String,
    /// Context window size in tokens (`n_ctx` in llama.cpp terminology).
    pub n_ctx: u32,
    /// Number of CPU threads for inference.
    pub n_threads: u32,
    /// Prompt-evaluation batch size (`n_batch`).
    pub n_batch: u32,
    /// Maximum new tokens to generate per request.
    pub max_new_tokens: u32,
}

impl NativeRuntimeConfig {
    /// Builds a config from environment variables with sensible defaults.
    ///
    /// | Field            | Env var                       | Default |
    /// |------------------|-------------------------------|---------|
    /// | `model_path`     | `ANIMA_NATIVE_MODEL_PATH`     | `""` (fixture) |
    /// | `n_ctx`          | `ANIMA_NATIVE_N_CTX`          | `4096` |
    /// | `n_threads`      | `ANIMA_NATIVE_N_THREADS`      | `4` |
    /// | `n_batch`        | `ANIMA_NATIVE_N_BATCH`        | `512` |
    /// | `max_new_tokens` | `ANIMA_NATIVE_MAX_NEW_TOKENS` | `512` |
    pub fn from_env() -> Self {
        Self {
            model_path: std::env::var("ANIMA_NATIVE_MODEL_PATH").unwrap_or_default(),
            n_ctx: parse_env_u32("ANIMA_NATIVE_N_CTX", 4096),
            n_threads: parse_env_u32("ANIMA_NATIVE_N_THREADS", 4),
            n_batch: parse_env_u32("ANIMA_NATIVE_N_BATCH", 512),
            max_new_tokens: parse_env_u32("ANIMA_NATIVE_MAX_NEW_TOKENS", 512),
        }
    }
}

impl Default for NativeRuntimeConfig {
    fn default() -> Self {
        Self {
            model_path: String::new(),
            n_ctx: 4096,
            n_threads: 4,
            n_batch: 512,
            max_new_tokens: 512,
        }
    }
}

fn parse_env_u32(var: &str, default: u32) -> u32 {
    std::env::var(var)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

// ── NativeRuntime trait ───────────────────────────────────────────────────────

/// Abstraction over an in-process LLM runtime (E8 S8.3 hook point).
///
/// Implementors load a model once at construction time and expose a synchronous
/// `generate` method. The wrapping `LlmBackend` implementations call `generate`
/// from within an async block; the synchronous call is acceptable because both
/// llama.cpp and LiteRT-LM expose blocking APIs that internally manage their own
/// thread pools and are designed to be called from a single caller thread.
///
/// When the `llama-native-live` or `litert-lm-live` feature flags land, the
/// corresponding FFI structs will implement this trait and be slotted into
/// [`LlamaCppNativeBackend::from_env`] / [`LiteRtLmBackend::from_env`] without
/// any changes to callers.
pub trait NativeRuntime: Send + Sync {
    /// A short identifier for the runtime (e.g. `"llama-cpp-native"`).
    fn runtime_id(&self) -> &'static str;

    /// The model identifier string used in audit logs.
    fn model_id(&self) -> &str;

    /// Maximum context window in tokens.
    fn n_ctx(&self) -> u32;

    /// Generate tokens for `prompt`, stopping at `max_tokens` or EOS.
    ///
    /// Implementations must check `cancel.is_cancelled()` between each token
    /// and return [`NativeRuntimeError::Cancelled`] when the flag is set.
    fn generate(
        &self,
        prompt: &str,
        max_tokens: u32,
        cancel: &CancellationToken,
    ) -> Result<Vec<String>, NativeRuntimeError>;
}

/// Errors from a native runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeRuntimeError {
    /// The native library is not available (feature not compiled in).
    BackendUnavailable,
    /// Model file could not be loaded.
    ModelLoadFailed(String),
    /// Inference failed.
    InferenceFailed(String),
    /// The caller cancelled the request.
    Cancelled,
}

impl std::fmt::Display for NativeRuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BackendUnavailable => write!(
                f,
                "native runtime is not available; recompile with the \
                 appropriate feature flag (llama-native-live / litert-lm-live)"
            ),
            Self::ModelLoadFailed(e) => write!(f, "model load failed: {e}"),
            Self::InferenceFailed(e) => write!(f, "inference failed: {e}"),
            Self::Cancelled => write!(f, "inference cancelled by caller"),
        }
    }
}

// ── Fixture shim ─────────────────────────────────────────────────────────────

/// Deterministic `NativeRuntime` shim for CI-safe testing.
///
/// Returns pre-recorded token lists keyed by exact prompt text. Unknown prompts
/// receive a single sentinel token so tests never silently produce empty output.
struct FixtureNativeRuntime {
    id: &'static str,
    model: String,
    n_ctx: u32,
    fixtures: HashMap<String, Vec<String>>,
    sentinel: &'static str,
}

impl FixtureNativeRuntime {
    fn new(
        id: &'static str,
        model: impl Into<String>,
        n_ctx: u32,
        fixtures: impl IntoIterator<Item = (String, Vec<String>)>,
        sentinel: &'static str,
    ) -> Self {
        Self {
            id,
            model: model.into(),
            n_ctx,
            fixtures: fixtures.into_iter().collect(),
            sentinel,
        }
    }
}

impl NativeRuntime for FixtureNativeRuntime {
    fn runtime_id(&self) -> &'static str {
        self.id
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    fn n_ctx(&self) -> u32 {
        self.n_ctx
    }

    fn generate(
        &self,
        prompt: &str,
        _max_tokens: u32,
        cancel: &CancellationToken,
    ) -> Result<Vec<String>, NativeRuntimeError> {
        let tokens = self
            .fixtures
            .get(prompt)
            .cloned()
            .unwrap_or_else(|| vec![self.sentinel.to_string()]);

        let mut result = Vec::with_capacity(tokens.len());
        for tok in tokens {
            if cancel.is_cancelled() {
                return Err(NativeRuntimeError::Cancelled);
            }
            result.push(tok);
        }
        Ok(result)
    }
}

// ── Built-in fixture sets ─────────────────────────────────────────────────────

const LLAMACPP_FIXTURE_SENTINEL: &str = "[llamacpp-native-fixture-not-found]";
const LITERT_FIXTURE_SENTINEL: &str = "[litert-lm-fixture-not-found]";

/// Pre-recorded token sequences for the llama.cpp fixture backend.
const LLAMACPP_FIXTURES: &[(&str, &[&str])] = &[
    (
        "Hello, world!",
        &["Hello", " there", ",", " how", " can", " I", " help", "?"],
    ),
    ("What is 2+2?", &["2", "+", "2", " equals", " 4", "."]),
    (
        "Summarise the following text:",
        &["Here", " is", " a", " summary", ":"],
    ),
    (
        "Write a haiku about Rust:",
        &[
            "Ownership",
            " rules",
            "\n",
            "Lifetimes",
            " guard",
            " each",
            " borrowed",
            " ref",
            "\n",
            "Safe",
            " systems",
            " sing",
            ".",
        ],
    ),
];

/// Pre-recorded token sequences for the LiteRT-LM fixture backend.
const LITERT_FIXTURES: &[(&str, &[&str])] = &[
    (
        "Hello, world!",
        &["Hello", "!", " I", "'m", " running", " on", "-device", "."],
    ),
    ("What is 2+2?", &["The", " answer", " is", " 4", "."]),
    ("Translate to French: cat", &["chat"]),
    (
        "What is your name?",
        &[
            "I",
            " am",
            " an",
            " on",
            "-device",
            " language",
            " model",
            " running",
            " via",
            " LiteRT",
            "-LM",
            ".",
        ],
    ),
];

// ── LlamaCppNativeBackend ─────────────────────────────────────────────────────

/// llama.cpp in-process inference backend (E8 S8.3).
///
/// Loads a GGUF-quantised model directly into the process address space via the
/// llama.cpp C API, avoiding the HTTP hop required by [`LlamaCppServer`]. Shares
/// KV-cache blocks with the memory subsystem through the standard block-structured
/// context-tracking interface.
///
/// [`LlamaCppServer`]: crate::compat::OpenAiCompatibleBackend
///
/// # Modes
///
/// | Mode | Condition | Behaviour |
/// |------|-----------|-----------|
/// | Fixture | default | Replays pre-recorded token lists. |
/// | Live | `ANIMA_LLAMACPP_NATIVE_LIVE=1` + feature `llama-native-live` | Real in-process inference via FFI. |
///
/// In the current codebase the `llama-native-live` feature is not yet wired to
/// an FFI crate; the env var is recognised but the backend transparently falls
/// back to fixture mode with a diagnostic message on stderr.
///
/// # Supported models (live mode, when enabled)
///
/// Any GGUF-format model supported by llama.cpp ≥ b3800:
/// Llama 3.x, Phi-3.5, Gemma 2, Qwen 2.5, Mistral, and many others.
pub struct LlamaCppNativeBackend {
    runtime: Arc<dyn NativeRuntime>,
    config: NativeRuntimeConfig,
}

impl LlamaCppNativeBackend {
    /// Fixture mode: deterministic pre-recorded tokens, CI-safe.
    pub fn new() -> Self {
        let fixtures = LLAMACPP_FIXTURES.iter().map(|(prompt, tokens)| {
            (
                prompt.to_string(),
                tokens.iter().map(|t| t.to_string()).collect(),
            )
        });

        let runtime = Arc::new(FixtureNativeRuntime::new(
            "llama-cpp-native",
            "phi-3.5-mini-instruct.Q4_K_M.gguf",
            4096,
            fixtures,
            LLAMACPP_FIXTURE_SENTINEL,
        ));

        Self {
            runtime,
            config: NativeRuntimeConfig::default(),
        }
    }

    /// Reads config from environment variables and selects fixture vs live mode.
    ///
    /// When `ANIMA_LLAMACPP_NATIVE_LIVE=1` is set but the `llama-native-live`
    /// cargo feature is not compiled in, logs a warning to stderr and falls back
    /// to fixture mode.
    pub fn from_env() -> Self {
        let config = NativeRuntimeConfig::from_env();
        let live_requested = std::env::var("ANIMA_LLAMACPP_NATIVE_LIVE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        if live_requested {
            // Live mode skeleton: log a diagnostic and fall through to fixture mode.
            // When `llama-native-live` lands, this branch will construct the real
            // `LlamaCppRuntime` with the config's model_path and thread settings.
            eprintln!(
                "[anima] LlamaCppNativeBackend: live mode requested \
                 (ANIMA_LLAMACPP_NATIVE_LIVE=1) but the `llama-native-live` \
                 cargo feature is not compiled in; falling back to fixture mode. \
                 Rebuild with `--features llama-native-live` to enable real inference."
            );
        }

        let fixtures = LLAMACPP_FIXTURES.iter().map(|(prompt, tokens)| {
            (
                prompt.to_string(),
                tokens.iter().map(|t| t.to_string()).collect(),
            )
        });

        let runtime = Arc::new(FixtureNativeRuntime::new(
            "llama-cpp-native",
            "phi-3.5-mini-instruct.Q4_K_M.gguf",
            config.n_ctx,
            fixtures,
            LLAMACPP_FIXTURE_SENTINEL,
        ));

        Self { runtime, config }
    }

    /// Fixture mode with a custom model name and token fixture set (for testing).
    pub fn with_custom_fixtures(
        model: impl Into<String>,
        fixtures: impl IntoIterator<Item = (String, Vec<String>)>,
    ) -> Self {
        let runtime = Arc::new(FixtureNativeRuntime::new(
            "llama-cpp-native",
            model,
            4096,
            fixtures,
            LLAMACPP_FIXTURE_SENTINEL,
        ));
        Self {
            runtime,
            config: NativeRuntimeConfig::default(),
        }
    }

    /// Exposes the active [`NativeRuntimeConfig`] (e.g. for audit logging).
    pub fn config(&self) -> &NativeRuntimeConfig {
        &self.config
    }
}

impl Default for LlamaCppNativeBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl LlmBackend for LlamaCppNativeBackend {
    fn id(&self) -> &'static str {
        "llama-cpp-native"
    }

    fn model_id(&self) -> &str {
        self.runtime.model_id()
    }

    fn max_context_tokens(&self) -> u32 {
        self.runtime.n_ctx()
    }

    fn stream_completion<'a>(
        &'a self,
        prompt: &'a str,
        cancel: &'a CancellationToken,
    ) -> CompletionFuture<'a> {
        Box::pin(async move {
            let tokens = self
                .runtime
                .generate(prompt, self.config.max_new_tokens, cancel)
                .map_err(|e| match e {
                    NativeRuntimeError::Cancelled => LlmBackendError::Cancelled,
                    other => LlmBackendError::Provider(other.to_string()),
                })?;

            let mut events: Vec<StreamingCompletion> = Vec::with_capacity(tokens.len() + 1);
            for tok in tokens {
                if cancel.is_cancelled() {
                    return Err(LlmBackendError::Cancelled);
                }
                events.push(StreamingCompletion::Token(tok));
            }
            events.push(StreamingCompletion::Done);
            Ok(events)
        })
    }
}

// ── LiteRtLmBackend ───────────────────────────────────────────────────────────

/// LiteRT-LM on-device inference backend (E8 S8.3).
///
/// Runs inference via the MediaPipe LLM Inference API (LiteRT-LM), targeting
/// mobile and embedded hosts. Integrates with XNNPACK (multi-threaded CPU) and
/// GPU delegates (NVIDIA, Apple Metal, Arm Mali). Suitable for the cheap-local
/// tier on devices where loading a GGUF model is not practical.
///
/// # Modes
///
/// | Mode | Condition | Behaviour |
/// |------|-----------|-----------|
/// | Fixture | default | Replays pre-recorded token lists. |
/// | Live | `ANIMA_LITERT_LM_LIVE=1` + feature `litert-lm-live` | Real on-device inference. |
///
/// The `litert-lm-live` cargo feature is not yet wired; live mode logs a
/// diagnostic to stderr and falls back to fixture mode transparently.
///
/// # Supported model formats (live mode, when enabled)
///
/// MediaPipe Task bundles (`.task`) and TFLite flatbuffers (`.tflite`) that
/// include an LLM Inference sub-graph. Google provides pre-converted bundles for
/// Gemma-2B, Phi-2, and Falcon-RW-1B; third-party converters handle Llama 3.
pub struct LiteRtLmBackend {
    runtime: Arc<dyn NativeRuntime>,
    config: NativeRuntimeConfig,
}

impl LiteRtLmBackend {
    /// Fixture mode: deterministic pre-recorded tokens, CI-safe.
    pub fn new() -> Self {
        let fixtures = LITERT_FIXTURES.iter().map(|(prompt, tokens)| {
            (
                prompt.to_string(),
                tokens.iter().map(|t| t.to_string()).collect(),
            )
        });

        let runtime = Arc::new(FixtureNativeRuntime::new(
            "litert-lm",
            "gemma-2-2b-it.task",
            8192,
            fixtures,
            LITERT_FIXTURE_SENTINEL,
        ));

        Self {
            runtime,
            config: NativeRuntimeConfig {
                model_path: String::new(),
                n_ctx: 8192,
                n_threads: 4,
                n_batch: 256,
                max_new_tokens: 512,
            },
        }
    }

    /// Reads config from environment variables and selects fixture vs live mode.
    ///
    /// When `ANIMA_LITERT_LM_LIVE=1` is set but the `litert-lm-live` feature is
    /// not compiled in, logs a warning to stderr and falls back to fixture mode.
    pub fn from_env() -> Self {
        let config = NativeRuntimeConfig::from_env();
        let live_requested = std::env::var("ANIMA_LITERT_LM_LIVE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        if live_requested {
            eprintln!(
                "[anima] LiteRtLmBackend: live mode requested \
                 (ANIMA_LITERT_LM_LIVE=1) but the `litert-lm-live` cargo \
                 feature is not compiled in; falling back to fixture mode. \
                 Rebuild with `--features litert-lm-live` to enable real inference."
            );
        }

        let fixtures = LITERT_FIXTURES.iter().map(|(prompt, tokens)| {
            (
                prompt.to_string(),
                tokens.iter().map(|t| t.to_string()).collect(),
            )
        });

        let runtime = Arc::new(FixtureNativeRuntime::new(
            "litert-lm",
            "gemma-2-2b-it.task",
            config.n_ctx,
            fixtures,
            LITERT_FIXTURE_SENTINEL,
        ));

        Self { runtime, config }
    }

    /// Fixture mode with a custom model name and token fixture set (for testing).
    pub fn with_custom_fixtures(
        model: impl Into<String>,
        fixtures: impl IntoIterator<Item = (String, Vec<String>)>,
    ) -> Self {
        let runtime = Arc::new(FixtureNativeRuntime::new(
            "litert-lm",
            model,
            8192,
            fixtures,
            LITERT_FIXTURE_SENTINEL,
        ));
        Self {
            runtime,
            config: NativeRuntimeConfig {
                model_path: String::new(),
                n_ctx: 8192,
                n_threads: 4,
                n_batch: 256,
                max_new_tokens: 512,
            },
        }
    }

    /// Exposes the active [`NativeRuntimeConfig`] (e.g. for audit logging).
    pub fn config(&self) -> &NativeRuntimeConfig {
        &self.config
    }
}

impl Default for LiteRtLmBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl LlmBackend for LiteRtLmBackend {
    fn id(&self) -> &'static str {
        "litert-lm"
    }

    fn model_id(&self) -> &str {
        self.runtime.model_id()
    }

    fn max_context_tokens(&self) -> u32 {
        self.runtime.n_ctx()
    }

    fn stream_completion<'a>(
        &'a self,
        prompt: &'a str,
        cancel: &'a CancellationToken,
    ) -> CompletionFuture<'a> {
        Box::pin(async move {
            let tokens = self
                .runtime
                .generate(prompt, self.config.max_new_tokens, cancel)
                .map_err(|e| match e {
                    NativeRuntimeError::Cancelled => LlmBackendError::Cancelled,
                    other => LlmBackendError::Provider(other.to_string()),
                })?;

            let mut events: Vec<StreamingCompletion> = Vec::with_capacity(tokens.len() + 1);
            for tok in tokens {
                if cancel.is_cancelled() {
                    return Err(LlmBackendError::Cancelled);
                }
                events.push(StreamingCompletion::Token(tok));
            }
            events.push(StreamingCompletion::Done);
            Ok(events)
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        use std::pin::Pin;
        use std::task::{Context, Poll, Waker};
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut f = Box::pin(f);
        loop {
            match Pin::as_mut(&mut f).poll(&mut cx) {
                Poll::Ready(v) => return v,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    // ── NativeRuntimeConfig ───────────────────────────────────────────────────

    #[test]
    fn native_runtime_config_default_has_sensible_values() {
        let cfg = NativeRuntimeConfig::default();
        assert!(cfg.model_path.is_empty());
        assert_eq!(cfg.n_ctx, 4096);
        assert_eq!(cfg.n_threads, 4);
        assert_eq!(cfg.n_batch, 512);
        assert_eq!(cfg.max_new_tokens, 512);
    }

    #[test]
    fn native_runtime_config_from_env_falls_back_to_defaults_when_unset() {
        // Ensure these test-specific env vars are not set in CI.
        // (We use uniquely-named vars so we do not pollute the ambient env.)
        let cfg = NativeRuntimeConfig::from_env();
        // We can't assert exact values because env may vary; just assert the
        // defaults are sensible non-zero values when the vars are absent.
        assert!(cfg.n_ctx > 0);
        assert!(cfg.n_threads > 0);
        assert!(cfg.n_batch > 0);
        assert!(cfg.max_new_tokens > 0);
    }

    // ── LlamaCppNativeBackend ─────────────────────────────────────────────────

    #[test]
    fn llamacpp_native_backend_has_correct_id() {
        let b = LlamaCppNativeBackend::new();
        assert_eq!(b.id(), "llama-cpp-native");
    }

    #[test]
    fn llamacpp_native_backend_model_id_is_non_empty() {
        let b = LlamaCppNativeBackend::new();
        assert!(!b.model_id().is_empty());
    }

    #[test]
    fn llamacpp_native_backend_max_context_tokens_is_positive() {
        let b = LlamaCppNativeBackend::new();
        assert!(b.max_context_tokens() > 0);
    }

    #[test]
    fn llamacpp_native_backend_fixture_replays_known_prompt() {
        let b = LlamaCppNativeBackend::new();
        let cancel = CancellationToken::new();
        let events = block_on(b.stream_completion("Hello, world!", &cancel)).unwrap();
        assert!(
            matches!(events.last(), Some(StreamingCompletion::Done)),
            "stream must end with Done"
        );
        let tokens: Vec<_> = events
            .iter()
            .filter_map(|e| {
                if let StreamingCompletion::Token(t) = e {
                    Some(t.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert!(
            !tokens.is_empty(),
            "known prompt must yield at least one token"
        );
    }

    #[test]
    fn llamacpp_native_backend_unknown_prompt_yields_sentinel() {
        let b = LlamaCppNativeBackend::new();
        let cancel = CancellationToken::new();
        let events =
            block_on(b.stream_completion("this prompt is not in the fixture", &cancel)).unwrap();
        let has_sentinel = events.iter().any(|e| match e {
            StreamingCompletion::Token(t) => t.contains("llamacpp-native-fixture-not-found"),
            _ => false,
        });
        assert!(has_sentinel, "unknown prompt must yield sentinel token");
    }

    #[test]
    fn llamacpp_native_backend_cancellation_returns_cancelled() {
        let b = LlamaCppNativeBackend::new();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = block_on(b.stream_completion("Hello, world!", &cancel)).unwrap_err();
        assert_eq!(err, LlmBackendError::Cancelled);
    }

    #[test]
    fn llamacpp_native_backend_with_custom_fixtures_round_trips() {
        let fixtures = vec![(
            "test prompt".to_string(),
            vec!["hello".to_string(), " world".to_string()],
        )];
        let b = LlamaCppNativeBackend::with_custom_fixtures("my-model.gguf", fixtures);
        let cancel = CancellationToken::new();
        let events = block_on(b.stream_completion("test prompt", &cancel)).unwrap();
        let tokens: Vec<_> = events
            .iter()
            .filter_map(|e| {
                if let StreamingCompletion::Token(t) = e {
                    Some(t.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(tokens, vec!["hello", " world"]);
    }

    #[test]
    fn llamacpp_native_backend_from_env_is_ci_safe() {
        // from_env() must not panic or start real inference in CI.
        let b = LlamaCppNativeBackend::from_env();
        assert_eq!(b.id(), "llama-cpp-native");
    }

    #[test]
    fn llamacpp_native_backend_estimate_token_count_is_positive_for_non_empty() {
        let b = LlamaCppNativeBackend::new();
        assert!(b.estimate_token_count("hello world") > 0);
    }

    // ── LiteRtLmBackend ───────────────────────────────────────────────────────

    #[test]
    fn litert_lm_backend_has_correct_id() {
        let b = LiteRtLmBackend::new();
        assert_eq!(b.id(), "litert-lm");
    }

    #[test]
    fn litert_lm_backend_model_id_is_non_empty() {
        let b = LiteRtLmBackend::new();
        assert!(!b.model_id().is_empty());
    }

    #[test]
    fn litert_lm_backend_max_context_tokens_is_positive() {
        let b = LiteRtLmBackend::new();
        assert!(b.max_context_tokens() > 0);
    }

    #[test]
    fn litert_lm_backend_fixture_replays_known_prompt() {
        let b = LiteRtLmBackend::new();
        let cancel = CancellationToken::new();
        let events = block_on(b.stream_completion("Hello, world!", &cancel)).unwrap();
        assert!(
            matches!(events.last(), Some(StreamingCompletion::Done)),
            "stream must end with Done"
        );
        let tokens: Vec<_> = events
            .iter()
            .filter_map(|e| {
                if let StreamingCompletion::Token(t) = e {
                    Some(t.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert!(
            !tokens.is_empty(),
            "known prompt must yield at least one token"
        );
    }

    #[test]
    fn litert_lm_backend_unknown_prompt_yields_sentinel() {
        let b = LiteRtLmBackend::new();
        let cancel = CancellationToken::new();
        let events =
            block_on(b.stream_completion("this prompt is not in the litert fixture", &cancel))
                .unwrap();
        let has_sentinel = events.iter().any(|e| match e {
            StreamingCompletion::Token(t) => t.contains("litert-lm-fixture-not-found"),
            _ => false,
        });
        assert!(has_sentinel, "unknown prompt must yield sentinel token");
    }

    #[test]
    fn litert_lm_backend_cancellation_returns_cancelled() {
        let b = LiteRtLmBackend::new();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = block_on(b.stream_completion("Hello, world!", &cancel)).unwrap_err();
        assert_eq!(err, LlmBackendError::Cancelled);
    }

    #[test]
    fn litert_lm_backend_with_custom_fixtures_round_trips() {
        let fixtures = vec![(
            "on-device test".to_string(),
            vec!["fast".to_string(), " inference".to_string()],
        )];
        let b = LiteRtLmBackend::with_custom_fixtures("gemma-test.task", fixtures);
        let cancel = CancellationToken::new();
        let events = block_on(b.stream_completion("on-device test", &cancel)).unwrap();
        let tokens: Vec<_> = events
            .iter()
            .filter_map(|e| {
                if let StreamingCompletion::Token(t) = e {
                    Some(t.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(tokens, vec!["fast", " inference"]);
    }

    #[test]
    fn litert_lm_backend_from_env_is_ci_safe() {
        let b = LiteRtLmBackend::from_env();
        assert_eq!(b.id(), "litert-lm");
    }

    #[test]
    fn litert_lm_backend_estimate_token_count_is_positive_for_non_empty() {
        let b = LiteRtLmBackend::new();
        assert!(b.estimate_token_count("on-device hello") > 0);
    }

    // ── NativeRuntimeError ────────────────────────────────────────────────────

    #[test]
    fn native_runtime_error_display_is_human_readable() {
        assert!(NativeRuntimeError::BackendUnavailable
            .to_string()
            .contains("feature flag"));
        assert!(NativeRuntimeError::ModelLoadFailed("bad path".into())
            .to_string()
            .contains("bad path"));
        assert!(NativeRuntimeError::InferenceFailed("OOM".into())
            .to_string()
            .contains("OOM"));
        assert!(NativeRuntimeError::Cancelled.to_string().contains("cancel"));
    }

    // ── NativeRuntime trait contract ──────────────────────────────────────────

    #[test]
    fn fixture_native_runtime_satisfies_trait_contract() {
        let runtime: Arc<dyn NativeRuntime> = Arc::new(FixtureNativeRuntime::new(
            "test-runtime",
            "test-model",
            2048,
            vec![("hi".to_string(), vec!["hello".to_string()])],
            "[test-sentinel]",
        ));
        assert_eq!(runtime.runtime_id(), "test-runtime");
        assert_eq!(runtime.model_id(), "test-model");
        assert_eq!(runtime.n_ctx(), 2048);

        let cancel = CancellationToken::new();
        let tokens = runtime.generate("hi", 10, &cancel).unwrap();
        assert_eq!(tokens, vec!["hello"]);
    }

    #[test]
    fn fixture_native_runtime_returns_sentinel_for_unknown_prompt() {
        let runtime =
            FixtureNativeRuntime::new("test-runtime", "test-model", 2048, vec![], "[my-sentinel]");
        let cancel = CancellationToken::new();
        let tokens = runtime.generate("unknown", 10, &cancel).unwrap();
        assert_eq!(tokens, vec!["[my-sentinel]"]);
    }

    #[test]
    fn fixture_native_runtime_cancelled_before_first_token_returns_cancelled() {
        let runtime = FixtureNativeRuntime::new(
            "test-runtime",
            "test-model",
            2048,
            vec![("hi".to_string(), vec!["a".to_string(), "b".to_string()])],
            "[sentinel]",
        );
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = runtime.generate("hi", 10, &cancel).unwrap_err();
        assert_eq!(err, NativeRuntimeError::Cancelled);
    }

    // ── config() accessor ─────────────────────────────────────────────────────

    #[test]
    fn llamacpp_config_accessor_returns_config() {
        let b = LlamaCppNativeBackend::new();
        let cfg = b.config();
        assert_eq!(cfg.n_ctx, 4096);
        assert_eq!(cfg.n_threads, 4);
    }

    #[test]
    fn litert_lm_config_accessor_returns_config() {
        let b = LiteRtLmBackend::new();
        let cfg = b.config();
        assert_eq!(cfg.n_ctx, 8192);
        assert_eq!(cfg.n_threads, 4);
    }
}
