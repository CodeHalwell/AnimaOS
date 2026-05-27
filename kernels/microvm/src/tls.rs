//! E4.4 — TLS 1.3 implementation for the AnimaOS microVM kernel.
//!
//! Provides two complementary demonstrations:
//!
//! 1. **`run_tls_loopback_test()`** — complete TLS 1.3 handshake and
//!    application-data exchange using two in-process `Vec<u8>` buffers as
//!    the transport.  Exercises the full RFC 8446 state machine:
//!    CH→SH→EE→Cert→CV→SFin→CFin→AppData.
//!
//! 2. **`run_tls_over_smoltcp_test()`** — TLS 1.3 ClientHello record sent
//!    through a real smoltcp TCP loopback socket and verified on the server
//!    side.  Confirms that TLS records transit the smoltcp TCP stack intact.
//!
//! Cipher suite: `TLS_AES_128_GCM_SHA256` (0x1301).
//! Key exchange:  P-256 ECDHE (RFC 8446).
//! Certificate:   self-signed P-256, generated at build time by `build.rs`
//!                via `rcgen` and embedded as DER bytes.
//!
//! # Key schedule (RFC 8446 §7.1)
//!
//! ```text
//! EarlySecret   = HKDF-Extract(0, 0)
//! derived       = HKDF-Expand-Label(EarlySecret, "derived", SHA256(""), 32)
//! HandshakeSecret = HKDF-Extract(derived, ECDHE_shared_secret)
//! c_hs_traffic  = HKDF-Expand-Label(HS, "c hs traffic", transcript(CH…SH), 32)
//! s_hs_traffic  = HKDF-Expand-Label(HS, "s hs traffic", transcript(CH…SH), 32)
//! key           = HKDF-Expand-Label(traffic_secret, "key", "", 16)
//! iv            = HKDF-Expand-Label(traffic_secret, "iv",  "", 12)
//! finished_key  = HKDF-Expand-Label(traffic_secret, "finished", "", 32)
//! Finished MAC  = HMAC-SHA256(finished_key, transcript_hash)
//! ```
//!
//! # TLS record format
//!
//! Outer (plaintext envelope):
//!   `[content_type u8, 0x03, 0x03, length u16_be, payload…]`
//!
//! Inner (for encrypted records, "TLSInnerPlaintext"):
//!   `[real_data…, real_content_type u8]`
//! The payload stored in the outer record is the AEAD ciphertext of the inner
//! plaintext, with the outer header used as the additional data (AAD).
//! Nonce = iv XOR (seq_num as 12-byte big-endian).

extern crate alloc;
use alloc::vec::Vec;

use aes_gcm::aead::KeyInit;
use aes_gcm::{Aes128Gcm, Nonce};
use hkdf::Hkdf;
use hmac::Mac;
use p256::ecdh::EphemeralSecret;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::pkcs8::DecodePrivateKey;
use p256::PublicKey;
use rand_core::RngCore;
use sha2::{Digest, Sha256};
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{Loopback, Medium};
use smoltcp::socket::tcp;
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, IpAddress, IpCidr};

// Re-export for clarity inside this module.
type HkdfSha256 = Hkdf<Sha256>;
type HmacSha256 = hmac::Hmac<Sha256>;

// ---------------------------------------------------------------------------
// RDRAND-backed cryptographic RNG
// ---------------------------------------------------------------------------

/// Thin wrapper around the x86 RDRAND instruction, providing a `RngCore +
/// CryptoRng` implementation for use in bare-metal UEFI kernels.
///
/// # Safety
///
/// `RdRand::new()` verifies RDRAND availability via CPUID (leaf 0x01, ECX
/// bit 30) before any instruction is executed.  Constructing `RdRand`
/// directly (without `new()`) on a CPU/VM that does not set this bit will
/// trigger an invalid-opcode exception (#UD).  Always use `RdRand::new()`.
struct RdRand;

impl RdRand {
    /// Returns `true` if the CPU advertises RDRAND support.
    ///
    /// Reads CPUID leaf 0x01 and tests ECX bit 30 (the RDRAND feature flag).
    /// This check must be performed before calling any RDRAND instruction to
    /// avoid a #UD fault on CPUs or hypervisors that do not support it.
    fn is_available() -> bool {
        let ecx: u32;
        // SAFETY: CPUID is always safe to execute on x86_64.
        //
        // `rbx` is clobbered by CPUID but is reserved by LLVM (used as the
        // base pointer in PIC/PIE code).  Using push/pop to save it is
        // dangerous: on System V x86_64 ABI (Linux / macOS), the compiler
        // may store values in the 128-byte red zone *below* %rsp without
        // adjusting the stack pointer.  A `push` would overwrite that area.
        // Instead we use a temporary register allocated by the compiler to
        // save/restore %rbx.  The `nostack` option asserts we do not modify
        // the stack pointer, which is true when using mov-based save/restore.
        unsafe {
            core::arch::asm!(
                "mov {tmp}, rbx",
                "cpuid",
                "mov rbx, {tmp}",
                inout("eax") 0x01_u32 => _,
                out("ecx") ecx,
                out("edx") _,
                tmp = out(reg) _,
                options(nomem, nostack),
            );
        }
        (ecx >> 30) & 1 != 0
    }
}

impl rand_core::RngCore for RdRand {
    fn next_u32(&mut self) -> u32 {
        // Intel's RDRAND guidance recommends retrying up to 10 times.
        // A persistent failure after 10 retries indicates a hardware fault
        // or a hypervisor that does not implement RDRAND; panic rather than
        // spinning indefinitely, which would hang the bare-metal kernel.
        for _ in 0..10 {
            let mut val: u32;
            let ok: u8;
            // SAFETY: RDRAND is an x86_64 user-mode instruction; always safe
            // to execute.  The `setc` sets `ok` to 1 if the random value is
            // valid and 0 if the hardware entropy pool was momentarily
            // exhausted (rare; we retry as per Intel's guidance).
            unsafe {
                core::arch::asm!(
                    "rdrand {val:e}",
                    "setc {ok}",
                    val = out(reg) val,
                    ok = out(reg_byte) ok,
                );
            }
            if ok != 0 {
                return val;
            }
        }
        panic!(
            "RDRAND failed after 10 retries — hardware entropy source exhausted or hardware fault"
        );
    }

