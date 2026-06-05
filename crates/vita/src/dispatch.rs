//! Egress-aware tool dispatch — E7 S7.0.3.
//!
//! Extends the vita tool-dispatch loop so every call carrying an outbound
//! network effect is screened by the [`EgressGuard`] before `invoke` is
//! called.  Audit entries for each screening decision are buffered and flushed
//! to the main [`AuditLog`] by the call site after the cortex invocation
//! completes.
//!
//! # Architecture
//!
//! ```text
//! cortex ──ToolCall──► vita dispatch
//!                       │
//!                       ▼ EgressAwareDispatcher::dispatch
//!                       │  ├─ not a network tool → pass through unchanged
//!                       │  ├─ URL in args → EgressGuard::check_url
//!                       │  │    ├─ Allow → buffer EgressRequested entry
//!                       │  │    └─ Deny  → buffer EgressBlocked entry → Err
//!                       │  └─ no URL in args → buffer generic EgressRequested
//!                       │
//!                       ▼ inner ToolDispatcher::dispatch (ToolRegistry)
//!                       │
//!   ◄──ToolResponse─────┘
//! ```
//!
//! The `audit_buffer` is flushed to the main `AuditLog` by calling
//! [`EgressAwareDispatcher::flush_audit`] after each cortex invocation.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use actuators::egress::{EgressGuard, EgressVerdict};

use crate::audit::{AuditEntry, AuditLog};
use crate::cortex_bridge::ToolDispatcher;

/// Tool IDs that issue outbound network requests and require egress screening.
const NETWORK_TOOL_IDS: &[&str] = &["web-search", "browser", "navigate", "browse", "extract"];

/// Wraps any [`ToolDispatcher`] with per-call egress screening (E7 S7.0.3).
///
/// # Example
///
/// ```rust,ignore
/// use vita::dispatch::EgressAwareDispatcher;
/// use vita::cortex_bridge::FnDispatcher;
/// use actuators::EgressGuard;
///
/// let registry = /* your ToolRegistry */;
/// let inner = FnDispatcher(move |name, args| registry.dispatch(...));
/// let dispatcher = EgressAwareDispatcher::new(inner, EgressGuard::default());
///
/// // After the cortex invocation:
/// dispatcher.flush_audit(&mut lifecycle.audit);
/// ```
pub struct EgressAwareDispatcher<D: ToolDispatcher> {
    /// Inner dispatcher (e.g. a `FnDispatcher` wrapping a `ToolRegistry`).
    pub inner: D,
    /// Egress policy applied to outbound tool calls.
    pub egress_guard: EgressGuard,
    /// Buffer of audit entries accumulated during dispatch calls.
    ///
    /// Shared behind `Arc<Mutex<…>>` so it survives the `&self` borrow on
    /// `dispatch()` while still accumulating across multiple calls within a
    /// single cortex invocation.
    pub audit_buffer: Arc<Mutex<Vec<AuditEntry>>>,
    /// Set of tool IDs that are known to make outbound network calls.
    ///
    /// Defaults to [`NETWORK_TOOL_IDS`].  Callers may extend or replace this
    /// set to accommodate additional tool drivers.
    pub network_tool_ids: HashSet<String>,
}

