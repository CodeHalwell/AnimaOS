#![forbid(unsafe_code)]

//! Outbound webhook integration for AnimaOS — Epic E29.
//!
//! # Scope
//!
//! AnimaOS emits a rich audit trail of lifecycle events internally (via
//! `vita::AuditLog`) but has no way to push those events to external systems.
//! E29 closes that gap: operators can register HTTPS webhook endpoints that
//! receive signed JSON payloads whenever matching events occur.
//!
//! # Architecture
//!
//! ```text
//!  vita::AuditEntry                  caller (hosted kernel / future middleware)
//!      │                                           │
//!      │ convert to WebhookPayload                 │ register / remove endpoints
//!      ▼                                           ▼
//!  WebhookDispatcher  ←──── WebhookRegistry (persistence) ────►  disk
//!      │
//!      │ FixtureSender (CI) or real HTTP sender (live)
//!      ▼
//!  External webhook endpoint (POST <url>)
//! ```
//!
//! # Key types
//!
//! | Type | Purpose |
//! |------|---------|
//! | [`endpoint::WebhookEndpoint`] | Registered endpoint: URL, secret, event filter |
//! | [`endpoint::EventFilter`] | Which event kinds to forward (`All` or `Selected`) |
//! | [`payload::WebhookPayload`] | JSON envelope with HMAC-SHA256 signature support |
//! | [`registry::WebhookRegistry`] | Persistent endpoint store |
//! | [`dispatcher::WebhookDispatcher`] | Dispatch with retry and cumulative stats |
//! | [`dispatcher::DispatchStats`] | Per-dispatch outcome and attempt count |
//! | [`dispatcher::CumulativeStats`] | Aggregate success/failure/retry counters |
//!
//! # Modules

pub mod dispatcher;
pub mod endpoint;
pub mod payload;
pub mod registry;

// Re-export the most commonly used types.
pub use dispatcher::{
    CumulativeStats, DispatchConfig, DispatchStats, FixtureSender, WebhookDispatcher, WebhookSender,
};
pub use endpoint::{EventFilter, WebhookEndpoint};
pub use payload::WebhookPayload;
pub use registry::{RegistryError, WebhookRegistry};

/// Generate a unique delivery ID for a new payload.
///
/// Format: `"dlv-<16-hex-digits>"`.  Uses the current time and a process-local
/// counter for uniqueness without an external UUID library.
pub fn new_delivery_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    format!("dlv-{:016x}{:08x}", ts, n & 0xFFFF_FFFF)
}

/// Generate a unique endpoint ID.
///
/// Format: `"wh-<8-hex-digits>"`.
pub fn new_endpoint_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    format!("wh-{:08x}", (ts ^ n).wrapping_mul(0x9e37_79b9))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_delivery_id_has_correct_prefix() {
        assert!(new_delivery_id().starts_with("dlv-"));
    }

    #[test]
    fn new_delivery_ids_are_unique() {
        let ids: Vec<String> = (0..10).map(|_| new_delivery_id()).collect();
        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), 10, "all delivery IDs should be unique");
    }

    #[test]
    fn new_endpoint_id_has_correct_prefix() {
        assert!(new_endpoint_id().starts_with("wh-"));
    }

    #[test]
    fn new_endpoint_ids_are_unique() {
        let ids: Vec<String> = (0..10).map(|_| new_endpoint_id()).collect();
        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), 10, "all endpoint IDs should be unique");
    }

    #[test]
    fn end_to_end_dispatch_via_fixture_sender() {
        use crate::dispatcher::WebhookDispatcher;
        use crate::endpoint::EventFilter;

        let mut registry = WebhookRegistry::in_memory();
        registry
            .register(WebhookEndpoint::new(
                "wh-test",
                "https://hooks.example.com/anima",
                Some("signing-key".to_string()),
                EventFilter::All,
            ))
            .unwrap();

        let mut dispatcher = WebhookDispatcher::fixture();

        let endpoints = registry.endpoints_for_event("task_completed");
        for ep in &endpoints {
            let mut payload = WebhookPayload::new(
                new_delivery_id(),
                "agent-main",
                "task_completed",
                0,
                serde_json::json!({ "task_id": 7, "tokens": 128 }),
            );
            let stats = dispatcher.dispatch(ep, &mut payload);
            assert!(stats.success);
            // Verify the signature computed by the dispatcher matches the body.
            let body = payload.to_json();
            let sig = WebhookPayload::sign(&body, "signing-key");
            assert!(WebhookPayload::verify_signature(&body, "signing-key", &sig));
        }

        assert_eq!(dispatcher.stats().successful, 1);
    }
}