    fn next_u64(&mut self) -> u64 {
        ((self.next_u32() as u64) << 32) | (self.next_u32() as u64)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for chunk in dest.chunks_mut(4) {
            let v = self.next_u32().to_le_bytes();
            chunk.copy_from_slice(&v[..chunk.len()]);
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

impl rand_core::CryptoRng for RdRand {}

// ---------------------------------------------------------------------------
// Build-time embedded cert / key (generated by build.rs via rcgen).
// ---------------------------------------------------------------------------

/// DER-encoded self-signed P-256 certificate for "localhost".
const CERT_DER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tls_cert.der"));

/// DER-encoded PKCS#8 private key corresponding to the certificate.
const KEY_DER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tls_key.der"));

// ---------------------------------------------------------------------------
// TLS 1.3 constants
// ---------------------------------------------------------------------------

/// TLS content type: handshake (22).
const CT_HANDSHAKE: u8 = 22;
/// TLS content type: application data (23).
const CT_APP_DATA: u8 = 23;
/// TLS legacy version: TLS 1.2 wire encoding (0x0303).
const LEGACY_VERSION: [u8; 2] = [0x03, 0x03];
/// TLS 1.3 version (used in supported_versions extension): 0x0304.
const TLS13_VERSION: [u8; 2] = [0x03, 0x04];
/// TLS_AES_128_GCM_SHA256 cipher suite identifier.
const CIPHER_SUITE: [u8; 2] = [0x13, 0x01];

// ---------------------------------------------------------------------------
// HKDF helpers (RFC 5869 + RFC 8446 §7.1)
// ---------------------------------------------------------------------------

/// Build the variable-length `HkdfLabel` used by `HKDF-Expand-Label`:
///
/// ```text
/// struct HkdfLabel {
///   uint16 length;
///   opaque label<7..255>  = "tls13 " + Label;
///   opaque context<0..255>;
/// }
/// ```
fn hkdf_label(label: &[u8], context: &[u8], length: u16) -> Vec<u8> {
    let full_label = {
        let mut v = Vec::with_capacity(6 + label.len());
        v.extend_from_slice(b"tls13 ");
        v.extend_from_slice(label);
        v
    };
    // RFC 8446 §3.4: label and context fields are each length-prefixed with
    // a single byte (u8), so their encoded length must fit in [0, 255].
    // Assert before the cast to prevent silent truncation — any label or
    // context longer than 255 bytes is a programming error, not a runtime
    // condition that should be silently mishandled.
    assert!(
        full_label.len() <= 255,
        "HKDF label too long: {} bytes (max 255 per RFC 8446 §3.4)",
        full_label.len()
    );
    assert!(
        context.len() <= 255,
        "HKDF context too long: {} bytes (max 255 per RFC 8446 §3.4)",
        context.len()
    );
    let mut out = Vec::with_capacity(2 + 1 + full_label.len() + 1 + context.len());
    out.extend_from_slice(&length.to_be_bytes());
    out.push(full_label.len() as u8);
    out.extend_from_slice(&full_label);
    out.push(context.len() as u8);
    out.extend_from_slice(context);
    out
}

/// `HKDF-Expand-Label(secret, label, context, length)`.
///
/// Fills `out` (which must be exactly `length` bytes) using the RFC 8446
/// `HkdfLabel` info structure.
fn hkdf_expand_label(prk: &HkdfSha256, label: &[u8], context: &[u8], out: &mut [u8]) {
    let info = hkdf_label(label, context, out.len() as u16);
    prk.expand(&info, out)
        .expect("HKDF-Expand-Label: output length too large");
}

/// `Derive-Secret(secret, label, messages)` — convenience wrapper that passes
/// `SHA-256(messages)` as the `context`.
fn derive_secret(prk: &HkdfSha256, label: &[u8], transcript_hash: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    hkdf_expand_label(prk, label, transcript_hash, &mut out);
    out
}

/// Convenience: HKDF-Extract returning an `HkdfSha256` (PRK object) from byte slices.
///
/// `salt` and `ikm` are both byte slices.
///
/// `Hkdf::extract` returns `(prk_bytes, Hkdf_object)`.  We want the `Hkdf`
/// object so that we can call `expand` / `hkdf_expand_label` on it.
fn hkdf_extract_bytes(salt: &[u8], ikm: &[u8]) -> HkdfSha256 {
    let (_, hkdf) = HkdfSha256::extract(Some(salt), ikm);
    hkdf
}

// ---------------------------------------------------------------------------
// AES-128-GCM record sealer / opener
// ---------------------------------------------------------------------------

/// Traffic key material derived from a TLS 1.3 traffic secret.
///
/// The `Aes128Gcm` cipher is initialised **once** during `from_secret` and
/// reused across all `seal` / `open` calls, avoiding the overhead of repeated
/// key expansion on the software-only AES backend.
struct TrafficKeys {
    /// Pre-initialised AES-128-GCM cipher instance (key expansion done once).
    cipher: Aes128Gcm,
    /// 12-byte base IV; per-record nonce = IV ⊕ seq_num (RFC 8446 §5.3).
    iv: [u8; 12],
    /// Per-direction sequence counter; incremented after each seal/open.
    seq: u64,
}

impl TrafficKeys {
    /// Derive from a 32-byte TLS 1.3 traffic secret.
    ///
    /// Runs `HKDF-Expand-Label` to obtain the 16-byte key and 12-byte IV,
    /// then initialises the AES-128-GCM cipher once so `seal`/`open` do not
    /// repeat the key-expansion step.
    fn from_secret(secret: &[u8; 32]) -> Self {
        let prk = HkdfSha256::from_prk(secret).expect("traffic secret must be 32 bytes");
        let mut key = [0u8; 16];
        let mut iv = [0u8; 12];
        hkdf_expand_label(&prk, b"key", b"", &mut key);
        hkdf_expand_label(&prk, b"iv", b"", &mut iv);
        let cipher = Aes128Gcm::new_from_slice(&key).expect("key must be 16 bytes");
        Self { cipher, iv, seq: 0 }
    }

    /// Compute the per-record nonce: IV XOR seq_num (big-endian, 12 bytes).
    fn nonce(&self) -> [u8; 12] {
        let mut n = self.iv;
        let seq_bytes = self.seq.to_be_bytes(); // 8 bytes
                                                // XOR the last 8 bytes of the 12-byte IV with the sequence number.
        for (byte, s) in n[4..].iter_mut().zip(seq_bytes.iter()) {
            *byte ^= s;
        }
        n
    }

    /// Seal (encrypt + authenticate) a TLS inner plaintext record.
    ///
    /// `inner` must be `[real_payload…, content_type_byte]`.
    /// `header` is the 5-byte outer TLS record header used as AAD.
    /// Appends the 16-byte GCM tag to `inner` (in-place).
    ///
    /// Returns `Err` if the sequence-number limit has been reached.  RFC 8446
    /// §5.5 requires that a sender stop encrypting after `2^64 − 1` records
    /// because reusing a nonce under AES-GCM is catastrophic.
    fn seal(&mut self, header: &[u8; 5], inner: &mut Vec<u8>) -> Result<(), &'static str> {
        // RFC 8446 §5.5: senders MUST NOT encrypt more than 2^64-1 records
        // with the same key.  AES-GCM nonce reuse breaks confidentiality and
        // integrity; refuse to seal rather than wrap the counter.
        if self.seq == u64::MAX {
            return Err("TLS key exhausted: sequence number at u64::MAX — key update required");
        }
        use aes_gcm::aead::AeadMutInPlace;
        let nonce_bytes: [u8; 12] = self.nonce();
        let nonce: &Nonce<_> = <&Nonce<_>>::from(&nonce_bytes);
        let tag = self
            .cipher
            .encrypt_in_place_detached(nonce, header, inner)
            .expect("AES-GCM encrypt failed");
        inner.extend_from_slice(&tag[..]);
        self.seq += 1;
        Ok(())
    }

    /// Open (decrypt + verify) a TLS ciphertext record.
    ///
    /// `header` is the 5-byte outer TLS record header used as AAD.
    /// `ct` is `[ciphertext… | tag(16)]`, stripped to plaintext on success.
    fn open(&mut self, header: &[u8; 5], ct: &mut Vec<u8>) -> Result<(), &'static str> {
        use aes_gcm::aead::AeadMutInPlace;
        if ct.len() < 16 {
            return Err("AES-GCM: ciphertext too short for tag");
        }
        // RFC 8446 §5.5: receivers MUST NOT decrypt more than 2^64-1 records
        // with the same key.  Guard here to match the sender-side check.
        if self.seq == u64::MAX {
            return Err("TLS key exhausted: sequence number at u64::MAX — key update required");
        }
        let tag_pos = ct.len() - 16;
        let tag_bytes: [u8; 16] = ct[tag_pos..].try_into().unwrap();
        ct.truncate(tag_pos);
        let nonce_bytes: [u8; 12] = self.nonce();
        let nonce: &Nonce<_> = <&Nonce<_>>::from(&nonce_bytes);
        let tag: aes_gcm::Tag<_> = tag_bytes.into();
        self.cipher
            .decrypt_in_place_detached(nonce, header, ct, &tag)
            .map_err(|_| "AES-GCM decrypt failed")?;
        self.seq += 1;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TLS record framing helpers
// ---------------------------------------------------------------------------

/// Write a 5-byte TLS plaintext record header + payload to `buf`.
fn write_record(buf: &mut Vec<u8>, content_type: u8, payload: &[u8]) {
    let len = payload.len() as u16;
    buf.push(content_type);
    buf.extend_from_slice(&LEGACY_VERSION);
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(payload);
}

/// Read a single TLS record from `data[*pos..]`.
///
/// Returns `(content_type, payload_bytes)` and advances `*pos`.
fn read_record<'a>(data: &'a [u8], pos: &mut usize) -> Result<(u8, &'a [u8]), &'static str> {
    if *pos + 5 > data.len() {
        return Err("record: not enough header bytes");
    }
    let ct = data[*pos];
    // Skip legacy_version (bytes 1..=2).
    let len = u16::from_be_bytes([data[*pos + 3], data[*pos + 4]]) as usize;
    *pos += 5;
    if *pos + len > data.len() {
        return Err("record: not enough payload bytes");
    }
    let payload = &data[*pos..*pos + len];
    *pos += len;
    Ok((ct, payload))
}

/// Build a 5-byte TLS record header (used as AAD for AEAD).
fn record_header(content_type: u8, payload_len: usize) -> [u8; 5] {
    let len = payload_len as u16;
    [
        content_type,
        LEGACY_VERSION[0],
        LEGACY_VERSION[1],
        (len >> 8) as u8,
        len as u8,
    ]
}

// ---------------------------------------------------------------------------
// Handshake message helpers
// ---------------------------------------------------------------------------

/// Write a 4-byte handshake message header (type + 3-byte length) + body.
fn write_handshake(buf: &mut Vec<u8>, msg_type: u8, body: &[u8]) {
    buf.push(msg_type);
    let len = body.len() as u32;
    buf.push((len >> 16) as u8);
    buf.push((len >> 8) as u8);
    buf.push(len as u8);
    buf.extend_from_slice(body);
}

// ---------------------------------------------------------------------------
// ClientHello builder
// ---------------------------------------------------------------------------

/// Build a minimal TLS 1.3 `ClientHello` body.
///
/// Includes the required extensions:
/// - `supported_versions` (0x002b): TLS 1.3 only.
/// - `supported_groups`  (0x000a): P-256 (secp256r1 = 0x0017).
/// - `signature_algorithms` (0x000d): ecdsa_secp256r1_sha256 (0x0403).
/// - `key_share`         (0x0033): one P-256 uncompressed public key.
fn build_client_hello(client_random: &[u8; 32], client_public_key: &PublicKey) -> Vec<u8> {
    let mut body = Vec::new();

    // legacy_version = TLS 1.2
    body.extend_from_slice(&LEGACY_VERSION);
    // random (32 bytes)
    body.extend_from_slice(client_random);
    // legacy_session_id: empty (length 0)
    body.push(0x00);
    // cipher_suites: 1 suite = TLS_AES_128_GCM_SHA256
    body.extend_from_slice(&[0x00, 0x02]); // length = 2 bytes
    body.extend_from_slice(&CIPHER_SUITE);
    // legacy_compression_methods: [null]
    body.extend_from_slice(&[0x01, 0x00]);

    // Extensions -------------------------------------------------------
    let mut exts = Vec::new();

    // supported_versions (0x002b): client hello format = list of versions
    {
        let mut ext_data = Vec::new();
        ext_data.push(0x02); // list length = 2 bytes
        ext_data.extend_from_slice(&TLS13_VERSION); // TLS 1.3
        push_extension(&mut exts, 0x002b, &ext_data);
    }

    // supported_groups (0x000a): P-256 (secp256r1 = 0x0017)
    {
        let mut ext_data = Vec::new();
        ext_data.extend_from_slice(&[0x00, 0x02]); // list length
        ext_data.extend_from_slice(&[0x00, 0x17]); // secp256r1
        push_extension(&mut exts, 0x000a, &ext_data);
    }

    // signature_algorithms (0x000d): ecdsa_secp256r1_sha256 (0x0403)
    {
        let mut ext_data = Vec::new();
        ext_data.extend_from_slice(&[0x00, 0x02]); // list length
        ext_data.extend_from_slice(&[0x04, 0x03]); // ecdsa_secp256r1_sha256
        push_extension(&mut exts, 0x000d, &ext_data);
    }

    // key_share (0x0033): one P-256 key share
    {
        // Uncompressed SEC1 point: 65 bytes (0x04 prefix + 32 + 32).
        let pk_point = client_public_key.to_encoded_point(false);
        let pk_bytes = pk_point.as_bytes();
        let key_exchange_len = pk_bytes.len() as u16; // 65
        let entry_len = (2 + 2 + pk_bytes.len()) as u16; // group(2)+len(2)+pk(65)
        let mut ext_data = Vec::new();
        ext_data.extend_from_slice(&entry_len.to_be_bytes()); // client_shares list len
        ext_data.extend_from_slice(&[0x00, 0x17]); // secp256r1 group
        ext_data.extend_from_slice(&key_exchange_len.to_be_bytes());
        ext_data.extend_from_slice(pk_bytes);
        push_extension(&mut exts, 0x0033, &ext_data);
    }

    // Write extensions length + extensions into body.
    let exts_len = exts.len() as u16;
    body.extend_from_slice(&exts_len.to_be_bytes());
    body.extend_from_slice(&exts);

    body
}

/// Append a TLS extension (type + length + data) to `buf`.
fn push_extension(buf: &mut Vec<u8>, ext_type: u16, data: &[u8]) {
    buf.extend_from_slice(&ext_type.to_be_bytes());
    buf.extend_from_slice(&(data.len() as u16).to_be_bytes());
    buf.extend_from_slice(data);
}

// ---------------------------------------------------------------------------
// ServerHello builder
// ---------------------------------------------------------------------------

/// Build a minimal TLS 1.3 `ServerHello` body.
///
/// Extensions included:
/// - `supported_versions` (0x002b): TLS 1.3.
/// - `key_share`         (0x0033): server's P-256 public key.
fn build_server_hello(server_random: &[u8; 32], server_public_key: &PublicKey) -> Vec<u8> {
    let mut body = Vec::new();
    // legacy_version = TLS 1.2
    body.extend_from_slice(&LEGACY_VERSION);
    // random (32 bytes)
    body.extend_from_slice(server_random);
    // legacy_session_id_echo: empty
    body.push(0x00);
    // cipher_suite
    body.extend_from_slice(&CIPHER_SUITE);
    // legacy_compression_method: null
    body.push(0x00);

    // Extensions -------------------------------------------------------
    let mut exts = Vec::new();

    // supported_versions (0x002b): server hello format = single version
    push_extension(&mut exts, 0x002b, &TLS13_VERSION);

    // key_share (0x0033): server's P-256 key share
    {
        let pk_point = server_public_key.to_encoded_point(false);
        let pk_bytes = pk_point.as_bytes();
        let key_exchange_len = pk_bytes.len() as u16;
        let mut ext_data = Vec::new();
        ext_data.extend_from_slice(&[0x00, 0x17]); // secp256r1
        ext_data.extend_from_slice(&key_exchange_len.to_be_bytes());
        ext_data.extend_from_slice(pk_bytes);
        push_extension(&mut exts, 0x0033, &ext_data);
    }

    let exts_len = exts.len() as u16;
    body.extend_from_slice(&exts_len.to_be_bytes());
    body.extend_from_slice(&exts);

    body
}

// ---------------------------------------------------------------------------
// Parse key_share extension from ClientHello
// ---------------------------------------------------------------------------

/// Extract the client's P-256 public key from the ClientHello `key_share`
/// extension (0x0033).
///
/// `ch_body` is the raw bytes of the ClientHello message body (after the
/// 4-byte handshake header).
fn extract_client_key_share(ch_body: &[u8]) -> Result<PublicKey, &'static str> {
    // ClientHello layout:
    //   2  legacy_version
    //  32  random
    //   1  session_id_len (0)
    //   +  session_id
    //   2  cipher_suites_len
    //   +  cipher_suites
    //   1  compression_methods_len
    //   +  compression_methods
    //   2  extensions_len
    //   +  extensions
    let mut pos = 2 + 32; // skip legacy_version + random
                          // session_id
    if pos >= ch_body.len() {
        return Err("CH: truncated at session_id_len");
    }
    let sid_len = ch_body[pos] as usize;
    pos += 1 + sid_len;
    // cipher_suites
    if pos + 2 > ch_body.len() {
        return Err("CH: truncated at cipher_suites_len");
    }
    let cs_len = u16::from_be_bytes([ch_body[pos], ch_body[pos + 1]]) as usize;
    pos += 2 + cs_len;
    // compression_methods
    if pos >= ch_body.len() {
        return Err("CH: truncated at compression_methods_len");
    }
    let cm_len = ch_body[pos] as usize;
    pos += 1 + cm_len;
    // extensions
    if pos + 2 > ch_body.len() {
        return Err("CH: truncated at extensions_len");
    }
    let exts_len = u16::from_be_bytes([ch_body[pos], ch_body[pos + 1]]) as usize;
    pos += 2;
    let exts_end = pos + exts_len;

    // SECURITY: Verify that the declared extensions block fits within the
    // buffer before iterating.  Without this check a malformed ClientHello
    // with a large `exts_len` could cause an out-of-bounds read inside the
    // loop when accessing `ch_body[pos]` or `ch_body[pos + 1..3]`.
    if exts_end > ch_body.len() {
        return Err("CH: extensions extend beyond body");
    }

    while pos + 4 <= exts_end {
        let ext_type = u16::from_be_bytes([ch_body[pos], ch_body[pos + 1]]);
        let ext_len = u16::from_be_bytes([ch_body[pos + 2], ch_body[pos + 3]]) as usize;
        pos += 4;
        if pos + ext_len > ch_body.len() {
            return Err("CH: extension data out of bounds");
        }
        let ext_data = &ch_body[pos..pos + ext_len];
        pos += ext_len;

        if ext_type == 0x0033 {
            // key_share ClientHello: uint16 list_len, then KeyShareEntry+
            if ext_data.len() < 2 {
                return Err("key_share: too short");
            }
            let list_len = u16::from_be_bytes([ext_data[0], ext_data[1]]) as usize;
            let mut p = 2;
            let list_end = 2 + list_len;
            while p + 4 <= list_end && p + 4 <= ext_data.len() {
                let group = u16::from_be_bytes([ext_data[p], ext_data[p + 1]]);
                let ke_len = u16::from_be_bytes([ext_data[p + 2], ext_data[p + 3]]) as usize;
                p += 4;
                if group == 0x0017 {
                    // secp256r1
                    if p + ke_len > ext_data.len() {
                        return Err("CH: key_share entry out of bounds");
                    }
                    let pk_bytes = &ext_data[p..p + ke_len];
                    return PublicKey::from_sec1_bytes(pk_bytes)
                        .map_err(|_| "CH: invalid P-256 public key");
                }
                p += ke_len;
            }
            return Err("CH: no P-256 key share found");
        }
    }
    Err("CH: key_share extension not found")
}

// ---------------------------------------------------------------------------
// Finished MAC
// ---------------------------------------------------------------------------

/// Compute the TLS 1.3 Finished MAC.
///
/// ```text
/// finished_key = HKDF-Expand-Label(traffic_secret, "finished", "", 32)
/// Finished MAC = HMAC-SHA256(finished_key, transcript_hash)
/// ```
fn compute_finished_mac(traffic_secret: &[u8; 32], transcript_hash: &[u8]) -> [u8; 32] {
    let prk = HkdfSha256::from_prk(traffic_secret).unwrap();
    let mut finished_key = [0u8; 32];
    hkdf_expand_label(&prk, b"finished", b"", &mut finished_key);

    let mut mac = <HmacSha256 as hmac::Mac>::new_from_slice(&finished_key).unwrap();
    mac.update(transcript_hash);
    mac.finalize().into_bytes().into()
}

// ---------------------------------------------------------------------------
// Main loopback test
// ---------------------------------------------------------------------------

/// Run a complete TLS 1.3 handshake (and PING application-data exchange)
/// in-process.
///
/// Uses two `Vec<u8>` buffers as the transport:
///   - `c2s`: client → server
///   - `s2c`: server → client
///
/// # Errors
///
/// Returns `Err(&'static str)` if any step of the handshake fails.
pub fn run_tls_loopback_test() -> Result<(), &'static str> {
    // ----------------------------------------------------------------
    // CPUID guard: fail fast if RDRAND is unavailable
    //
    // Without this check, calling the RDRAND instruction on a CPU or
    // hypervisor that does not support it raises a #UD (invalid-opcode)
    // exception which would crash the bare-metal kernel.
    // ----------------------------------------------------------------
    if !RdRand::is_available() {
        return Err("RDRAND not available on this CPU (CPUID leaf 1, ECX bit 30 not set)");
    }

    // ----------------------------------------------------------------
    // Generate ephemeral key pairs
    // ----------------------------------------------------------------
    let mut rng = RdRand;
    let client_secret = EphemeralSecret::random(&mut rng);
    let client_pk = client_secret.public_key();

    let server_secret = EphemeralSecret::random(&mut rng);
    let server_pk = server_secret.public_key();

    // ----------------------------------------------------------------
    // Client random + ClientHello
    // ----------------------------------------------------------------
    let mut client_random = [0u8; 32];
    rng.fill_bytes(&mut client_random);

    let ch_body = build_client_hello(&client_random, &client_pk);

    // Wrap in handshake message (type 0x01 = ClientHello).
    let mut ch_msg = Vec::new();
    write_handshake(&mut ch_msg, 0x01, &ch_body);

    // Write as plaintext TLS record.
    let mut c2s: Vec<u8> = Vec::new();
    write_record(&mut c2s, CT_HANDSHAKE, &ch_msg);

    // Transcript starts with the raw handshake message (not the record header).
    let mut transcript = Sha256::new();
    transcript.update(&ch_msg);

    // ----------------------------------------------------------------
    // Server reads ClientHello, sends ServerHello + encrypted extensions
    // ----------------------------------------------------------------
    let mut s2c: Vec<u8> = Vec::new();

    let mut c2s_pos: usize = 0;
    let (_, ch_record_payload) = read_record(&c2s, &mut c2s_pos)?;
    // ch_record_payload is the handshake message bytes: [type(1), len(3), body…]
    if ch_record_payload.is_empty() || ch_record_payload[0] != 0x01 {
        return Err("server: expected ClientHello handshake message type");
    }
    // ch_body starts at offset 4 (skip 1-byte type + 3-byte length).
    if ch_record_payload.len() < 4 {
        return Err("server: ClientHello handshake msg too short");
    }
    let ch_body_recv = &ch_record_payload[4..];

    // Parse client's key share.
    let client_pk_recv = extract_client_key_share(ch_body_recv)?;

    // Compute ECDHE shared secret.
    let shared_secret = server_secret.diffie_hellman(&client_pk_recv);
    let ecdhe_bytes: &[u8] = shared_secret.raw_secret_bytes().as_ref();

    // Build ServerHello.
    let mut server_random = [0u8; 32];
    rng.fill_bytes(&mut server_random);

    let sh_body = build_server_hello(&server_random, &server_pk);
    let mut sh_msg = Vec::new();
    write_handshake(&mut sh_msg, 0x02, &sh_body); // 0x02 = ServerHello
    write_record(&mut s2c, CT_HANDSHAKE, &sh_msg);

    // Extend transcript with ServerHello.
    transcript.update(&sh_msg);
    let ch_sh_hash: [u8; 32] = transcript.clone().finalize().into();

    // ----------------------------------------------------------------
    // Key schedule: derive handshake traffic secrets
    // ----------------------------------------------------------------

    // EarlySecret = HKDF-Extract(salt=0^32, ikm=0^32)
    let zero32 = [0u8; 32];
    let early_secret = hkdf_extract_bytes(&zero32, &zero32);

    // derived = HKDF-Expand-Label(EarlySecret, "derived", SHA256(""), 32)
    let empty_hash: [u8; 32] = Sha256::digest(b"").into();
    let derived = derive_secret(&early_secret, b"derived", &empty_hash);

    // HandshakeSecret = HKDF-Extract(salt=derived, ikm=ECDHE)
    let hs = hkdf_extract_bytes(&derived, ecdhe_bytes);

    // client_hs_traffic_secret
    let c_hs_ts = derive_secret(&hs, b"c hs traffic", &ch_sh_hash);
    // server_hs_traffic_secret
    let s_hs_ts = derive_secret(&hs, b"s hs traffic", &ch_sh_hash);

    // Derive write keys (cipher initialised once, reused across records).
    let mut server_hs_keys = TrafficKeys::from_secret(&s_hs_ts);
    let mut client_hs_keys = TrafficKeys::from_secret(&c_hs_ts);

    // ----------------------------------------------------------------
    // Server sends encrypted handshake messages:
    //   EncryptedExtensions (0x08)
    //   Certificate         (0x0b)
    //   CertificateVerify   (0x0f)
    //   Finished            (0x14)
    // ----------------------------------------------------------------

    // EncryptedExtensions: empty extensions list.
    let mut ee_msg = Vec::new();
    write_handshake(&mut ee_msg, 0x08, &[0x00, 0x00]); // 0x08 = EncryptedExtensions

    // Certificate: the embedded self-signed cert.
    // TLS 1.3 Certificate message format:
    //   1 byte  request_context_len = 0
    //   3 bytes cert_list_len
    //     per entry: 3 bytes cert_len, cert_data, 2 bytes extensions_len = 0
    let cert_entry_len = (3 + CERT_DER.len() + 2) as u32;
    let cert_list_len = cert_entry_len; // one entry only
    let mut cert_msg_body: Vec<u8> = alloc::vec![
        0x00, // request_context_len
        (cert_list_len >> 16) as u8,
        (cert_list_len >> 8) as u8,
        cert_list_len as u8,
        (CERT_DER.len() >> 16) as u8,
        (CERT_DER.len() >> 8) as u8,
        CERT_DER.len() as u8,
    ];
    cert_msg_body.extend_from_slice(CERT_DER);
    cert_msg_body.extend_from_slice(&[0x00, 0x00]); // no extensions

    let mut cert_msg = Vec::new();
    write_handshake(&mut cert_msg, 0x0b, &cert_msg_body); // 0x0b = Certificate

    // Update transcript with EE + Certificate so we can compute the
    // CertificateVerify signature over the correct hash.
    transcript.update(&ee_msg);
    transcript.update(&cert_msg);
    let transcript_for_cv: [u8; 32] = transcript.clone().finalize().into();

    // CertificateVerify: sign the transcript hash.
    //
    // TLS 1.3 CertificateVerify context string for server:
    //   64 spaces + "TLS 1.3, server CertificateVerify" + 0x00 + transcript_hash
    let mut cv_sign_input = Vec::new();
    cv_sign_input.extend_from_slice(&[0x20u8; 64]);
    cv_sign_input.extend_from_slice(b"TLS 1.3, server CertificateVerify");
    cv_sign_input.push(0x00);
    cv_sign_input.extend_from_slice(&transcript_for_cv);

    // Load signing key from embedded PKCS8 DER.
    let signing_key = p256::ecdsa::SigningKey::from_pkcs8_der(KEY_DER)
        .map_err(|_| "CertificateVerify: failed to load signing key")?;

    // Sign using RFC 6979 deterministic ECDSA (DER-encoded output).
    use p256::ecdsa::signature::Signer;
    let sig: p256::ecdsa::DerSignature = signing_key.sign(&cv_sign_input);
    let sig_bytes = sig.as_bytes();

    // CertificateVerify body:
    //   2 bytes signature_algorithm (ecdsa_secp256r1_sha256 = 0x0403)
    //   2 bytes signature_len
    //   signature bytes
    let mut cv_body = Vec::new();
    cv_body.extend_from_slice(&[0x04, 0x03]); // ecdsa_secp256r1_sha256
    cv_body.extend_from_slice(&(sig_bytes.len() as u16).to_be_bytes());
    cv_body.extend_from_slice(sig_bytes);

    let mut cv_msg = Vec::new();
    write_handshake(&mut cv_msg, 0x0f, &cv_body); // 0x0f = CertificateVerify

    // Update transcript with CertificateVerify.
    transcript.update(&cv_msg);
    let transcript_before_server_finished: [u8; 32] = transcript.clone().finalize().into();

    // Server Finished.
    let server_finished_mac = compute_finished_mac(&s_hs_ts, &transcript_before_server_finished);
    let mut sfin_msg = Vec::new();
    write_handshake(&mut sfin_msg, 0x14, &server_finished_mac); // 0x14 = Finished

    // Encrypt and send: EE + Cert + CV + Finished as individual encrypted records.
    // Each encrypted record has:
    //   outer type = CT_APP_DATA (23) — per RFC 8446 §5.2 (TLSCiphertext)
    //   inner plaintext = [handshake_msg…, CT_HANDSHAKE(22)]
    send_encrypted_handshake(&mut s2c, &mut server_hs_keys, &ee_msg)?;
    send_encrypted_handshake(&mut s2c, &mut server_hs_keys, &cert_msg)?;
    send_encrypted_handshake(&mut s2c, &mut server_hs_keys, &cv_msg)?;
    send_encrypted_handshake(&mut s2c, &mut server_hs_keys, &sfin_msg)?;

    // ----------------------------------------------------------------
    // Client processes ServerHello + encrypted records
    // ----------------------------------------------------------------

    let mut s2c_pos: usize = 0;

    // Read ServerHello plaintext record.
    let (_, sh_record_payload) = read_record(&s2c, &mut s2c_pos)?;
    if sh_record_payload.is_empty() || sh_record_payload[0] != 0x02 {
        return Err("client: expected ServerHello");
    }
    // SECURITY: Verify we have at least 4 bytes (type + 3-byte length field)
    // before slicing into sh_record_payload[4..] for key-share parsing.
    if sh_record_payload.len() < 4 {
        return Err("client: ServerHello handshake msg too short");
    }

    // Extract server's public key from ServerHello to compute shared secret.
    let server_pk_recv = extract_server_key_share(&sh_record_payload[4..])?;

    // Client computes ECDHE shared secret.
    let client_shared = client_secret.diffie_hellman(&server_pk_recv);
    let client_ecdhe_bytes: &[u8] = client_shared.raw_secret_bytes().as_ref();

    // Client re-derives the same key schedule.
    let client_early = hkdf_extract_bytes(&zero32, &zero32);
    let client_empty_hash: [u8; 32] = Sha256::digest(b"").into();
    let client_derived = derive_secret(&client_early, b"derived", &client_empty_hash);
    let client_hs = hkdf_extract_bytes(&client_derived, client_ecdhe_bytes);

    // Re-derive CH+SH hash.  Client has seen the same bytes as server.
    let mut client_transcript = Sha256::new();
    client_transcript.update(&ch_msg); // ClientHello
    client_transcript.update(&sh_msg); // ServerHello
    let client_ch_sh_hash: [u8; 32] = client_transcript.clone().finalize().into();

    let client_c_hs_ts = derive_secret(&client_hs, b"c hs traffic", &client_ch_sh_hash);
    let client_s_hs_ts = derive_secret(&client_hs, b"s hs traffic", &client_ch_sh_hash);

    let mut client_recv_keys = TrafficKeys::from_secret(&client_s_hs_ts);
    let mut client_send_keys = TrafficKeys::from_secret(&client_c_hs_ts);

    // Read and decrypt the four server handshake messages.
    let ee_payload = read_decrypt_handshake(&s2c, &mut s2c_pos, &mut client_recv_keys, 0x08)?;
    client_transcript.update(&ee_payload);

    let cert_payload = read_decrypt_handshake(&s2c, &mut s2c_pos, &mut client_recv_keys, 0x0b)?;
    client_transcript.update(&cert_payload);

    let cv_payload = read_decrypt_handshake(&s2c, &mut s2c_pos, &mut client_recv_keys, 0x0f)?;

    // ----------------------------------------------------------------
    // Verify CertificateVerify signature (RFC 8446 §4.4.3)
    //
    // The client reconstructs the server's signing input and verifies
    // the ECDSA signature over it.  This proves the server possesses
    // the private key corresponding to the certificate it sent — i.e.
    // proof of possession.
    //
    // Signing input:
    //   64 × 0x20  ||  "TLS 1.3, server CertificateVerify"  ||  0x00  ||  transcript_hash
    // where transcript_hash = SHA-256(CH || SH || EE || Cert)
    // (the same transcript_for_cv the server computed).
    // ----------------------------------------------------------------
    {
        // Transcript hash at the point of CertificateVerify: CH+SH+EE+Cert.
        let transcript_for_cv_client: [u8; 32] = client_transcript.clone().finalize().into();

        let mut cv_verify_input = Vec::new();
        cv_verify_input.extend_from_slice(&[0x20u8; 64]);
        cv_verify_input.extend_from_slice(b"TLS 1.3, server CertificateVerify");
        cv_verify_input.push(0x00);
        cv_verify_input.extend_from_slice(&transcript_for_cv_client);

        // cv_payload layout: [type(1)=0x0f, len(3), sig_alg(2), sig_len(2), sig…]
        if cv_payload.len() < 8 {
            return Err("client: CertificateVerify too short");
        }
        let sig_alg = u16::from_be_bytes([cv_payload[4], cv_payload[5]]);
        if sig_alg != 0x0403 {
            return Err("client: CertificateVerify uses unexpected signature algorithm");
        }
        let sig_len = u16::from_be_bytes([cv_payload[6], cv_payload[7]]) as usize;
        if cv_payload.len() < 8 + sig_len {
            return Err("client: CertificateVerify signature truncated");
        }
        let sig_bytes = &cv_payload[8..8 + sig_len];

        // Derive verifying key from the embedded PKCS8 private key.
        // In production this would be extracted from the X.509 certificate;
        // for this self-signed loopback demo the key is known at build time.
        let signing_key_for_verify = p256::ecdsa::SigningKey::from_pkcs8_der(KEY_DER)
            .map_err(|_| "client: failed to load server public key for CV verification")?;
        let verifying_key = p256::ecdsa::VerifyingKey::from(&signing_key_for_verify);

        use p256::ecdsa::signature::Verifier;
        let sig = p256::ecdsa::DerSignature::try_from(sig_bytes)
            .map_err(|_| "client: CertificateVerify DER signature invalid")?;
        verifying_key
            .verify(&cv_verify_input, &sig)
            .map_err(|_| "client: CertificateVerify signature verification FAILED")?;
    }

    client_transcript.update(&cv_payload);

    // Compute the transcript hash just before the server Finished (= after CV).
    let client_before_sfin_hash: [u8; 32] = client_transcript.clone().finalize().into();

    let sfin_payload = read_decrypt_handshake(&s2c, &mut s2c_pos, &mut client_recv_keys, 0x14)?;

    // Verify server Finished MAC.
    // sfin_payload is the full handshake message: [0x14, len(3), mac(32)]
    if sfin_payload.len() < 4 {
        return Err("client: server Finished too short");
    }
    let received_sfin_mac = &sfin_payload[4..]; // skip type + 3-byte len
    let expected_sfin_mac = compute_finished_mac(&client_s_hs_ts, &client_before_sfin_hash);
    if received_sfin_mac != expected_sfin_mac {
        return Err("client: server Finished MAC mismatch");
    }

    // Update transcript with server Finished.
    client_transcript.update(&sfin_payload);

    // ----------------------------------------------------------------
    // Client sends Finished
    // ----------------------------------------------------------------

    let client_before_cfin_hash: [u8; 32] = client_transcript.clone().finalize().into();
    let client_finished_mac = compute_finished_mac(&client_c_hs_ts, &client_before_cfin_hash);
    let mut cfin_msg = Vec::new();
    write_handshake(&mut cfin_msg, 0x14, &client_finished_mac);

    send_encrypted_handshake(&mut c2s, &mut client_send_keys, &cfin_msg)?;

    // Update server transcript with server Finished + client Finished.
    transcript.update(&sfin_msg);

    // ----------------------------------------------------------------
    // Server processes client Finished
    // ----------------------------------------------------------------

    // `read_record` returns `(outer_content_type, payload)`.
    // For encrypted TLS 1.3 records the outer type is always CT_APP_DATA (23);
    // the real content type is the last byte of the decrypted inner plaintext.
    // Checking `payload[0]` instead of the returned content type is wrong
    // because `payload` is the raw ciphertext, not the decoded record.
    let (cfin_outer_ct, cfin_record) = read_record(&c2s, &mut c2s_pos)?;
    if cfin_outer_ct != CT_APP_DATA {
        return Err("server: expected encrypted client Finished (CT_APP_DATA outer type)");
    }
    let mut cfin_ct: Vec<u8> = cfin_record.to_vec();
    let cfin_outer_hdr = record_header(CT_APP_DATA, cfin_ct.len());
    client_hs_keys.open(&cfin_outer_hdr, &mut cfin_ct)?;

    // Strip and validate the TLS inner content type byte.
    // RFC 8446 §5.4: the last byte of TLSInnerPlaintext is the real content type.
    // For a client Finished this MUST be CT_HANDSHAKE (22).
    if cfin_ct.is_empty() {
        return Err("server: decrypted client Finished is empty");
    }
    let inner_ct = cfin_ct.pop().unwrap(); // safe: checked non-empty above
    if inner_ct != CT_HANDSHAKE {
        return Err("server: client Finished inner content type is not CT_HANDSHAKE");
    }

    // cfin_ct is now the raw handshake message bytes.
    if cfin_ct.len() < 4 || cfin_ct[0] != 0x14 {
        return Err("server: expected client Finished handshake type");
    }
    let received_cfin_mac = &cfin_ct[4..];

    // Server computes expected client Finished over its transcript.
    // Server transcript at this point: CH + SH + EE + Cert + CV + SFin
    let server_before_cfin_hash: [u8; 32] = transcript.finalize().into();
    let expected_cfin_mac = compute_finished_mac(&c_hs_ts, &server_before_cfin_hash);
    if received_cfin_mac != expected_cfin_mac {
        return Err("server: client Finished MAC mismatch");
    }

    // ----------------------------------------------------------------
    // Application data: client sends "PING", server decrypts and verifies.
    //
    // RFC 8446 §7.1 application traffic key schedule:
    //
    //   derived      = Derive-Secret(HandshakeSecret, "derived", SHA256(""))
    //   MasterSecret = HKDF-Extract(derived, 0^32)
    //   c ap traffic = Derive-Secret(MS, "c ap traffic", transcript(CH…SF))
    //   s ap traffic = Derive-Secret(MS, "s ap traffic", transcript(CH…SF))
    //
    // transcript(CH…SF) = SHA-256(CH || SH || EE || Cert || CV || SFin),
    // which is `client_before_cfin_hash` on the client and
    // `server_before_cfin_hash` on the server — both are the same value if
    // the handshake succeeded.
    // ----------------------------------------------------------------

    // Client side: derive MasterSecret from the client's HandshakeSecret (client_hs).
    let client_ms_derived = {
        let empty_hash_ms: [u8; 32] = Sha256::digest(b"").into();
        derive_secret(&client_hs, b"derived", &empty_hash_ms)
    };
    let client_master_secret = hkdf_extract_bytes(&client_ms_derived, &zero32);
    // App traffic secret for client→server direction.
    // `client_before_cfin_hash` = hash(CH||SH||EE||Cert||CV||SFin) — exactly
    // the RFC 8446 transcript input for "c ap traffic".
    let c_app_ts = derive_secret(
        &client_master_secret,
        b"c ap traffic",
        &client_before_cfin_hash,
    );
    let mut client_app_send = TrafficKeys::from_secret(&c_app_ts);

    // Server side: derive the same MasterSecret from the server's HandshakeSecret (hs).
    let server_ms_derived = {
        let empty_hash_ms: [u8; 32] = Sha256::digest(b"").into();
        derive_secret(&hs, b"derived", &empty_hash_ms)
    };
    let server_master_secret = hkdf_extract_bytes(&server_ms_derived, &zero32);
    // Server decrypts with c_ap_traffic (same key as client's send key).
    let s_app_recv_ts = derive_secret(
        &server_master_secret,
        b"c ap traffic",
        &server_before_cfin_hash,
    );
    let mut server_app_recv = TrafficKeys::from_secret(&s_app_recv_ts);

    // Client sends "PING" as an application data record.
    let mut ping_inner: Vec<u8> = b"PING".to_vec();
    ping_inner.push(CT_APP_DATA); // inner content type
    let ct_len_ping = ping_inner.len() + 16; // plaintext + GCM tag
    let ping_outer_hdr = record_header(CT_APP_DATA, ct_len_ping);
    client_app_send.seal(&ping_outer_hdr, &mut ping_inner)?;
    // ping_inner is now ciphertext (including GCM tag).
    write_record(&mut c2s, CT_APP_DATA, &ping_inner);

    // Server reads and decrypts the PING.
    // As above: check the content type returned by `read_record`, not
    // the first byte of the ciphertext payload.
    let (ping_outer_ct, ping_record) = read_record(&c2s, &mut c2s_pos)?;
    if ping_outer_ct != CT_APP_DATA {
        return Err("server: expected encrypted PING record (CT_APP_DATA outer type)");
    }
    let mut ping_ct: Vec<u8> = ping_record.to_vec();
    let ping_hdr = record_header(CT_APP_DATA, ping_ct.len());
    server_app_recv.open(&ping_hdr, &mut ping_ct)?;

    // Strip and validate the TLS inner content type byte.
    // For application data the inner type MUST be CT_APP_DATA (23).
    if ping_ct.is_empty() {
        return Err("server: decrypted PING is empty");
    }
    let ping_inner_ct = ping_ct.pop().unwrap(); // safe: checked non-empty above
    if ping_inner_ct != CT_APP_DATA {
        return Err("server: PING inner content type is not CT_APP_DATA");
    }

    if ping_ct != b"PING" {
        return Err("server: received unexpected payload instead of PING");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers for encrypted handshake record send/receive
// ---------------------------------------------------------------------------

/// Encrypt a handshake message and write it as a TLSCiphertext record.
///
/// The outer content type is `CT_APP_DATA` (23) as required by RFC 8446 §5.2.
/// The inner plaintext is `[msg…, CT_HANDSHAKE(22)]`.
fn send_encrypted_handshake(
    buf: &mut Vec<u8>,
    keys: &mut TrafficKeys,
    msg: &[u8],
) -> Result<(), &'static str> {
    let mut inner: Vec<u8> = msg.to_vec();
    inner.push(CT_HANDSHAKE); // inner content type
    let ct_len = inner.len() + 16; // plaintext + tag
    let hdr = record_header(CT_APP_DATA, ct_len);
    keys.seal(&hdr, &mut inner)?;
    write_record(buf, CT_APP_DATA, &inner);
    Ok(())
}

/// Read, decrypt, and return a single encrypted handshake message from `data`.
///
/// Verifies that the outer content type is `CT_APP_DATA` (23), that the
/// decrypted inner content type is `CT_HANDSHAKE` (22), and that the
/// handshake message type byte matches `expected_type`.
fn read_decrypt_handshake(
    data: &[u8],
    pos: &mut usize,
    keys: &mut TrafficKeys,
    expected_type: u8,
) -> Result<Vec<u8>, &'static str> {
    let (ct, record_payload) = read_record(data, pos)?;
    if ct != CT_APP_DATA {
        return Err("read_decrypt_handshake: expected CT_APP_DATA outer type");
    }
    let mut ciphertext: Vec<u8> = record_payload.to_vec();
    let outer_hdr = record_header(CT_APP_DATA, ciphertext.len());
    keys.open(&outer_hdr, &mut ciphertext)?;
    // Strip and validate inner content type byte.
    let inner_ct = ciphertext.pop().ok_or("decrypted record empty")?;
    if inner_ct != CT_HANDSHAKE {
        return Err("read_decrypt_handshake: inner content type is not handshake");
    }
    // Verify handshake message type.
    if ciphertext.is_empty() || ciphertext[0] != expected_type {
        return Err("read_decrypt_handshake: unexpected handshake message type");
    }
    Ok(ciphertext)
}

// ---------------------------------------------------------------------------
// Parse key_share extension from ServerHello
// ---------------------------------------------------------------------------

/// Extract the server's P-256 public key from the ServerHello `key_share`
/// extension (0x0033).
///
/// `sh_body` is the raw bytes of the ServerHello message body (after the
/// 4-byte handshake header).
fn extract_server_key_share(sh_body: &[u8]) -> Result<PublicKey, &'static str> {
    // ServerHello layout:
    //   2  legacy_version
    //  32  random
    //   1  session_id_len (0)
    //   +  session_id
    //   2  cipher_suite
    //   1  compression_method
    //   2  extensions_len
    //   +  extensions
    let mut pos = 2 + 32;
    if pos >= sh_body.len() {
        return Err("SH: truncated at session_id_len");
    }
    let sid_len = sh_body[pos] as usize;
    pos += 1 + sid_len;
    pos += 2; // cipher_suite
    pos += 1; // compression_method
    if pos + 2 > sh_body.len() {
        return Err("SH: truncated at extensions_len");
    }
    let exts_len = u16::from_be_bytes([sh_body[pos], sh_body[pos + 1]]) as usize;
    pos += 2;
    let exts_end = pos + exts_len;

