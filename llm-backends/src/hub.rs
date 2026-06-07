//! Hugging Face Hub model discovery (E8 S8.2.3).
//!
//! [`HfHubClient`] resolves model metadata — context window, model type, and
//! supported features — from the Hugging Face Hub API.  The default is
//! **fixture mode** (CI-safe, no network); live mode is opt-in via
//! `ANIMA_HF_LIVE=1`.
//!
//! # Usage
//!
//! ```rust
//! use llm_backends::hub::HfHubClient;
//!
//! let client = HfHubClient::new();
//! let info = client.fetch_model_info("microsoft/Phi-3.5-mini-instruct").unwrap();
//! assert_eq!(info.context_window, Some(131_072));
//! assert!(info.tools_support);
//! ```

use serde::{Deserialize, Serialize};

// ── Model metadata ─────────────────────────────────────────────────────────────

/// Metadata about a model retrieved from the Hugging Face Hub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HfModelInfo {
    /// Canonical model identifier, e.g. `"meta-llama/Llama-3.2-3B-Instruct"`.
    pub model_id: String,
    /// Resolved context window in tokens, when reported in the Hub model card.
    pub context_window: Option<u32>,
    /// Coarse model architecture, e.g. `"llama"`, `"mistral"`, `"phi3"`.
    pub model_type: Option<String>,
    /// Task and library tags from the Hub card metadata.
    pub tags: Vec<String>,
    /// Whether the Hub card indicates the model supports tool/function calling.
    pub tools_support: bool,
}

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors from Hub interactions.
#[derive(Debug)]
pub enum HubError {
    /// Model not found (404 in live mode; absent from fixture table).
    NotFound(String),
    /// HTTP or network error (live mode only).
    Network(String),
    /// Failed to parse the Hub API response.
    Parse(String),
}

impl std::fmt::Display for HubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(id) => write!(f, "model not found on HF Hub: {id}"),
            Self::Network(msg) => write!(f, "HF Hub network error: {msg}"),
            Self::Parse(msg) => write!(f, "HF Hub parse error: {msg}"),
        }
    }
}

impl std::error::Error for HubError {}

// ── Embedded fixture data ──────────────────────────────────────────────────────

/// Fixture records for common models used in the CI test suite and doctor wizard.
///
/// Each entry is `(model_id, json_serialised_HfModelInfo)`.  The JSON encoding
/// matches the [`HfModelInfo`] struct so fixture records round-trip cleanly
/// through serde without a separate struct.
static FIXTURE_MODELS: &[(&str, &str)] = &[
    (
        "microsoft/Phi-3.5-mini-instruct",
        r#"{"model_id":"microsoft/Phi-3.5-mini-instruct","context_window":131072,"model_type":"phi3","tags":["text-generation","phi","instruct"],"tools_support":true}"#,
    ),
    (
        "meta-llama/Llama-3.2-3B-Instruct",
        r#"{"model_id":"meta-llama/Llama-3.2-3B-Instruct","context_window":131072,"model_type":"llama","tags":["text-generation","llama","instruct"],"tools_support":true}"#,
    ),
    (
        "mistralai/Mistral-7B-Instruct-v0.3",
        r#"{"model_id":"mistralai/Mistral-7B-Instruct-v0.3","context_window":32768,"model_type":"mistral","tags":["text-generation","mistral","instruct"],"tools_support":false}"#,
    ),
    (
        "google/gemma-2-2b-it",
        r#"{"model_id":"google/gemma-2-2b-it","context_window":8192,"model_type":"gemma2","tags":["text-generation","gemma","instruct"],"tools_support":false}"#,
    ),
    (
        "Qwen/Qwen2.5-7B-Instruct",
        r#"{"model_id":"Qwen/Qwen2.5-7B-Instruct","context_window":131072,"model_type":"qwen2","tags":["text-generation","qwen","instruct"],"tools_support":true}"#,
    ),
    (
        "bartowski/Phi-3.5-mini-instruct-GGUF",
        r#"{"model_id":"bartowski/Phi-3.5-mini-instruct-GGUF","context_window":131072,"model_type":"phi3","tags":["text-generation","phi","gguf"],"tools_support":true}"#,
    ),
    (
        "unsloth/Phi-3.5-mini-instruct",
        r#"{"model_id":"unsloth/Phi-3.5-mini-instruct","context_window":131072,"model_type":"phi3","tags":["text-generation","phi","unsloth","instruct"],"tools_support":true}"#,
    ),
    (
        "NousResearch/Hermes-3-Llama-3.1-8B",
        r#"{"model_id":"NousResearch/Hermes-3-Llama-3.1-8B","context_window":131072,"model_type":"llama","tags":["text-generation","llama","instruct","function-calling"],"tools_support":true}"#,
    ),
];

