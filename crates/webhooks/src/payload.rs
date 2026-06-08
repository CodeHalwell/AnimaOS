//! Webhook payload envelope and HMAC-SHA256 signature helpers.
//!
//! Signatures are computed over the exact JSON bytes that are transmitted and
//! delivered exclusively via the `X-Anima-Signature` HTTP header — matching the
//! industry-standard pattern used by GitHub, Stripe, and others.  Receivers can
//! simply hash the raw request body; no JSON parsing is required.

use serde::{Deserialize, Serialize};

/// The outbound webhook payload envelope.
///
/// Serialises to compact JSON for HTTP delivery.  The signature is **not**
/// embedded here; it is computed over `to_json()` and sent as the
/// `X-Anima-Signature: sha256=<hex>` HTTP header.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebhookPayload {
    /// Unique delivery identifier.
    pub delivery_id: String,
    /// Agent that emitted the event.
    pub agent_id: String,
    /// Event kind string (e.g. `"task_completed"`, `"sleep_entered"`).
    pub event_kind: String,
    /// Nanosecond-epoch timestamp when the event was emitted.
    pub timestamp_ns: u64,
    /// Event-specific data as a freeform JSON object.
    pub data: serde_json::Value,
}

impl WebhookPayload {
    /// Construct a payload.
    pub fn new(
        delivery_id: impl Into<String>,
        agent_id: impl Into<String>,
        event_kind: impl Into<String>,
        timestamp_ns: u64,
        data: serde_json::Value,
    ) -> Self {
        WebhookPayload {
            delivery_id: delivery_id.into(),
            agent_id: agent_id.into(),
            event_kind: event_kind.into(),
            timestamp_ns,
            data,
        }
    }

    /// Serialise to a compact JSON string for HTTP delivery.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Compute an HMAC-SHA256 signature over `body_json` using `secret`.
    ///
    /// Returns a `"sha256=<64-lowercase-hex>"` string suitable for the
    /// `X-Anima-Signature` HTTP header.  Call this on the result of
    /// `to_json()` *after* serialisation so the signature covers the exact
    /// bytes the receiver will see.
    pub fn sign(body_json: &str, secret: &str) -> String {
        let mac = hmac_sha256(secret.as_bytes(), body_json.as_bytes());
        format!("sha256={}", to_hex(&mac))
    }

    /// Verify an `X-Anima-Signature` header value.
    ///
    /// Returns `true` when `signature` is `"sha256=<hex>"` and the HMAC of
    /// `body_json` under `secret` matches.
    pub fn verify_signature(body_json: &str, secret: &str, signature: &str) -> bool {
        let expected = Self::sign(body_json, secret);
        expected == signature
    }
}

// ── Real HMAC-SHA256 (mirrors the implementation in crates/constitution) ───────

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};

    const BLOCK: usize = 64;

    let mut block_key = [0u8; BLOCK];
    if key.len() > BLOCK {
        let d = Sha256::digest(key);
        block_key[..32].copy_from_slice(&d);
    } else {
        block_key[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= block_key[i];
        opad[i] ^= block_key[i];
    }

    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(data);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    outer.finalize().into()
}

fn to_hex(bytes: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(64);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_payload(kind: &str) -> WebhookPayload {
        WebhookPayload::new(
            "dlv-0001",
            "agent-x",
            kind,
            1_700_000_000_000_000_000u64,
            serde_json::json!({ "task_id": 42 }),
        )
    }

    #[test]
    fn sign_has_sha256_prefix() {
        let body = make_payload("task_completed").to_json();
        let sig = WebhookPayload::sign(&body, "secret");
        assert!(sig.starts_with("sha256="));
        assert_eq!(sig.len(), 7 + 64); // "sha256=" + 64 hex chars
    }

    #[test]
    fn sign_and_verify_round_trip() {
        let body = make_payload("task_completed").to_json();
        let sig = WebhookPayload::sign(&body, "my-secret");
        assert!(WebhookPayload::verify_signature(&body, "my-secret", &sig));
    }

    #[test]
    fn verify_fails_with_wrong_secret() {
        let body = make_payload("task_completed").to_json();
        let sig = WebhookPayload::sign(&body, "correct");
        assert!(!WebhookPayload::verify_signature(&body, "wrong", &sig));
    }

    #[test]
    fn verify_fails_on_tampered_body() {
        let body = make_payload("task_completed").to_json();
        let sig = WebhookPayload::sign(&body, "secret");
        let tampered = body.replace("42", "999");
        assert!(!WebhookPayload::verify_signature(&tampered, "secret", &sig));
    }

    #[test]
    fn verify_fails_for_malformed_signature() {
        let body = make_payload("alert_fired").to_json();
        assert!(!WebhookPayload::verify_signature(
            &body,
            "secret",
            "not-a-sig"
        ));
        assert!(!WebhookPayload::verify_signature(
            &body,
            "secret",
            "sha256=badhex"
        ));
    }

    #[test]
    fn signing_is_deterministic_for_same_inputs() {
        let body = make_payload("alert_fired").to_json();
        let sig1 = WebhookPayload::sign(&body, "key");
        let sig2 = WebhookPayload::sign(&body, "key");
        assert_eq!(sig1, sig2);
    }

    #[test]
    fn payload_round_trips_through_json() {
        let p = make_payload("sleep_entered");
        let json = p.to_json();
        let restored: WebhookPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(p, restored);
    }

    #[test]
    fn payload_json_contains_no_signature_field() {
        let p = make_payload("wake_entered");
        assert!(!p.to_json().contains("signature"));
    }
}