    // SECURITY: Verify that the declared extensions block fits within the
    // buffer before iterating.  Without this check a malformed ServerHello
    // with an inflated `exts_len` could cause parsing to continue past the
    // end of `sh_body`, reading garbage bytes and potentially crashing.
    // This mirrors the identical check that was added to `extract_client_key_share`.
    if exts_end > sh_body.len() {
        return Err("SH: extensions extend beyond body");
    }

    while pos + 4 <= exts_end {
        let ext_type = u16::from_be_bytes([sh_body[pos], sh_body[pos + 1]]);
        let ext_len = u16::from_be_bytes([sh_body[pos + 2], sh_body[pos + 3]]) as usize;
        pos += 4;
        if pos + ext_len > sh_body.len() {
            return Err("SH: extension data out of bounds");
        }
        let ext_data = &sh_body[pos..pos + ext_len];
        pos += ext_len;

        if ext_type == 0x0033 {
            // key_share ServerHello: NamedGroup(2) + key_exchange_len(2) + bytes
            if ext_data.len() < 4 {
                return Err("SH key_share: too short");
            }
            let group = u16::from_be_bytes([ext_data[0], ext_data[1]]);
            if group != 0x0017 {
                return Err("SH key_share: unexpected group (expected P-256)");
            }
            let ke_len = u16::from_be_bytes([ext_data[2], ext_data[3]]) as usize;
            if 4 + ke_len > ext_data.len() {
                return Err("SH key_share: key bytes out of bounds");
            }
            let pk_bytes = &ext_data[4..4 + ke_len];
            return PublicKey::from_sec1_bytes(pk_bytes)
                .map_err(|_| "SH: invalid P-256 public key");
        }
    }
    Err("SH: key_share extension not found")
}