// ── Client ────────────────────────────────────────────────────────────────────

enum HubMode {
    Fixture,
    /// Live HTTP mode — requires `ANIMA_HF_LIVE=1`.  Uses blocking `ureq`.
    Live {
        api_token: Option<String>,
        agent: ureq::Agent,
    },
}

/// Client for the Hugging Face Hub model-discovery API (E8 S8.2.3).
///
/// # Modes
///
/// | Mode    | How to activate                       | Network? |
/// |---------|---------------------------------------|----------|
/// | Fixture | default                               | no       |
/// | Live    | `ANIMA_HF_LIVE=1` env var             | yes      |
///
/// In fixture mode the client performs a linear scan over the embedded
/// [`FIXTURE_MODELS`] table, which covers the models used during development
/// and in the `anima doctor` wizard.
pub struct HfHubClient {
    mode: HubMode,
}

impl HfHubClient {
    /// Returns a CI-safe fixture-mode client.
    pub fn new() -> Self {
        Self {
            mode: HubMode::Fixture,
        }
    }

    /// Returns a live-mode client that queries `huggingface.co/api`.
    ///
    /// Reads `HF_TOKEN` from the environment for private models and higher
    /// rate limits; works without a token for public models.
    pub fn live() -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(30)))
            .build()
            .into();
        Self {
            mode: HubMode::Live {
                api_token: std::env::var("HF_TOKEN").ok(),
                agent,
            },
        }
    }

    /// Returns a client whose mode is selected from the environment.
    ///
    /// Uses live mode when `ANIMA_HF_LIVE=1`; otherwise fixture mode.
    pub fn from_env() -> Self {
        if std::env::var("ANIMA_HF_LIVE")
            .map(|v| v == "1")
            .unwrap_or(false)
        {
            Self::live()
        } else {
            Self::new()
        }
    }

    /// Fetches metadata for `model_id`.
    ///
    /// In fixture mode performs an O(n) scan of the embedded table; in live
    /// mode issues a GET request to `https://huggingface.co/api/models/{id}`.
    pub fn fetch_model_info(&self, model_id: &str) -> Result<HfModelInfo, HubError> {
        match &self.mode {
            HubMode::Fixture => self.fixture_lookup(model_id),
            HubMode::Live { api_token, agent } => {
                self.live_fetch(model_id, api_token.as_deref(), agent)
            }
        }
    }

    /// Resolves the context window for `model_id`, returning `None` when the
    /// model is unknown or when the Hub card does not report a context size.
    ///
    /// This is the convenience entry point used by `anima doctor` and the init
    /// wizard to populate [`crate::capabilities::ProviderConfig::max_context_tokens`].
    pub fn resolve_context_window(&self, model_id: &str) -> Option<u32> {
        self.fetch_model_info(model_id)
            .ok()
            .and_then(|info| info.context_window)
    }

    /// Returns `true` if the model is known to the fixture table (or Hub in
    /// live mode) and the model's card indicates tool-calling support.
    pub fn supports_tools(&self, model_id: &str) -> bool {
        self.fetch_model_info(model_id)
            .map(|i| i.tools_support)
            .unwrap_or(false)
    }

    /// Returns all fixture models known to this client (fixture mode only).
    ///
    /// Useful for auto-completing model IDs in the init wizard.
    pub fn fixture_model_ids() -> &'static [&'static str] {
        // Safety: slice of static string references.
        // SAFETY: FIXTURE_MODELS is a static slice; this view is sound.
        static IDS: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
        IDS.get_or_init(|| FIXTURE_MODELS.iter().map(|(id, _)| *id).collect())
            .as_slice()
    }

    // ── Private ───────────────────────────────────────────────────────────────

    fn fixture_lookup(&self, model_id: &str) -> Result<HfModelInfo, HubError> {
        for (id, json) in FIXTURE_MODELS {
            if *id == model_id {
                return serde_json::from_str(json).map_err(|e| HubError::Parse(e.to_string()));
            }
        }
        Err(HubError::NotFound(model_id.to_string()))
    }

    fn live_fetch(
        &self,
        model_id: &str,
        api_token: Option<&str>,
        agent: &ureq::Agent,
    ) -> Result<HfModelInfo, HubError> {
        let url = format!("https://huggingface.co/api/models/{model_id}");
        let mut req = agent.get(&url).header("Accept", "application/json");
        if let Some(token) = api_token {
            req = req.header("Authorization", &format!("Bearer {token}"));
        }

        let response = match req.call() {
            Ok(res) => res,
            Err(ureq::Error::StatusCode(404)) => {
                return Err(HubError::NotFound(model_id.to_string()));
            }
            Err(e) => return Err(HubError::Network(e.to_string())),
        };

        let body_text = response
            .into_body()
            .read_to_string()
            .map_err(|e| HubError::Parse(e.to_string()))?;

        let body: serde_json::Value =
            serde_json::from_str(&body_text).map_err(|e| HubError::Parse(e.to_string()))?;

        Self::parse_hub_response(model_id, &body)
    }

    fn parse_hub_response(
        model_id: &str,
        body: &serde_json::Value,
    ) -> Result<HfModelInfo, HubError> {
        let context_window = body
            .pointer("/config/max_position_embeddings")
            .or_else(|| body.pointer("/config/max_seq_len"))
            .or_else(|| body.pointer("/config/n_positions"))
            .and_then(|v| v.as_u64())
            .and_then(|v| u32::try_from(v).ok());

        let model_type = body
            .pointer("/config/model_type")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let tags: Vec<String> = body
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t.as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        let tools_support = tags.iter().any(|t: &String| {
            t == "function-calling"
                || t == "tool-use"
                || t == "tool_use"
                || t.contains("function-calling")
        });

        Ok(HfModelInfo {
            model_id: model_id.to_string(),
            context_window,
            model_type,
            tags,
            tools_support,
        })
    }
}

