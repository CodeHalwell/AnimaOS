//! Browser tool — E7 S7.2.
//!
//! Implements the `browser` family of [`praxis::ToolDriver`]s over a pluggable
//! [`BrowserDriver`] abstraction, mirroring the structure of
//! [`crate::web_search`]:
//!
//! - [`MockBrowserDriver`]: returns canned page state keyed by URL. Default for
//!   CI — performs **no** network access and spawns **no** subprocess, so every
//!   hermetic test passes without Node, Chromium, or Playwright installed.
//! - [`PlaywrightDriver`]: drives a live Playwright worker subprocess over a
//!   length-prefixed-JSON-over-UDS protocol (the same wire shape used by the
//!   cortex bridge). **Gated behind `feature = "live"`** and never compiled into
//!   CI. Used in opt-in live smoke tests (`#[ignore]`).
//!
//! # Wire format
//!
//! The dispatcher routes browser tools with a `{"url": "..."}` payload (see
//! `crates/vita/src/dispatch.rs`). Each tool accepts a small JSON request and
//! returns a JSON response.
//!
//! **`browser` (navigate)** — request `{ "url": "https://..." }`,
//! response [`PageState`] as JSON:
//! ```json
//! { "url": "https://...", "title": "...", "text": "..." }
//! ```
//!
//! **`browse` (read text)** — request `{ "url": "https://..." }`,
//! response `{ "text": "..." }`.
//!
//! **`extract`** — request `{ "url": "https://...", "selector": "h1" }`,
//! response `["item one", "item two", ...]`.
//!
//! # Safety
//!
//! Every navigation is screened by the [`EgressGuard`] held by each tool
//! **before any network access or subprocess command is issued**. Requests to
//! private IPs, blocklisted hosts, or non-HTTPS URLs are rejected up front.
//! Extracted page content is returned as **untrusted external text** and must be
//! screened by the defence layer's injection detector before the cortex acts on
//! it.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use praxis::{ToolDriver, ToolInvocationError};

use crate::egress::{EgressGuard, EgressVerdict};

// ── Wire types ────────────────────────────────────────────────────────────────

/// Request accepted by [`BrowserNavigateTool`] and [`BrowserReadTextTool`].
#[derive(Debug, Deserialize)]
struct NavigateRequest {
    /// The URL to load.
    url: String,
}

/// Request accepted by [`BrowserExtractTool`].
#[derive(Debug, Deserialize)]
struct ExtractRequest {
    /// The URL to load.
    url: String,
    /// CSS-style selector identifying the elements to extract.
    selector: String,
}

/// Snapshot of a loaded page returned by [`BrowserDriver::navigate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageState {
    /// The final URL of the page (after any redirects, for the live driver).
    pub url: String,
    /// The page `<title>`.
    pub title: String,
    /// The readable text content of the page.
    pub text: String,
}

/// Response body for [`BrowserReadTextTool`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ReadTextResponse {
    /// The readable text content of the page.
    text: String,
}

// ── BrowserDriver ───────────────────────────────────────────────────────────--

/// Abstraction over the browser back-end.
///
/// Two implementations ship:
/// - [`MockBrowserDriver`] for CI/hermetic testing (canned, offline).
/// - [`PlaywrightDriver`] for live use (subprocess, `feature = "live"` only).
///
/// All methods are synchronous, consistent with the [`ToolDriver`] interface and
/// the design decision in `docs/12-real-world-tools-plan.md §3.2`.
///
/// Implementations **must not** perform network access for a URL that has not
/// passed [`EgressGuard::check_url`]; the tool drivers enforce this before
/// calling [`BrowserDriver::navigate`], and live drivers re-check internally as
/// defence-in-depth.
pub trait BrowserDriver: Send + Sync {
    /// Load `url` and return its [`PageState`] (url, title, readable text).
    fn navigate(&self, url: &str) -> Result<PageState, String>;

    /// Load `url` and return only its readable text content.
    ///
    /// The default implementation delegates to [`BrowserDriver::navigate`] and
    /// returns the `text` field; drivers may override for efficiency.
    fn read_text(&self, url: &str) -> Result<String, String> {
        Ok(self.navigate(url)?.text)
    }