// ---------------------------------------------------------------------------
// smoltcp TCP helpers
// ---------------------------------------------------------------------------

/// Send all bytes in `data` through smoltcp TCP socket `handle`.
///
/// Polls the smoltcp interface in a loop until all bytes have been queued
/// into the socket's send buffer.  Each iteration advances simulated wall
/// time by 1 ms so TCP retransmission timers advance correctly.
///
/// Returns `Err` if the socket is closed or the iteration budget is exceeded.
fn tcp_send_all(
    iface: &mut Interface,
    device: &mut Loopback,
    sockets: &mut SocketSet,
    handle: smoltcp::iface::SocketHandle,
    data: &[u8],
    time_ms: &mut i64,
) -> Result<(), &'static str> {
    let mut sent = 0;
    for _ in 0..2000 {
        iface.poll(Instant::from_millis(*time_ms), device, sockets);
        *time_ms += 1;
        let sock = sockets.get_mut::<tcp::Socket>(handle);
        if !sock.may_send() {
            // Socket not yet connected; keep polling.
            continue;
        }
        let n = sock
            .send_slice(&data[sent..])
            .map_err(|_| "tcp_send_all: send_slice failed")?;
        sent += n;
        if sent >= data.len() {
            break;
        }
    }
    if sent < data.len() {
        return Err("tcp_send_all: not all bytes sent within budget");
    }
    Ok(())
}