impl Default for HfHubClient {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_lookup_finds_phi35_mini() {
        let client = HfHubClient::new();
        let info = client
            .fetch_model_info("microsoft/Phi-3.5-mini-instruct")
            .unwrap();
        assert_eq!(info.model_id, "microsoft/Phi-3.5-mini-instruct");
        assert_eq!(info.context_window, Some(131_072));
        assert_eq!(info.model_type.as_deref(), Some("phi3"));
        assert!(info.tools_support);
    }

    #[test]
    fn fixture_lookup_finds_llama32_3b() {
        let client = HfHubClient::new();
        let info = client
            .fetch_model_info("meta-llama/Llama-3.2-3B-Instruct")
            .unwrap();
        assert_eq!(info.context_window, Some(131_072));
        assert!(info.tools_support);
    }

    #[test]
    fn fixture_lookup_finds_mistral_7b_without_tools() {
        let client = HfHubClient::new();
        let info = client
            .fetch_model_info("mistralai/Mistral-7B-Instruct-v0.3")
            .unwrap();
        assert_eq!(info.context_window, Some(32_768));
        assert!(!info.tools_support);
    }

    #[test]
    fn fixture_lookup_finds_gemma2_2b() {
        let client = HfHubClient::new();
        let info = client.fetch_model_info("google/gemma-2-2b-it").unwrap();
        assert_eq!(info.context_window, Some(8_192));
        assert!(!info.tools_support);
    }

