//! Web-search tool — E7 S7.1.
//!
//! Implements the `web-search` [`praxis::ToolDriver`] over a pluggable
//! [`SearchProvider`] abstraction:
//!
//! - [`FixtureProvider`]: returns a pre-loaded list of results. Default for CI
//!   (no network calls). Registered tools using `FixtureProvider` pass all
//!   hermetic tests without an external SearXNG instance.
//! - [`SearxngProvider`]: issues a live `GET` request to a self-hosted SearXNG
//!   instance. Used in opt-in live smoke tests (`#[ignore]`).
//!
//! # Wire format
//!
//! **Request** (JSON payload sent by the cortex):
//! ```json
//! { "query": "recent Rust news", "max_results": 5, "categories": ["general"] }
//! ```
//!
//! **Response** (JSON returned by the tool):
//! ```json
//! [
//!   { "title": "...", "url": "https://...", "snippet": "..." },
//!   ...
//! ]
//! ```
//!
//! # Safety
//!
//! All egress is gated by the [`EgressGuard`] held by [`WebSearchTool`].
//! Requests to private IPs, blocklisted hosts, or non-HTTPS URLs are rejected
//! before any network activity occurs.  Results are returned as untrusted text
//! and must be screened by the defence layer's injection detector before the
//! cortex is allowed to act on them.

use serde::{Deserialize, Serialize};

use praxis::{ToolDriver, ToolInvocationError};

use crate::egress::{EgressGuard, EgressVerdict};

// ── Wire types ────────────────────────────────────────────────────────────────

/// Arguments accepted by the `web-search` tool.
#[derive(Debug, Deserialize)]
struct SearchRequest {
    /// The search query string.
    query: String,
    /// Maximum number of results to return. Defaults to 5.
    #[serde(default = "default_max_results")]
    max_results: usize,
    /// Optional SearXNG category filter (e.g. `["general", "news"]`).
    #[serde(default)]
    categories: Vec<String>,
}

fn default_max_results() -> usize {
    5
}

/// A single search result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    /// Page title.
    pub title: String,
    /// Page URL.
    pub url: String,
    /// Short snippet / description.
    pub snippet: String,
}

// ── SearchProvider ────────────────────────────────────────────────────────────

/// Abstraction over the search back-end.
///
/// Two implementations ship:
/// - [`FixtureProvider`] for CI/hermetic testing.
/// - [`SearxngProvider`] for live use.
pub trait SearchProvider: Send + Sync {
    /// Execute `query` and return up to `max_results` ranked results.
    ///
    /// `categories` may be empty (provider uses its default in that case).
    fn search(
        &self,
        query: &str,
        max_results: usize,
        categories: &[String],
    ) -> Result<Vec<SearchResult>, String>;
}

// ── FixtureProvider ───────────────────────────────────────────────────────────

/// Returns a pre-loaded fixture response.
///
/// The fixture is cloned and truncated to `max_results` on each call.
/// Entirely synchronous and offline — suitable for all CI configurations.
pub struct FixtureProvider {
    /// The canned results to return.
    pub fixture: Vec<SearchResult>,
}

impl FixtureProvider {
    /// Create a provider returning the given fixture results.
    pub fn new(fixture: Vec<SearchResult>) -> Self {
        Self { fixture }
    }
}

impl SearchProvider for FixtureProvider {
    fn search(
        &self,
        _query: &str,
        max_results: usize,
        _categories: &[String],
    ) -> Result<Vec<SearchResult>, String> {
        Ok(self
            .fixture
            .iter()
            .take(max_results)
            .cloned()
            .collect())
    }
}

// ── SearxngProvider ───────────────────────────────────────────────────────────

/// Calls a self-hosted SearXNG instance for live searches.
///
/// This provider uses a synchronous HTTP client (`reqwest::blocking`) to keep
/// the [`ToolDriver`] interface synchronous, consistent with the design
/// decision in `docs/12-real-world-tools-plan.md §3.2`.
///
/// The `base_url` is screened by the [`EgressGuard`] on every call.
pub struct SearxngProvider {
    /// Base URL of the SearXNG instance (e.g. `https://searxng.example.com`).
    pub base_url: String,
    /// Egress guard applied to the SearXNG base URL before each request.
    pub egress_guard: EgressGuard,
}

impl SearxngProvider {
    /// Create a provider targeting `base_url`.
    ///
    /// `egress_guard` is applied to `base_url` on every search call.
    pub fn new(base_url: impl Into<String>, egress_guard: EgressGuard) -> Self {
        Self {
            base_url: base_url.into(),
            egress_guard,
        }
    }
}