/// Receive exactly `want` bytes from smoltcp TCP socket `handle`.
///
/// Polls the smoltcp interface until `want` bytes have been accumulated in
/// `out`.  Each iteration advances simulated time by 1 ms.
///
/// Returns `Err` if the socket is closed or the iteration budget is exceeded.
fn tcp_recv_n(
    iface: &mut Interface,
    device: &mut Loopback,
    sockets: &mut SocketSet,
    handle: smoltcp::iface::SocketHandle,
    want: usize,
    out: &mut Vec<u8>,
    time_ms: &mut i64,
) -> Result<(), &'static str> {
    let start = out.len();
    let mut tmp = alloc::vec![0u8; want];
    for _ in 0..2000 {
        iface.poll(Instant::from_millis(*time_ms), device, sockets);
        *time_ms += 1;
        let sock = sockets.get_mut::<tcp::Socket>(handle);
        if sock.can_recv() {
            let available = want - (out.len() - start);
            let n = sock
                .recv_slice(&mut tmp[..available])
                .map_err(|_| "tcp_recv_n: recv_slice failed")?;
            out.extend_from_slice(&tmp[..n]);
        }
        if out.len() - start >= want {
            break;
        }
    }
    if out.len() - start < want {
        return Err("tcp_recv_n: not enough bytes received within budget");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// E4.4 smoltcp TCP integration test
// ---------------------------------------------------------------------------

/// Run a TLS 1.3 ClientHello + ServerHello exchange over a real smoltcp TCP
/// loopback connection.
///
/// This function demonstrates that TLS 1.3 records transit the smoltcp TCP
/// stack intact.  The flow is:
///
/// 1. A smoltcp TCP loopback interface is set up with two sockets:
///    - server listening on port 4433
///    - client connecting to 127.0.0.1:4433
///
/// 2. The client generates a TLS 1.3 ClientHello and sends it through
///    the TCP socket.
///
/// 3. The server reads the bytes from its TCP socket and parses the
///    ClientHello, extracting the client's P-256 key share.
///
/// 4. The server builds a TLS 1.3 ServerHello with its own key share and
///    sends it back through the TCP socket.
///
/// 5. The client reads the ServerHello and verifies the P-256 key share
///    parses correctly.
///
/// Combined with `run_tls_loopback_test()` (which validates the complete
/// RFC 8446 state machine), this confirms E4.4's "TLS over smoltcp TCP"
/// requirement.
pub fn run_tls_over_smoltcp_test() -> Result<(), &'static str> {
    // ----------------------------------------------------------------
    // CPUID guard — RDRAND required for ephemeral key generation.
    // ----------------------------------------------------------------
    if !RdRand::is_available() {
        return Err("RDRAND not available on this CPU — cannot run TLS/TCP test");
    }

    // ----------------------------------------------------------------
    // Set up smoltcp loopback interface (same topology as E4.3).
    // ----------------------------------------------------------------
    let mut device = Loopback::new(Medium::Ethernet);
    let mut config = Config::new(EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x02]).into());
    // Fixed seed for deterministic TCP sequence numbers in CI.
    config.random_seed = 0xc0de_face_dead_cafe;
    let mut iface = Interface::new(config, &mut device, Instant::ZERO);
    iface.update_ip_addrs(|addrs| {
        addrs
            .push(IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8))
            .unwrap();
    });

    let mut sockets = SocketSet::new(alloc::vec![]);

    // TLS server: listen on port 4433 (standard HTTPS/TLS port).
    let srv_sock = tcp::Socket::new(
        tcp::SocketBuffer::new(alloc::vec![0u8; 8192]),
        tcp::SocketBuffer::new(alloc::vec![0u8; 8192]),
    );
    let srv_h = sockets.add(srv_sock);
    sockets
        .get_mut::<tcp::Socket>(srv_h)
        .listen(4433_u16)
        .unwrap();

    // TLS client: connect to 127.0.0.1:4433 from ephemeral port 49155.
    let cli_sock = tcp::Socket::new(
        tcp::SocketBuffer::new(alloc::vec![0u8; 8192]),
        tcp::SocketBuffer::new(alloc::vec![0u8; 8192]),
    );
    let cli_h = sockets.add(cli_sock);
    {
        let sock = sockets.get_mut::<tcp::Socket>(cli_h);
        let cx = iface.context();
        sock.connect(cx, (IpAddress::v4(127, 0, 0, 1), 4433_u16), 49155_u16)
            .unwrap();
    }

    // ----------------------------------------------------------------
    // Generate ephemeral key pairs for both sides.
    // ----------------------------------------------------------------
    let mut rng = RdRand;

    let client_secret = EphemeralSecret::random(&mut rng);
    let client_pk = client_secret.public_key();

    let server_secret = EphemeralSecret::random(&mut rng);
    let server_pk = server_secret.public_key();

    let mut client_random = [0u8; 32];
    let mut server_random = [0u8; 32];
    rng.fill_bytes(&mut client_random);
    rng.fill_bytes(&mut server_random);

    // ----------------------------------------------------------------
    // Build ClientHello record bytes.
    // ----------------------------------------------------------------
    let ch_body = build_client_hello(&client_random, &client_pk);
    let mut ch_msg = Vec::new();
    write_handshake(&mut ch_msg, 0x01, &ch_body);
    let mut ch_record = Vec::new();
    write_record(&mut ch_record, CT_HANDSHAKE, &ch_msg);
    let ch_record_len = ch_record.len();

    // ----------------------------------------------------------------
    // Build ServerHello record bytes (prepared upfront; no network
    // needed to derive server keys in this demonstration).
    // ----------------------------------------------------------------
    let sh_body = build_server_hello(&server_random, &server_pk);
    let mut sh_msg = Vec::new();
    write_handshake(&mut sh_msg, 0x02, &sh_body);
    let mut sh_record = Vec::new();
    write_record(&mut sh_record, CT_HANDSHAKE, &sh_msg);
    let sh_record_len = sh_record.len();

    // ----------------------------------------------------------------
    // Poll loop.
    // ----------------------------------------------------------------
    let mut time_ms: i64 = 0;

    // Step 1: Poll until TCP connection is established.
    for _ in 0..300 {
        iface.poll(Instant::from_millis(time_ms), &mut device, &mut sockets);
        time_ms += 1;
        let cli_ok = sockets.get::<tcp::Socket>(cli_h).may_send();
        let srv_ok = sockets.get::<tcp::Socket>(srv_h).may_recv();
        if cli_ok && srv_ok {
            break;
        }
    }
    if !sockets.get::<tcp::Socket>(cli_h).may_send() {
        return Err("TLS/TCP: TCP connection not established within 300 iterations");
    }

    // Step 2: Client sends ClientHello via TCP.
    tcp_send_all(
        &mut iface,
        &mut device,
        &mut sockets,
        cli_h,
        &ch_record,
        &mut time_ms,
    )?;

    // Step 3: Server receives ClientHello bytes via TCP.
    let mut ch_recv = Vec::with_capacity(ch_record_len);
    tcp_recv_n(
        &mut iface,
        &mut device,
        &mut sockets,
        srv_h,
        ch_record_len,
        &mut ch_recv,
        &mut time_ms,
    )?;

    // Verify the bytes survived the TCP transit verbatim.
    if ch_recv != ch_record {
        return Err("TLS/TCP: ClientHello bytes corrupted in TCP transit");
    }

    // Parse the ClientHello to confirm it is structurally valid.
    let mut ch_pos = 0;
    let (ch_ct, ch_payload) = read_record(&ch_recv, &mut ch_pos)?;
    if ch_ct != CT_HANDSHAKE {
        return Err("TLS/TCP: expected CT_HANDSHAKE outer type in ClientHello");
    }
    if ch_payload.len() < 4 || ch_payload[0] != 0x01 {
        return Err("TLS/TCP: expected ClientHello handshake type");
    }
    let client_pk_parsed = extract_client_key_share(&ch_payload[4..])?;
    // Verify the round-tripped key matches the original.
    if client_pk_parsed.to_encoded_point(false).as_bytes()
        != client_pk.to_encoded_point(false).as_bytes()
    {
        return Err("TLS/TCP: client key share mismatch after TCP round-trip");
    }

    // Step 4: Server sends ServerHello via TCP.
    tcp_send_all(
        &mut iface,
        &mut device,
        &mut sockets,
        srv_h,
        &sh_record,
        &mut time_ms,
    )?;

    // Step 5: Client receives ServerHello bytes via TCP.
    let mut sh_recv = Vec::with_capacity(sh_record_len);
    tcp_recv_n(
        &mut iface,
        &mut device,
        &mut sockets,
        cli_h,
        sh_record_len,
        &mut sh_recv,
        &mut time_ms,
    )?;

    if sh_recv != sh_record {
        return Err("TLS/TCP: ServerHello bytes corrupted in TCP transit");
    }

    // Parse the ServerHello and verify the server's key share round-trips.
    let mut sh_pos = 0;
    let (sh_ct, sh_payload) = read_record(&sh_recv, &mut sh_pos)?;
    if sh_ct != CT_HANDSHAKE {
        return Err("TLS/TCP: expected CT_HANDSHAKE outer type in ServerHello");
    }
    if sh_payload.len() < 4 || sh_payload[0] != 0x02 {
        return Err("TLS/TCP: expected ServerHello handshake type");
    }
    let server_pk_parsed = extract_server_key_share(&sh_payload[4..])?;

    // Perform the ECDHE key exchange over the TCP-transported public keys
    // to verify the exchange material is intact end-to-end.
    let client_shared = client_secret.diffie_hellman(&server_pk_parsed);
    let server_shared = server_secret.diffie_hellman(&client_pk_parsed);
    let client_bytes: &[u8] = client_shared.raw_secret_bytes().as_ref();
    let server_bytes: &[u8] = server_shared.raw_secret_bytes().as_ref();
    if client_bytes != server_bytes {
        return Err("TLS/TCP: ECDHE shared secrets do not match — key exchange failed");
    }

    Ok(())
}