    #[test]
    fn fixture_lookup_finds_qwen25_7b() {
        let client = HfHubClient::new();
        let info = client.fetch_model_info("Qwen/Qwen2.5-7B-Instruct").unwrap();
        assert_eq!(info.context_window, Some(131_072));
        assert!(info.tools_support);
    }

    #[test]
    fn fixture_lookup_returns_not_found_for_unknown_model() {
        let client = HfHubClient::new();
        let err = client
            .fetch_model_info("nonexistent/totally-made-up-model")
            .unwrap_err();
        assert!(
            matches!(err, HubError::NotFound(_)),
            "expected NotFound, got {err}"
        );
        assert!(err.to_string().contains("totally-made-up-model"));
    }

    #[test]
    fn resolve_context_window_returns_some_for_known_model() {
        let client = HfHubClient::new();
        assert_eq!(
            client.resolve_context_window("microsoft/Phi-3.5-mini-instruct"),
            Some(131_072)
        );
    }

    #[test]
    fn resolve_context_window_returns_none_for_unknown_model() {
        let client = HfHubClient::new();
        assert_eq!(client.resolve_context_window("no/such-model"), None);
    }

    #[test]
    fn supports_tools_returns_true_for_phi35() {
        let client = HfHubClient::new();
        assert!(client.supports_tools("microsoft/Phi-3.5-mini-instruct"));
    }

    #[test]
    fn supports_tools_returns_false_for_mistral_v03() {
        let client = HfHubClient::new();
        assert!(!client.supports_tools("mistralai/Mistral-7B-Instruct-v0.3"));
    }

    #[test]
    fn supports_tools_returns_false_for_unknown_model() {
        let client = HfHubClient::new();
        assert!(!client.supports_tools("unknown/model-that-does-not-exist"));
    }

    #[test]
    fn from_env_returns_fixture_mode_when_live_not_set() {
        std::env::remove_var("ANIMA_HF_LIVE");
        let client = HfHubClient::from_env();
        // Can still look up fixture models.
        assert!(client.fetch_model_info("google/gemma-2-2b-it").is_ok());
    }

    #[test]
    fn fixture_model_ids_lists_all_embedded_models() {
        let ids = HfHubClient::fixture_model_ids();
        assert_eq!(ids.len(), FIXTURE_MODELS.len());
        assert!(ids.contains(&"microsoft/Phi-3.5-mini-instruct"));
        assert!(ids.contains(&"meta-llama/Llama-3.2-3B-Instruct"));
    }

    #[test]
    fn default_is_fixture_mode() {
        let client = HfHubClient::default();
        assert!(client.fetch_model_info("Qwen/Qwen2.5-7B-Instruct").is_ok());
    }

    #[test]
    fn model_info_serialises_and_deserialises() {
        let client = HfHubClient::new();
        let info = client
            .fetch_model_info("microsoft/Phi-3.5-mini-instruct")
            .unwrap();
        let json = serde_json::to_string(&info).unwrap();
        let round_tripped: HfModelInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(info, round_tripped);
    }

    #[test]
    fn parse_hub_response_extracts_context_window() {
        let body = serde_json::json!({
            "config": { "max_position_embeddings": 4096, "model_type": "llama" },
            "tags": ["text-generation", "function-calling"]
        });
        let info = HfHubClient::parse_hub_response("test/model", &body).unwrap();
        assert_eq!(info.context_window, Some(4_096));
        assert_eq!(info.model_type.as_deref(), Some("llama"));
        assert!(info.tools_support);
    }

    #[test]
    fn parse_hub_response_handles_missing_context_window() {
        let body = serde_json::json!({ "config": {}, "tags": [] });
        let info = HfHubClient::parse_hub_response("test/no-ctx", &body).unwrap();
        assert_eq!(info.context_window, None);
    }

    #[test]
    fn unsloth_model_is_in_fixture_table() {
        let client = HfHubClient::new();
        let info = client
            .fetch_model_info("unsloth/Phi-3.5-mini-instruct")
            .unwrap();
        assert!(info.tools_support);
    }
}