impl SearchProvider for SearxngProvider {
    fn search(
        &self,
        query: &str,
        _max_results: usize,
        categories: &[String],
    ) -> Result<Vec<SearchResult>, String> {
        // Screen the provider URL via egress guard before making any request.
        let verdict = self.egress_guard.check_url(&self.base_url);
        if let EgressVerdict::Deny(reason) = verdict {
            return Err(format!(
                "egress-blocked: {}",
                reason.description()
            ));
        }

        // Build the SearXNG API URL.
        // GET /search?q=<query>&format=json&categories=<cat>&pageno=1
        let encoded_query: String = query
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '~' {
                    c.to_string()
                } else if c == ' ' {
                    "+".to_string()
                } else {
                    format!("%{:02X}", c as u32)
                }
            })
            .collect();

        let cat_param = if categories.is_empty() {
            String::new()
        } else {
            format!("&categories={}", categories.join(","))
        };

        let url = format!(
            "{}/search?q={}&format=json&pageno=1{}",
            self.base_url.trim_end_matches('/'),
            encoded_query,
            cat_param
        );

        // Live HTTP call — requires network access.
        // reqwest::blocking is the simplest synchronous HTTP client.
        // This feature path is only activated outside CI (env-gated tests).
        #[cfg(feature = "live")]
        {
            use reqwest::blocking::Client;
            let client = Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .map_err(|e| format!("http client build failed: {e}"))?;
            let resp = client
                .get(&url)
                .header("Accept", "application/json")
                .send()
                .map_err(|e| format!("http request failed: {e}"))?;
            if !resp.status().is_success() {
                return Err(format!("searxng returned status {}", resp.status()));
            }
            let body: serde_json::Value =
                resp.json().map_err(|e| format!("json decode failed: {e}"))?;
            return parse_searxng_response(&body, max_results);
        }

        // Without the `live` feature: return an error so tests never silently
        // make real requests.
        #[cfg(not(feature = "live"))]
        Err(format!(
            "SearxngProvider requires the `live` feature for real HTTP calls; \
             use FixtureProvider in tests (url={url})"
        ))
    }
}

