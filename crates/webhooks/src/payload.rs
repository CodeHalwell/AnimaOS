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
    ///
    /// The comparison runs in constant time over the raw HMAC bytes to avoid
    /// leaking how many leading bytes of a forged signature were correct (a
    /// timing oracle that would otherwise let an attacker recover a valid
    /// signature byte-by-byte).
    pub fn verify_signature(body_json: &str, secret: &str, signature: &str) -> bool {
        let expected = hmac_sha256(secret.as_bytes(), body_json.as_bytes());
        // Parse the candidate `"sha256=<hex>"` into raw bytes; a malformed header
        // (wrong prefix, bad hex, wrong length) simply fails to parse and is
        // rejected without any content comparison.
        let candidate = match parse_sha256_hex(signature) {
            Some(bytes) => bytes,
            None => return false,
        };
        constant_time_eq(&expected, &candidate)
    }
}

/// Parse a `"sha256=<64-hex>"` header value into its 32 raw HMAC bytes.
///
/// Returns `None` for any malformed input (missing prefix, wrong length, or
/// non-hex characters).
fn parse_sha256_hex(signature: &str) -> Option<[u8; 32]> {
    let hex = signature.strip_prefix("sha256=")?;
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    let bytes = hex.as_bytes();
    for (i, slot) in out.iter_mut().enumerate() {
        let hi = hex_val(bytes[i * 2])?;
        let lo = hex_val(bytes[i * 2 + 1])?;
        *slot = (hi << 4) | lo;
    }
    Some(out)
}

/// Decode a single ASCII hex digit (0-9, a-f, A-F) to its nibble value.
fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Constant-time byte-slice equality.
///
/// Folds an XOR accumulator over both slices so the running time depends only on
/// the input length, never on where (or whether) the first differing byte
/// occurs.  Differing lengths return `false` without a short-circuit on content.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
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

    // ── Constant-time signature verification (timing-oracle hardening) ─────────

    #[test]
    fn verify_accepts_correct_and_rejects_wrong_signature() {
        let body = make_payload("task_completed").to_json();
        let sig = WebhookPayload::sign(&body, "shared-secret");
        // Correct signature is accepted.
        assert!(WebhookPayload::verify_signature(
            &body,
            "shared-secret",
            &sig
        ));

        // A signature that differs only in its last byte must be rejected. (This
        // is the case a short-circuiting compare would have leaked timing on.)
        let mut wrong = sig.clone().into_bytes();
        let last = wrong.last_mut().unwrap();
        *last = if *last == b'0' { b'1' } else { b'0' };
        let wrong = String::from_utf8(wrong).unwrap();
        assert!(!WebhookPayload::verify_signature(
            &body,
            "shared-secret",
            &wrong
        ));
    }

    #[test]
    fn verify_accepts_uppercase_hex_signature() {
        let body = make_payload("task_completed").to_json();
        let sig = WebhookPayload::sign(&body, "key").to_uppercase();
        // The hex payload is case-insensitive; only the "sha256=" prefix differs.
        let sig = sig.replacen("SHA256=", "sha256=", 1);
        assert!(WebhookPayload::verify_signature(&body, "key", &sig));
    }

    #[test]
    fn constant_time_eq_matches_naive_equality() {
        assert!(constant_time_eq(b"abcd", b"abcd"));
        assert!(!constant_time_eq(b"abcd", b"abce"));
        assert!(!constant_time_eq(b"abcd", b"abc")); // length mismatch
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn parse_sha256_hex_rejects_malformed() {
        assert!(parse_sha256_hex("no-prefix").is_none());
        assert!(parse_sha256_hex("sha256=short").is_none());
        assert!(parse_sha256_hex(&format!("sha256={}", "z".repeat(64))).is_none());
        assert!(parse_sha256_hex(&format!("sha256={}", "a".repeat(64))).is_some());
    }
}