    /// Load `url` and return the text content of every element matching
    /// `selector`, in document order.
    fn extract(&self, url: &str, selector: &str) -> Result<Vec<String>, String>;
}

// ── MockBrowserDriver ─────────────────────────────────────────────────────────

/// Canned page returned by the mock driver for a single URL.
#[derive(Debug, Clone, Default)]
pub struct MockPage {
    /// The `<title>` returned for this URL.
    pub title: String,
    /// The readable text returned for this URL.
    pub text: String,
    /// Extraction results keyed by selector. A missing selector yields `[]`.
    pub extractions: HashMap<String, Vec<String>>,
}

impl MockPage {
    /// Build a page with the given title and text and no extractions.
    pub fn new(title: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            text: text.into(),
            extractions: HashMap::new(),
        }
    }

    /// Add a canned extraction result for `selector`.
    #[must_use]
    pub fn with_extraction(mut self, selector: impl Into<String>, items: Vec<String>) -> Self {
        self.extractions.insert(selector.into(), items);
        self
    }
}

/// Returns pre-loaded page state keyed by URL.
///
/// Entirely synchronous and offline — spawns no subprocess and opens no socket,
/// so it is safe in every CI configuration. Navigation to a URL absent from the
/// fixture map yields an error (so tests are explicit about what is mocked).
#[derive(Debug, Default)]
pub struct MockBrowserDriver {
    /// Canned pages keyed by their exact URL.
    pub pages: HashMap<String, MockPage>,
}

impl MockBrowserDriver {
    /// Create an empty mock driver.
    pub fn new() -> Self {
        Self {
            pages: HashMap::new(),
        }
    }

    /// Register a canned `page` for `url`, returning `self` for chaining.
    #[must_use]
    pub fn with_page(mut self, url: impl Into<String>, page: MockPage) -> Self {
        self.pages.insert(url.into(), page);
        self
    }

    /// Look up the canned page for `url`, or an error if none is registered.
    fn lookup(&self, url: &str) -> Result<&MockPage, String> {
        self.pages
            .get(url)
            .ok_or_else(|| format!("mock-browser: no canned page for url {url:?}"))
    }
}

impl BrowserDriver for MockBrowserDriver {
    fn navigate(&self, url: &str) -> Result<PageState, String> {
        let page = self.lookup(url)?;
        Ok(PageState {
            url: url.to_string(),
            title: page.title.clone(),
            text: page.text.clone(),
        })
    }

    fn read_text(&self, url: &str) -> Result<String, String> {
        Ok(self.lookup(url)?.text.clone())
    }

    fn extract(&self, url: &str, selector: &str) -> Result<Vec<String>, String> {
        let page = self.lookup(url)?;
        // A missing selector returns an empty list rather than an error: the page
        // loaded fine, it simply contains no matching elements.
        Ok(page.extractions.get(selector).cloned().unwrap_or_default())
    }
}

// ── PlaywrightDriver (live, feature-gated) ────────────────────────────────────

/// Live browser driver backed by a Playwright worker subprocess.
///
/// **Compiled only under `feature = "live"`** and therefore never built in CI.
/// The driver speaks the same length-prefixed-JSON-over-UDS protocol as the
/// cortex bridge (`crates/vita/src/cortex_bridge.rs`): a 4-byte big-endian
/// length header followed by a JSON body, in both directions.
///
/// # Lifecycle
///
/// This is a deliberately minimal, per-call skeleton: each navigation spawns the
/// worker, performs one request/response round-trip, and tears the process down
/// via the [`ChildGuard`] RAII pattern (kill + reap + socket cleanup on drop).
/// A production implementation (`S7.2.2 BrowserBridge`) would keep one
/// long-lived browser context and reuse it across calls with page-level
/// resource limits; that optimisation is out of scope for the trait + mock + CI
/// slice and is left as a clearly-marked extension point.
///
/// # Safety
///
/// Every method screens the target URL via [`EgressGuard::check_url`] **before**
/// spawning the worker or sending any command — defence-in-depth alongside the
/// motor-gate hook in vita's dispatch loop.
#[cfg(feature = "live")]
pub struct PlaywrightDriver {
    /// Path to the Playwright worker script (Node or Python entrypoint).
    pub worker_cmd: String,
    /// Arguments to pass before the `--socket <path>` flag.
    pub worker_args: Vec<String>,
    /// Working directory for the worker process.
    pub workspace_root: std::path::PathBuf,
    /// Per-action timeout.
    pub timeout: std::time::Duration,
    /// Egress guard applied to every navigation URL before any subprocess work.
    pub egress_guard: EgressGuard,
}