impl<D: ToolDispatcher> EgressAwareDispatcher<D> {
    /// Create a new dispatcher wrapping `inner` with the given `egress_guard`.
    pub fn new(inner: D, egress_guard: EgressGuard) -> Self {
        Self {
            inner,
            egress_guard,
            audit_buffer: Arc::new(Mutex::new(Vec::new())),
            network_tool_ids: NETWORK_TOOL_IDS.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Flush accumulated audit entries into `log` and clear the buffer.
    ///
    /// Call this after each cortex invocation completes to move the egress
    /// entries into the main lifecycle audit log.
    pub fn flush_audit(&self, log: &mut AuditLog) {
        if let Ok(mut buf) = self.audit_buffer.lock() {
            for entry in buf.drain(..) {
                log.push(entry);
            }
        }
    }
}

impl<D: ToolDispatcher> ToolDispatcher for EgressAwareDispatcher<D> {
    fn dispatch(&self, tool_name: &str, args: &str) -> Result<String, String> {
        // Only network-capable tools require egress screening.
        if self.network_tool_ids.contains(tool_name) {
            let url_from_args = extract_url_from_args(args);

            let mut buf = self.audit_buffer.lock().unwrap_or_else(|e| e.into_inner());

            if let Some(url) = url_from_args {
                // Screen the URL from the args (relevant for browser-style tools).
                match self.egress_guard.check_url(&url) {
                    EgressVerdict::Allow => {
                        buf.push(AuditEntry::EgressRequested {
                            tool_id: tool_name.to_string(),
                            url: redact_url(&url),
                        });
                    }
                    EgressVerdict::Deny(reason) => {
                        buf.push(AuditEntry::EgressBlocked {
                            tool_id: tool_name.to_string(),
                            url: redact_url(&url),
                            reason: reason.description(),
                        });
                        return Err(format!("egress-blocked: {}", reason.description()));
                    }
                }
            } else {
                // No URL in args (e.g. `web-search` — the URL is embedded in the
                // tool driver's `SearchProvider`).  Emit a generic EgressRequested
                // entry so the audit trail shows the network-capable tool was invoked.
                // The tool driver's own [`EgressGuard`] handles the actual check.
                buf.push(AuditEntry::EgressRequested {
                    tool_id: tool_name.to_string(),
                    url: "(url embedded in tool driver)".to_string(),
                });
            }
        }

        self.inner.dispatch(tool_name, args)
    }
}

// ── URL helpers ───────────────────────────────────────────────────────────────

/// Extract a URL value from `args` JSON by inspecting common field names.
fn extract_url_from_args(args: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(args).ok()?;
    for key in &["url", "base_url", "href", "link", "navigate_to"] {
        if let Some(url) = v.get(key).and_then(|u| u.as_str()) {
            return Some(url.to_string());
        }
    }
    None
}

/// Redact sensitive query-string parameters from a URL.
///
/// Replaces values of parameters whose names contain `key`, `token`, `secret`,
/// `auth`, `password`, or `api` with `[REDACTED]`, preventing API credentials
/// from appearing in the audit log.
///
/// This satisfies E7 S7.0.4: "no secrets ever appear in the audit log."
pub fn redact_url(url: &str) -> String {
    const SENSITIVE: &[&str] = &["key", "token", "secret", "auth", "password", "api"];
    if !url.contains('?') {
        return url.to_string();
    }
    let (base, query) = url.split_once('?').unwrap();
    let redacted: Vec<String> = query
        .split('&')
        .map(|pair| {
            if let Some((k, _v)) = pair.split_once('=') {
                let k_lower = k.to_lowercase();
                if SENSITIVE.iter().any(|s| k_lower.contains(s)) {
                    return format!("{k}=[REDACTED]");
                }
            }
            pair.to_string()
        })
        .collect();
    format!("{}?{}", base, redacted.join("&"))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cortex_bridge::FnDispatcher;

    fn passthrough_dispatcher(
    ) -> FnDispatcher<impl Fn(&str, &str) -> Result<String, String> + Send + Sync> {
        FnDispatcher(|name: &str, _args: &str| Ok(format!("result-of-{name}")))
    }

    // ── Non-network tool passes through unchanged ─────────────────────────────

    #[test]
    fn non_network_tool_passes_through_with_no_egress_entry() {
        let dispatcher =
            EgressAwareDispatcher::new(passthrough_dispatcher(), EgressGuard::default());
        let result = dispatcher.dispatch("clock", "{}").unwrap();
        assert_eq!(result, "result-of-clock");
        assert!(dispatcher.audit_buffer.lock().unwrap().is_empty());
    }

    // ── web-search: no URL in args → generic EgressRequested ─────────────────

    #[test]
    fn web_search_without_url_in_args_emits_egress_requested_entry() {
        let dispatcher =
            EgressAwareDispatcher::new(passthrough_dispatcher(), EgressGuard::default());
        let _ = dispatcher.dispatch("web-search", r#"{"query":"rust news"}"#);
        let buf = dispatcher.audit_buffer.lock().unwrap();
        assert_eq!(buf.len(), 1);
        assert!(matches!(
            &buf[0],
            AuditEntry::EgressRequested { tool_id, .. } if tool_id == "web-search"
        ));
    }

    // ── browser: private URL in args → EgressBlocked ─────────────────────────

    #[test]
    fn browser_with_private_ip_url_is_blocked_and_audited() {
        let dispatcher =
            EgressAwareDispatcher::new(passthrough_dispatcher(), EgressGuard::default());
        let result = dispatcher.dispatch("browser", r#"{"url":"https://192.168.1.1/admin"}"#);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("egress-blocked"));
        let buf = dispatcher.audit_buffer.lock().unwrap();
        assert_eq!(buf.len(), 1);
        assert!(matches!(
            &buf[0],
            AuditEntry::EgressBlocked { tool_id, .. } if tool_id == "browser"
        ));
    }

    // ── browser: public URL → EgressRequested ────────────────────────────────

    #[test]
    fn browser_with_public_url_emits_egress_requested_and_proceeds() {
        let dispatcher =
            EgressAwareDispatcher::new(passthrough_dispatcher(), EgressGuard::default());
        let result = dispatcher.dispatch("browser", r#"{"url":"https://example.com/page"}"#);
        assert!(result.is_ok());
        let buf = dispatcher.audit_buffer.lock().unwrap();
        assert_eq!(buf.len(), 1);
        assert!(matches!(
            &buf[0],
            AuditEntry::EgressRequested { tool_id, url }
                if tool_id == "browser" && url == "https://example.com/page"
        ));
    }

    // ── flush_audit empties the buffer ────────────────────────────────────────

    #[test]
    fn flush_audit_moves_entries_to_log_and_clears_buffer() {
        let dispatcher =
            EgressAwareDispatcher::new(passthrough_dispatcher(), EgressGuard::default());
        let _ = dispatcher.dispatch("web-search", r#"{"query":"test"}"#);
        assert_eq!(dispatcher.audit_buffer.lock().unwrap().len(), 1);

        let mut log = AuditLog::new();
        dispatcher.flush_audit(&mut log);

        assert!(
            dispatcher.audit_buffer.lock().unwrap().is_empty(),
            "buffer should be empty after flush"
        );
        assert_eq!(log.len(), 1, "log should have the flushed entry");
    }

    // ── secret redaction ──────────────────────────────────────────────────────

    #[test]
    fn redact_url_masks_api_key_in_query_string() {
        let url = "https://example.com/search?q=rust&api_key=sk-secret123&format=json";
        let redacted = redact_url(url);
        assert!(redacted.contains("q=rust"));
        assert!(redacted.contains("[REDACTED]"));
        assert!(!redacted.contains("sk-secret123"));
    }

    #[test]
    fn redact_url_preserves_non_sensitive_params() {
        let url = "https://example.com/search?q=rust&format=json&lang=en";
        assert_eq!(redact_url(url), url);
    }

    #[test]
    fn redact_url_handles_url_without_query_string() {
        let url = "https://example.com/page";
        assert_eq!(redact_url(url), url);
    }

    // ── Secret never appears in audit log ─────────────────────────────────────

    #[test]
    fn secret_in_url_args_never_appears_in_audit_log() {
        let dispatcher =
            EgressAwareDispatcher::new(passthrough_dispatcher(), EgressGuard::default());
        let _ = dispatcher.dispatch(
            "browser",
            r#"{"url":"https://example.com/page?api_key=topsecret&q=test"}"#,
        );
        let buf = dispatcher.audit_buffer.lock().unwrap();
        for entry in buf.iter() {
            let serialised = serde_json::to_string(entry).unwrap_or_default();
            assert!(
                !serialised.contains("topsecret"),
                "raw secret must not appear in audit entry: {serialised}"
            );
        }
    }
}