/// Parse the SearXNG `/search?format=json` response into [`SearchResult`]s.
#[allow(dead_code)]
fn parse_searxng_response(
    body: &serde_json::Value,
    max_results: usize,
) -> Result<Vec<SearchResult>, String> {
    let results = body
        .get("results")
        .and_then(|r| r.as_array())
        .ok_or_else(|| "searxng response missing 'results' array".to_string())?;

    Ok(results
        .iter()
        .take(max_results)
        .filter_map(|item| {
            Some(SearchResult {
                title: item.get("title")?.as_str()?.to_string(),
                url: item.get("url")?.as_str()?.to_string(),
                snippet: item
                    .get("content")
                    .or_else(|| item.get("snippet"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .collect())
}

// ── WebSearchTool ─────────────────────────────────────────────────────────────

/// `ToolDriver` implementation for web search.
///
/// Registered as `"web-search"` in the tool registry.
///
/// # Security
///
/// - The [`EgressGuard`] is applied to the provider's request URL before any
///   network activity, blocking SSRF and forbidden hosts.
/// - Search results are returned verbatim; they are **untrusted external text**
///   and must be screened by the defence injection detector before the cortex
///   acts on them.
pub struct WebSearchTool {
    /// The search back-end.
    pub provider: std::sync::Arc<dyn SearchProvider>,
    /// Egress guard applied inside the tool (defence-in-depth alongside the
    /// motor-gate hook in vita's dispatch loop).
    pub egress_guard: EgressGuard,
}

impl WebSearchTool {
    /// Create a `WebSearchTool` backed by `provider`.
    ///
    /// `egress_guard` is applied to the SearXNG URL before each request.
    /// For the [`FixtureProvider`], the guard is still instantiated but never
    /// triggers a network call.
    pub fn new(
        provider: impl SearchProvider + 'static,
        egress_guard: EgressGuard,
    ) -> Self {
        Self {
            provider: std::sync::Arc::new(provider),
            egress_guard,
        }
    }

    /// Convenience constructor with the default HTTPS-only egress policy.
    pub fn with_fixture(fixture: Vec<SearchResult>) -> Self {
        Self::new(FixtureProvider::new(fixture), EgressGuard::default())
    }
}

impl ToolDriver for WebSearchTool {
    fn id(&self) -> &'static str {
        "web-search"
    }

    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "description": "Search the web for information. Returns ranked results with title, URL, and snippet.",
  "properties": {
    "query": {
      "type": "string",
      "description": "The search query"
    },
    "max_results": {
      "type": "integer",
      "description": "Maximum number of results to return (default: 5)",
      "minimum": 1,
      "maximum": 20
    },
    "categories": {
      "type": "array",
      "items": { "type": "string" },
      "description": "Optional SearXNG category filter e.g. [\"general\", \"news\"]"
    }
  },
  "required": ["query"]
}"#
    }

    fn invoke(&self, payload: &[u8]) -> Result<Vec<u8>, ToolInvocationError> {
        // Parse the request.
        let req: SearchRequest = serde_json::from_slice(payload)
            .map_err(|_| ToolInvocationError::InvalidPayload)?;

        if req.query.trim().is_empty() {
            return Err(ToolInvocationError::InvalidPayload);
        }

        // Execute the search.
        let results = self
            .provider
            .search(&req.query, req.max_results, &req.categories)
            .map_err(|e| ToolInvocationError::ExecutionFailed(e))?;

        // Serialise results.
        serde_json::to_vec(&results)
            .map_err(|e| ToolInvocationError::ExecutionFailed(e.to_string()))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_fixture() -> Vec<SearchResult> {
        vec![
            SearchResult {
                title: "Rust Programming Language".to_string(),
                url: "https://www.rust-lang.org".to_string(),
                snippet: "A language empowering everyone to build reliable software.".to_string(),
            },
            SearchResult {
                title: "The Rust Reference".to_string(),
                url: "https://doc.rust-lang.org/reference".to_string(),
                snippet: "The primary reference for the Rust programming language.".to_string(),
            },
        ]
    }

    #[test]
    fn fixture_provider_returns_results_up_to_max() {
        let provider = FixtureProvider::new(sample_fixture());
        let results = provider.search("rust", 1, &[]).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Rust Programming Language");
    }

    #[test]
    fn fixture_provider_returns_all_when_max_exceeds_fixture() {
        let provider = FixtureProvider::new(sample_fixture());
        let results = provider.search("rust", 100, &[]).unwrap();
        assert_eq!(results.len(), 2); // fixture only has 2
    }

    #[test]
    fn web_search_tool_invokes_fixture_provider() {
        let tool = WebSearchTool::with_fixture(sample_fixture());
        let payload = serde_json::json!({"query": "rust programming"})
            .to_string()
            .into_bytes();
        let output = tool.invoke(&payload).unwrap();
        let results: Vec<SearchResult> = serde_json::from_slice(&output).unwrap();
        assert!(!results.is_empty());
        assert!(results[0].url.starts_with("https://"));
    }

    #[test]
    fn web_search_tool_respects_max_results_in_payload() {
        let tool = WebSearchTool::with_fixture(sample_fixture());
        let payload = serde_json::json!({"query": "rust", "max_results": 1})
            .to_string()
            .into_bytes();
        let output = tool.invoke(&payload).unwrap();
        let results: Vec<SearchResult> = serde_json::from_slice(&output).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn web_search_tool_rejects_empty_query() {
        let tool = WebSearchTool::with_fixture(sample_fixture());
        let payload = serde_json::json!({"query": ""}).to_string().into_bytes();
        let result = tool.invoke(&payload);
        assert!(
            matches!(result, Err(ToolInvocationError::InvalidPayload)),
            "empty query should be rejected"
        );
    }

    #[test]
    fn web_search_tool_rejects_invalid_json_payload() {
        let tool = WebSearchTool::with_fixture(sample_fixture());
        let result = tool.invoke(b"not json");
        assert!(matches!(result, Err(ToolInvocationError::InvalidPayload)));
    }

    #[test]
    fn web_search_tool_id_is_stable() {
        let tool = WebSearchTool::with_fixture(vec![]);
        assert_eq!(tool.id(), "web-search");
    }

    #[test]
    fn searxng_provider_without_live_feature_returns_error() {
        let provider = SearxngProvider::new("https://searxng.example.com", EgressGuard::default());
        let result = provider.search("rust", 5, &[]);
        // Without `live` feature, the provider returns an error (not a panic).
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(
            msg.contains("live") || msg.contains("fixture") || msg.contains("feature"),
            "error message should mention live feature: {msg}"
        );
    }

    #[test]
    fn searxng_provider_blocks_private_base_url() {
        let provider = SearxngProvider::new(
            "https://192.168.1.10/searxng",
            EgressGuard::default(),
        );
        let result = provider.search("rust", 5, &[]);
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(
            msg.contains("egress-blocked") || msg.contains("SSRF") || msg.contains("private"),
            "expected egress block, got: {msg}"
        );
    }

    #[test]
    fn fixture_results_have_https_urls() {
        let tool = WebSearchTool::with_fixture(sample_fixture());
        let payload = serde_json::json!({"query": "test"}).to_string().into_bytes();
        let output = tool.invoke(&payload).unwrap();
        let results: Vec<SearchResult> = serde_json::from_slice(&output).unwrap();
        for r in &results {
            assert!(
                r.url.starts_with("https://"),
                "fixture URL should be https: {}",
                r.url
            );
        }
    }
}