#[cfg(feature = "live")]
impl PlaywrightDriver {
    /// Create a driver that launches `worker_cmd` for each navigation.
    pub fn new(
        worker_cmd: impl Into<String>,
        workspace_root: impl Into<std::path::PathBuf>,
        egress_guard: EgressGuard,
    ) -> Self {
        Self {
            worker_cmd: worker_cmd.into(),
            worker_args: Vec::new(),
            workspace_root: workspace_root.into(),
            timeout: std::time::Duration::from_secs(30),
            egress_guard,
        }
    }

    /// Screen `url` and, if allowed, run one `command` round-trip against a
    /// freshly-spawned worker, returning the worker's JSON `result` value.
    ///
    /// The worker protocol is:
    /// - vita → worker: `{ "command": "<cmd>", "url": "<url>", "selector"?: ... }`
    /// - worker → vita: `{ "ok": true, "result": <value> }`
    ///   or `{ "ok": false, "error": "<message>" }`
    fn run_command(
        &self,
        command: &str,
        url: &str,
        selector: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixListener;
        use std::process::{Command, Stdio};

        // Screen the URL BEFORE any subprocess or socket work.
        if let EgressVerdict::Deny(reason) = self.egress_guard.check_url(url) {
            return Err(format!("egress-blocked: {}", reason.description()));
        }

        // Per-call UDS, mirroring the cortex-bridge convention.
        let socket_path = std::env::temp_dir().join(format!(
            "anima-browser-{}-{}.sock",
            std::process::id(),
            // Cheap unique suffix; avoids pulling in extra deps.
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path)
            .map_err(|e| format!("browser: bind UDS failed: {e}"))?;

        let mut cmd = Command::new(&self.worker_cmd);
        cmd.args(&self.worker_args)
            .arg("--socket")
            .arg(&socket_path)
            .current_dir(&self.workspace_root)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = cmd
            .spawn()
            .map_err(|e| format!("browser: spawn worker failed: {e}"))?;

        // RAII: kill + reap the worker and remove the socket on any error path.
        let guard = ChildGuard::new(child, socket_path.clone());

        // Accept the worker connection (blocking).
        let (mut stream, _) = listener
            .accept()
            .map_err(|e| format!("browser: accept failed: {e}"))?;

        // Send the command frame.
        let mut req = serde_json::Map::new();
        req.insert("command".to_string(), serde_json::json!(command));
        req.insert("url".to_string(), serde_json::json!(url));
        if let Some(sel) = selector {
            req.insert("selector".to_string(), serde_json::json!(sel));
        }
        let body = serde_json::to_vec(&serde_json::Value::Object(req))
            .map_err(|e| format!("browser: serialise request failed: {e}"))?;
        let len = (body.len() as u32).to_be_bytes();
        stream
            .write_all(&len)
            .and_then(|_| stream.write_all(&body))
            .map_err(|e| format!("browser: write request failed: {e}"))?;

        // Read the length-prefixed JSON response.
        let mut header = [0u8; 4];
        stream
            .read_exact(&mut header)
            .map_err(|e| format!("browser: read header failed: {e}"))?;
        let resp_len = u32::from_be_bytes(header) as usize;
        let mut resp_buf = vec![0u8; resp_len];
        stream
            .read_exact(&mut resp_buf)
            .map_err(|e| format!("browser: read body failed: {e}"))?;
        let resp: serde_json::Value = serde_json::from_slice(&resp_buf)
            .map_err(|e| format!("browser: parse response failed: {e}"))?;

        // Success path: take ownership and reap explicitly so the worker exits
        // cleanly rather than being killed.
        let (mut owned_child, owned_socket) = guard.into_inner();
        drop(stream);
        let _ = owned_child.wait();
        let _ = std::fs::remove_file(&owned_socket);

        if resp.get("ok").and_then(|v| v.as_bool()) == Some(true) {
            Ok(resp
                .get("result")
                .cloned()
                .unwrap_or(serde_json::Value::Null))
        } else {
            Err(resp
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("browser: worker reported failure")
                .to_string())
        }
    }
}

#[cfg(feature = "live")]
impl BrowserDriver for PlaywrightDriver {
    fn navigate(&self, url: &str) -> Result<PageState, String> {
        let result = self.run_command("navigate", url, None)?;
        Ok(PageState {
            url: result
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or(url)
                .to_string(),
            title: result
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            text: result
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        })
    }

    fn read_text(&self, url: &str) -> Result<String, String> {
        let result = self.run_command("read_text", url, None)?;
        Ok(result
            .get("text")
            .and_then(|v| v.as_str())
            .or_else(|| result.as_str())
            .unwrap_or("")
            .to_string())
    }

    fn extract(&self, url: &str, selector: &str) -> Result<Vec<String>, String> {
        let result = self.run_command("extract", url, Some(selector))?;
        let arr = result
            .as_array()
            .or_else(|| result.get("items").and_then(|v| v.as_array()))
            .ok_or_else(|| "browser: extract result is not an array".to_string())?;
        Ok(arr
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect())
    }
}

/// RAII guard that kills and reaps the Playwright worker on drop.
///
/// Mirrors the `ChildGuard` pattern in `crates/vita/src/cortex_bridge.rs`: on an
/// error path simply `return Err(…)` and the `Drop` impl kills + waits the
/// process and removes the socket; on the success path call
/// [`ChildGuard::into_inner`] to take ownership and reap explicitly.
#[cfg(feature = "live")]
struct ChildGuard {
    inner: Option<(std::process::Child, std::path::PathBuf)>,
}

#[cfg(feature = "live")]
impl ChildGuard {
    fn new(child: std::process::Child, socket_path: std::path::PathBuf) -> Self {
        Self {
            inner: Some((child, socket_path)),
        }
    }

    fn into_inner(mut self) -> (std::process::Child, std::path::PathBuf) {
        self.inner
            .take()
            .expect("ChildGuard::into_inner called twice")
    }
}

#[cfg(feature = "live")]
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some((mut child, socket_path)) = self.inner.take() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&socket_path);
        }
    }
}

// ── Tool drivers ──────────────────────────────────────────────────────────────

/// Shared egress screening for the browser tools.
///
/// Returns [`ToolInvocationError::ExecutionFailed`] with an `egress-blocked:`
/// prefix when the URL is denied, matching the dispatcher's audit expectations.
fn screen_url(guard: &EgressGuard, url: &str) -> Result<(), ToolInvocationError> {
    if let EgressVerdict::Deny(reason) = guard.check_url(url) {
        return Err(ToolInvocationError::ExecutionFailed(format!(
            "egress-blocked: {}",
            reason.description()
        )));
    }
    Ok(())
}

/// `ToolDriver` that navigates to a URL and returns its [`PageState`].
///
/// Registered as `"browser"` — the primary browser-navigation tool id used by
/// the dispatcher's network-tool allow-list.
///
/// # Security
///
/// The [`EgressGuard`] is applied to the target URL **before** the driver is
/// invoked, blocking SSRF and forbidden schemes. The returned page text is
/// untrusted external content.
pub struct BrowserNavigateTool {
    /// The browser back-end.
    pub driver: std::sync::Arc<dyn BrowserDriver>,
    /// Egress guard applied inside the tool (defence-in-depth).
    pub egress_guard: EgressGuard,
}

impl BrowserNavigateTool {
    /// Create a `BrowserNavigateTool` backed by `driver`.
    pub fn new(driver: impl BrowserDriver + 'static, egress_guard: EgressGuard) -> Self {
        Self {
            driver: std::sync::Arc::new(driver),
            egress_guard,
        }
    }

    /// Convenience constructor backed by a [`MockBrowserDriver`] with the default
    /// HTTPS-only egress policy. CI-safe.
    pub fn with_mock(driver: MockBrowserDriver) -> Self {
        Self::new(driver, EgressGuard::default())
    }
}

impl ToolDriver for BrowserNavigateTool {
    fn id(&self) -> &'static str {
        "browser"
    }

    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "description": "Navigate a browser to a URL and return the page title and readable text.",
  "properties": {
    "url": {
      "type": "string",
      "description": "The absolute https:// URL to load"
    }
  },
  "required": ["url"]
}"#
    }

    fn invoke(&self, payload: &[u8]) -> Result<Vec<u8>, ToolInvocationError> {
        let req: NavigateRequest =
            serde_json::from_slice(payload).map_err(|_| ToolInvocationError::InvalidPayload)?;
        if req.url.trim().is_empty() {
            return Err(ToolInvocationError::InvalidPayload);
        }

        // Screen BEFORE any navigation / network access.
        screen_url(&self.egress_guard, &req.url)?;

        let page = self
            .driver
            .navigate(&req.url)
            .map_err(ToolInvocationError::ExecutionFailed)?;

        serde_json::to_vec(&page).map_err(|e| ToolInvocationError::ExecutionFailed(e.to_string()))
    }
}

/// `ToolDriver` that loads a URL and returns only its readable text.
///
/// Registered as `"browse"` — a read-only convenience over navigation.
pub struct BrowserReadTextTool {
    /// The browser back-end.
    pub driver: std::sync::Arc<dyn BrowserDriver>,
    /// Egress guard applied inside the tool.
    pub egress_guard: EgressGuard,
}

impl BrowserReadTextTool {
    /// Create a `BrowserReadTextTool` backed by `driver`.
    pub fn new(driver: impl BrowserDriver + 'static, egress_guard: EgressGuard) -> Self {
        Self {
            driver: std::sync::Arc::new(driver),
            egress_guard,
        }
    }

    /// Convenience constructor backed by a [`MockBrowserDriver`]. CI-safe.
    pub fn with_mock(driver: MockBrowserDriver) -> Self {
        Self::new(driver, EgressGuard::default())
    }
}

impl ToolDriver for BrowserReadTextTool {
    fn id(&self) -> &'static str {
        "browse"
    }

    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "description": "Load a URL and return its readable text content.",
  "properties": {
    "url": {
      "type": "string",
      "description": "The absolute https:// URL to load"
    }
  },
  "required": ["url"]
}"#
    }

    fn invoke(&self, payload: &[u8]) -> Result<Vec<u8>, ToolInvocationError> {
        let req: NavigateRequest =
            serde_json::from_slice(payload).map_err(|_| ToolInvocationError::InvalidPayload)?;
        if req.url.trim().is_empty() {
            return Err(ToolInvocationError::InvalidPayload);
        }

        screen_url(&self.egress_guard, &req.url)?;

        let text = self
            .driver
            .read_text(&req.url)
            .map_err(ToolInvocationError::ExecutionFailed)?;

        serde_json::to_vec(&ReadTextResponse { text })
            .map_err(|e| ToolInvocationError::ExecutionFailed(e.to_string()))
    }
}

/// `ToolDriver` that loads a URL and extracts text for a CSS selector.
///
/// Registered as `"extract"`. Returns a JSON array of the matching elements'
/// text content.
pub struct BrowserExtractTool {
    /// The browser back-end.
    pub driver: std::sync::Arc<dyn BrowserDriver>,
    /// Egress guard applied inside the tool.
    pub egress_guard: EgressGuard,
}

impl BrowserExtractTool {
    /// Create a `BrowserExtractTool` backed by `driver`.
    pub fn new(driver: impl BrowserDriver + 'static, egress_guard: EgressGuard) -> Self {
        Self {
            driver: std::sync::Arc::new(driver),
            egress_guard,
        }
    }

    /// Convenience constructor backed by a [`MockBrowserDriver`]. CI-safe.
    pub fn with_mock(driver: MockBrowserDriver) -> Self {
        Self::new(driver, EgressGuard::default())
    }
}

impl ToolDriver for BrowserExtractTool {
    fn id(&self) -> &'static str {
        "extract"
    }

    fn schema(&self) -> &'static str {
        r#"{
  "type": "object",
  "description": "Load a URL and return the text of every element matching a CSS selector.",
  "properties": {
    "url": {
      "type": "string",
      "description": "The absolute https:// URL to load"
    },
    "selector": {
      "type": "string",
      "description": "A CSS selector identifying the elements to extract"
    }
  },
  "required": ["url", "selector"]
}"#
    }

    fn invoke(&self, payload: &[u8]) -> Result<Vec<u8>, ToolInvocationError> {
        let req: ExtractRequest =
            serde_json::from_slice(payload).map_err(|_| ToolInvocationError::InvalidPayload)?;
        if req.url.trim().is_empty() || req.selector.trim().is_empty() {
            return Err(ToolInvocationError::InvalidPayload);
        }

        screen_url(&self.egress_guard, &req.url)?;

        let items = self
            .driver
            .extract(&req.url, &req.selector)
            .map_err(ToolInvocationError::ExecutionFailed)?;

        serde_json::to_vec(&items).map_err(|e| ToolInvocationError::ExecutionFailed(e.to_string()))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE_URL: &str = "https://example.com/page";

    fn sample_driver() -> MockBrowserDriver {
        MockBrowserDriver::new().with_page(
            EXAMPLE_URL,
            MockPage::new("Example Domain", "This domain is for use in examples.")
                .with_extraction("h1", vec!["Example Domain".to_string()])
                .with_extraction(
                    "a",
                    vec!["More information".to_string(), "Contact".to_string()],
                ),
        )
    }

    // ── MockBrowserDriver ─────────────────────────────────────────────────────

    #[test]
    fn mock_driver_navigate_returns_canned_page() {
        let driver = sample_driver();
        let page = driver.navigate(EXAMPLE_URL).unwrap();
        assert_eq!(page.url, EXAMPLE_URL);
        assert_eq!(page.title, "Example Domain");
        assert!(page.text.contains("examples"));
    }

    #[test]
    fn mock_driver_read_text_returns_text() {
        let driver = sample_driver();
        let text = driver.read_text(EXAMPLE_URL).unwrap();
        assert_eq!(text, "This domain is for use in examples.");
    }

    #[test]
    fn mock_driver_extract_returns_items() {
        let driver = sample_driver();
        let items = driver.extract(EXAMPLE_URL, "a").unwrap();
        assert_eq!(items, vec!["More information", "Contact"]);
    }

    #[test]
    fn mock_driver_extract_unknown_selector_returns_empty() {
        let driver = sample_driver();
        let items = driver.extract(EXAMPLE_URL, ".nonexistent").unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn mock_driver_navigate_unknown_url_errors() {
        let driver = sample_driver();
        let result = driver.navigate("https://unknown.example.org/");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no canned page"));
    }

    // ── BrowserNavigateTool ───────────────────────────────────────────────────

    #[test]
    fn navigate_tool_returns_page_state_json() {
        let tool = BrowserNavigateTool::with_mock(sample_driver());
        let payload = serde_json::json!({"url": EXAMPLE_URL})
            .to_string()
            .into_bytes();
        let output = tool.invoke(&payload).unwrap();
        let page: PageState = serde_json::from_slice(&output).unwrap();
        assert_eq!(page.title, "Example Domain");
        assert_eq!(page.url, EXAMPLE_URL);
    }

    #[test]
    fn navigate_tool_id_is_browser() {
        let tool = BrowserNavigateTool::with_mock(MockBrowserDriver::new());
        assert_eq!(tool.id(), "browser");
    }

    #[test]
    fn navigate_tool_rejects_empty_url() {
        let tool = BrowserNavigateTool::with_mock(sample_driver());
        let payload = serde_json::json!({"url": ""}).to_string().into_bytes();
        assert!(matches!(
            tool.invoke(&payload),
            Err(ToolInvocationError::InvalidPayload)
        ));
    }

    #[test]
    fn navigate_tool_rejects_invalid_json() {
        let tool = BrowserNavigateTool::with_mock(sample_driver());
        assert!(matches!(
            tool.invoke(b"not json"),
            Err(ToolInvocationError::InvalidPayload)
        ));
    }

    #[test]
    fn navigate_tool_rejects_missing_url_field() {
        let tool = BrowserNavigateTool::with_mock(sample_driver());
        let payload = serde_json::json!({"nope": "x"}).to_string().into_bytes();
        assert!(matches!(
            tool.invoke(&payload),
            Err(ToolInvocationError::InvalidPayload)
        ));
    }

    // ── Egress screening (SSRF / scheme) ──────────────────────────────────────

    /// A driver that records whether it was ever called, to prove egress
    /// screening happens BEFORE any fetch.
    struct SpyDriver {
        called: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl BrowserDriver for SpyDriver {
        fn navigate(&self, url: &str) -> Result<PageState, String> {
            self.called.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(PageState {
                url: url.to_string(),
                title: String::new(),
                text: String::new(),
            })
        }

        fn extract(&self, _url: &str, _selector: &str) -> Result<Vec<String>, String> {
            self.called.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(vec![])
        }
    }

    #[test]
    fn navigate_tool_blocks_private_ip_and_does_not_fetch() {
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tool = BrowserNavigateTool::new(
            SpyDriver {
                called: called.clone(),
            },
            EgressGuard::default(),
        );
        let payload = serde_json::json!({"url": "https://192.168.1.10/page"})
            .to_string()
            .into_bytes();
        let result = tool.invoke(&payload);
        assert!(result.is_err());
        match result {
            Err(ToolInvocationError::ExecutionFailed(msg)) => {
                assert!(msg.contains("egress-blocked"), "got: {msg}");
            }
            other => panic!("expected ExecutionFailed egress-blocked, got {other:?}"),
        }
        assert!(
            !called.load(std::sync::atomic::Ordering::SeqCst),
            "driver must NOT be invoked when egress is blocked"
        );
    }

    #[test]
    fn navigate_tool_blocks_http_scheme_and_does_not_fetch() {
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tool = BrowserNavigateTool::new(
            SpyDriver {
                called: called.clone(),
            },
            EgressGuard::default(),
        );
        let payload = serde_json::json!({"url": "http://example.com/page"})
            .to_string()
            .into_bytes();
        let result = tool.invoke(&payload);
        assert!(matches!(
            result,
            Err(ToolInvocationError::ExecutionFailed(ref m)) if m.contains("egress-blocked")
        ));
        assert!(
            !called.load(std::sync::atomic::Ordering::SeqCst),
            "driver must NOT be invoked for a forbidden scheme"
        );
    }

    #[test]
    fn extract_tool_blocks_loopback_and_does_not_fetch() {
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tool = BrowserExtractTool::new(
            SpyDriver {
                called: called.clone(),
            },
            EgressGuard::default(),
        );
        let payload = serde_json::json!({"url": "https://127.0.0.1/admin", "selector": "h1"})
            .to_string()
            .into_bytes();
        let result = tool.invoke(&payload);
        assert!(result.is_err());
        assert!(
            !called.load(std::sync::atomic::Ordering::SeqCst),
            "driver must NOT be invoked when egress is blocked"
        );
    }

    // ── BrowserReadTextTool ───────────────────────────────────────────────────

    #[test]
    fn read_text_tool_returns_text_json() {
        let tool = BrowserReadTextTool::with_mock(sample_driver());
        let payload = serde_json::json!({"url": EXAMPLE_URL})
            .to_string()
            .into_bytes();
        let output = tool.invoke(&payload).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(
            parsed.get("text").and_then(|v| v.as_str()),
            Some("This domain is for use in examples.")
        );
    }

    #[test]
    fn read_text_tool_id_is_browse() {
        let tool = BrowserReadTextTool::with_mock(MockBrowserDriver::new());
        assert_eq!(tool.id(), "browse");
    }

    // ── BrowserExtractTool ────────────────────────────────────────────────────

    #[test]
    fn extract_tool_returns_items_json() {
        let tool = BrowserExtractTool::with_mock(sample_driver());
        let payload = serde_json::json!({"url": EXAMPLE_URL, "selector": "a"})
            .to_string()
            .into_bytes();
        let output = tool.invoke(&payload).unwrap();
        let items: Vec<String> = serde_json::from_slice(&output).unwrap();
        assert_eq!(items, vec!["More information", "Contact"]);
    }

    #[test]
    fn extract_tool_id_is_extract() {
        let tool = BrowserExtractTool::with_mock(MockBrowserDriver::new());
        assert_eq!(tool.id(), "extract");
    }

    #[test]
    fn extract_tool_rejects_empty_selector() {
        let tool = BrowserExtractTool::with_mock(sample_driver());
        let payload = serde_json::json!({"url": EXAMPLE_URL, "selector": ""})
            .to_string()
            .into_bytes();
        assert!(matches!(
            tool.invoke(&payload),
            Err(ToolInvocationError::InvalidPayload)
        ));
    }

    #[test]
    fn extract_tool_rejects_missing_selector() {
        let tool = BrowserExtractTool::with_mock(sample_driver());
        let payload = serde_json::json!({"url": EXAMPLE_URL})
            .to_string()
            .into_bytes();
        assert!(matches!(
            tool.invoke(&payload),
            Err(ToolInvocationError::InvalidPayload)
        ));
    }

    // ── Schema sanity ─────────────────────────────────────────────────────────

    #[test]
    fn schemas_are_valid_json_with_required_url() {
        for schema in [
            BrowserNavigateTool::with_mock(MockBrowserDriver::new()).schema(),
            BrowserReadTextTool::with_mock(MockBrowserDriver::new()).schema(),
            BrowserExtractTool::with_mock(MockBrowserDriver::new()).schema(),
        ] {
            let v: serde_json::Value =
                serde_json::from_str(schema).expect("schema must be valid JSON");
            let required = v
                .get("required")
                .and_then(|r| r.as_array())
                .expect("schema must declare required fields");
            assert!(
                required.iter().any(|f| f.as_str() == Some("url")),
                "every browser tool requires a url"
            );
        }
    }

    #[test]
    fn extract_schema_requires_selector() {
        let schema = BrowserExtractTool::with_mock(MockBrowserDriver::new()).schema();
        let v: serde_json::Value = serde_json::from_str(schema).unwrap();
        let required: Vec<&str> = v["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|f| f.as_str())
            .collect();
        assert!(required.contains(&"selector"));
    }

    // ── End-to-end: navigate → extract via the mock (S7.2 exit criterion 1) ───

    #[test]
    fn navigate_then_extract_end_to_end() {
        let driver = sample_driver();
        // Navigate succeeds.
        let nav_tool = BrowserNavigateTool::with_mock(
            MockBrowserDriver::new().with_page(
                EXAMPLE_URL,
                MockPage::new("Example Domain", "body")
                    .with_extraction("h1", vec!["Example Domain".to_string()]),
            ),
        );
        let nav_out = nav_tool
            .invoke(
                &serde_json::json!({"url": EXAMPLE_URL})
                    .to_string()
                    .into_bytes(),
            )
            .unwrap();
        let page: PageState = serde_json::from_slice(&nav_out).unwrap();
        assert_eq!(page.title, "Example Domain");

        // Extract from the same canned page.
        let extract_tool = BrowserExtractTool::with_mock(driver);
        let ex_out = extract_tool
            .invoke(
                &serde_json::json!({"url": EXAMPLE_URL, "selector": "h1"})
                    .to_string()
                    .into_bytes(),
            )
            .unwrap();
        let items: Vec<String> = serde_json::from_slice(&ex_out).unwrap();
        assert_eq!(items, vec!["Example Domain"]);
    }
}
