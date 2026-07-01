//! Charter parsing and tamper-evidence (S13.1).
//!
//! The charter is the signed, read-only value document.  At runtime the agent
//! can read it but never write it.  Tamper-evidence is provided by an
//! HMAC-SHA256 chain over the JSON-serialised `core` + `operator` sections,
//! using the same construction as the audit-log sidecar (EX.4 / threat T-8).
//!
//! # Trust model
//!
//! - An empty `meta.hmac_hex` (trust-on-first-use) is accepted with a warning.
//! - A non-empty `meta.hmac_hex` must match; mismatch returns
//!   [`CharterError::HmacMismatch`].
//! - The embedded default charter ships with an empty HMAC and is treated as
//!   the authoritative baseline until the operator seals it.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ── Public types ──────────────────────────────────────────────────────────────

/// A single inviolable prohibition in the core charter layer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Prohibition {
    /// Stable identifier (e.g. `"P1"`).
    pub id: String,
    /// Human-readable prohibition text.
    pub text: String,
    /// Keywords that trigger this prohibition in [`crate::ConstitutionCheck`].
    #[serde(default)]
    pub keywords: Vec<String>,
}

/// Per-drive ceiling in the core layer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DriveBound {
    /// Drive name (e.g. `"achievement"`, `"curiosity"`, `"autonomy"`).
    pub drive: String,
    /// Maximum normalised value [0.0, 1.0] the drive may reach.
    pub hard_ceiling: f64,
    /// Rationale for the ceiling.
    pub rationale: String,
}

/// The immutable core layer of the charter.
///
/// This layer is fixed at build time.  It states the agent's purpose,
/// inviolable prohibitions, and corrigibility commitment.  It cannot be
/// relaxed by the operator layer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CoreLayer {
    /// Monotonic version of the core layer schema.
    pub version: u32,
    /// One-paragraph statement of the agent's purpose.
    pub purpose: String,
    /// Corrigibility commitment (always honoured, regardless of drives).
    pub corrigibility: String,
    /// Ordered list of inviolable prohibitions.
    pub prohibitions: Vec<Prohibition>,
    /// Per-drive hard ceilings.
    #[serde(default)]
    pub drive_bounds: Vec<DriveBound>,
}

/// The operator layer of the charter.
///
/// Seeded at E9 onboarding.  May add additional bounds, but may never relax
/// or override a core prohibition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperatorLayer {
    /// Schema version for the operator layer.
    pub version: u32,
    /// Stable agent identifier this charter is bound to.
    pub agent_id: String,
    /// Operator priority level.
    #[serde(default)]
    pub priority: OperatorPriority,
    /// Additional operator-specific prohibitions or bounds (text).
    #[serde(default)]
    pub additional_bounds: Vec<String>,
}

/// Priority level of the operator layer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum OperatorPriority {
    /// Standard operator authority.
    #[default]
    Default,
    /// Elevated operator authority (allows broader override scope).
    Elevated,
}

/// Metadata section of the charter file.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CharterMeta {
    charter_version: u32,
    hmac_hex: String,
}

/// The complete charter document.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CharterFile {
    core: CoreLayer,
    operator: OperatorLayer,
    #[serde(default)]
    meta: CharterMeta,
}

/// The parsed, verified charter.
#[derive(Debug, Clone)]
pub struct Charter {
    /// Core layer (purpose, prohibitions, drive bounds).
    pub core: CoreLayer,
    /// Operator layer (additional bounds, agent ID).
    pub operator: OperatorLayer,
    /// Whether HMAC was present and verified.
    pub hmac_verified: bool,
}

/// Errors produced when loading or verifying a charter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CharterError {
    /// TOML parse error.
    ParseError(String),
    /// HMAC verification failed (charter may have been tampered with).
    HmacMismatch,
    /// The charter carries no HMAC seal (trust-on-first-use) but a strict load
    /// was requested (production must run a sealed charter). See
    /// [`Charter::from_path_strict`] (AUT-2).
    Unsealed,
    /// JSON serialisation of content for HMAC computation failed.
    SerialisationError(String),
}

impl std::fmt::Display for CharterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ParseError(e) => write!(f, "charter parse error: {e}"),
            Self::HmacMismatch => write!(f, "charter HMAC mismatch — possible tampering"),
            Self::Unsealed => write!(
                f,
                "charter is unsealed (no HMAC) — run `anima constitution seal` before production use"
            ),
            Self::SerialisationError(e) => write!(f, "charter serialisation error: {e}"),
        }
    }
}

impl std::error::Error for CharterError {}

// ── Charter implementation ────────────────────────────────────────────────────

/// The default charter TOML embedded at compile time.
pub const EMBEDDED_CHARTER: &str = include_str!("../constitution.toml");

impl Charter {
    /// Load the charter embedded at compile time.
    ///
    /// This is the primary constructor for the hosted kernel.  The embedded
    /// charter ships with an empty HMAC (trust-on-first-use); operators seal
    /// it with `anima constitution seal` in production.
    pub fn embedded() -> Result<Self, CharterError> {
        Self::from_toml_str(EMBEDDED_CHARTER, None)
    }

    /// Parse a charter from a TOML string, optionally verifying the HMAC.
    ///
    /// If `hmac_key` is `None` and `meta.hmac_hex` is non-empty, the HMAC is
    /// still checked using the `ANIMA_CONSTITUTION_HMAC_KEY` environment
    /// variable.  If neither is available, an empty `hmac_hex` is accepted
    /// (trust-on-first-use); a non-empty `hmac_hex` with no key is rejected.
    pub fn from_toml_str(toml: &str, hmac_key: Option<&[u8]>) -> Result<Self, CharterError> {
        let file: CharterFile =
            ::toml::from_str(toml).map_err(|e| CharterError::ParseError(e.to_string()))?;

        let hmac_verified = verify_hmac(&file, hmac_key)?;

        Ok(Self {
            core: file.core,
            operator: file.operator,
            hmac_verified,
        })
    }

    /// Load a charter from a file path.
    ///
    /// Loads permissively (trust-on-first-use for an unsealed charter) but emits
    /// a loud warning to stderr when the loaded charter carries no HMAC seal, so
    /// an unsealed production charter cannot pass unnoticed (AUT-2). Use
    /// [`Charter::from_path_strict`] to fail closed instead.
    pub fn from_path(path: &std::path::Path) -> Result<Self, CharterError> {
        let toml =
            std::fs::read_to_string(path).map_err(|e| CharterError::ParseError(e.to_string()))?;
        let charter = Self::from_toml_str(&toml, None)?;
        if !charter.hmac_verified {
            eprintln!(
                "anima-constitution: WARNING — charter at {} is UNSEALED (no HMAC); \
                 anyone who can edit it can weaken prohibitions undetected. \
                 Run `anima constitution seal` before production use.",
                path.display()
            );
        }
        Ok(charter)
    }

    /// Load a charter from a file path in **strict** mode: an unsealed
    /// (trust-on-first-use) charter is rejected with [`CharterError::Unsealed`]
    /// rather than accepted. Use this on production boot paths where the charter
    /// must carry a verified HMAC seal (AUT-2).
    pub fn from_path_strict(path: &std::path::Path) -> Result<Self, CharterError> {
        let charter = Self::from_path(path)?;
        if !charter.hmac_verified {
            return Err(CharterError::Unsealed);
        }
        Ok(charter)
    }

    /// Whether this charter was loaded with a present and verified HMAC seal.
    pub fn is_sealed(&self) -> bool {
        self.hmac_verified
    }

    /// Compute the canonical HMAC for this charter's content.
    ///
    /// The HMAC is over the JSON serialisation of `core` + `operator`, keyed
    /// with the provided key.  This is the same value that would be stored in
    /// `meta.hmac_hex` after `anima constitution seal`.
    pub fn compute_hmac(&self, key: &[u8]) -> Result<String, CharterError> {
        let payload = canonical_payload(&self.core, &self.operator)?;
        let mac = hmac_sha256(key, &[&payload]);
        Ok(to_hex(&mac))
    }

    /// Returns all prohibitions from both core and operator layers.
    pub fn all_prohibitions(&self) -> impl Iterator<Item = &Prohibition> {
        self.core.prohibitions.iter()
    }

    /// Returns the corrigibility commitment text.
    pub fn corrigibility_text(&self) -> &str {
        &self.core.corrigibility
    }
}

// ── HMAC helpers ─────────────────────────────────────────────────────────────

/// JSON-serialise core + operator in a canonical, deterministic form.
fn canonical_payload(core: &CoreLayer, operator: &OperatorLayer) -> Result<Vec<u8>, CharterError> {
    #[derive(Serialize)]
    struct Payload<'a> {
        core: &'a CoreLayer,
        operator: &'a OperatorLayer,
    }
    serde_json::to_vec(&Payload { core, operator })
        .map_err(|e| CharterError::SerialisationError(e.to_string()))
}

/// Verify the HMAC stored in `file.meta.hmac_hex`.
///
/// Returns:
/// - `Ok(true)` when HMAC was present and verified.
/// - `Ok(false)` when HMAC was absent (empty string) — trust-on-first-use.
/// - `Err(CharterError::HmacMismatch)` when HMAC was present but incorrect.
fn verify_hmac(file: &CharterFile, explicit_key: Option<&[u8]>) -> Result<bool, CharterError> {
    let stored = &file.meta.hmac_hex;
    if stored.is_empty() {
        return Ok(false);
    }

    // Resolve the HMAC key: explicit arg wins, then env var.
    let env_key_bytes;
    let key: &[u8] = if let Some(k) = explicit_key {
        k
    } else if let Ok(k) = std::env::var("ANIMA_CONSTITUTION_HMAC_KEY") {
        env_key_bytes = k.into_bytes();
        &env_key_bytes
    } else {
        // Non-empty HMAC with no key available — reject.
        return Err(CharterError::HmacMismatch);
    };

    let payload = canonical_payload(&file.core, &file.operator)?;
    let expected = to_hex(&hmac_sha256(key, &[&payload]));

    // Constant-time comparison so a timing side-channel cannot be used to forge
    // the seal byte-by-byte (AUT-2).
    if ct_str_eq(&expected, stored) {
        Ok(true)
    } else {
        Err(CharterError::HmacMismatch)
    }
}

/// Constant-time string equality over the compared bytes. The length is
/// revealed (the HMAC hex is always 64 chars), but the content comparison does
/// not short-circuit on the first differing byte.
fn ct_str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// HMAC-SHA256 — same construction as vita/audit.rs (RFC 2104).
fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
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
    for p in parts {
        inner.update(p);
    }
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_digest);

    let mut mac = [0u8; 32];
    mac.copy_from_slice(&outer.finalize());
    mac
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_charter_parses_without_error() {
        let c = Charter::embedded().expect("embedded charter must parse");
        assert_eq!(c.core.version, 1);
        assert!(!c.core.prohibitions.is_empty());
        assert!(!c.core.purpose.is_empty());
        assert!(!c.core.corrigibility.is_empty());
    }

    #[test]
    fn embedded_charter_has_all_eight_prohibitions() {
        let c = Charter::embedded().unwrap();
        let ids: Vec<&str> = c.core.prohibitions.iter().map(|p| p.id.as_str()).collect();
        for expected in ["P1", "P2", "P3", "P4", "P5", "P6", "P7", "P8"] {
            assert!(ids.contains(&expected), "missing prohibition {expected}");
        }
    }

    #[test]
    fn charter_hmac_not_verified_when_hmac_hex_is_empty() {
        let c = Charter::embedded().unwrap();
        // The embedded charter ships with an empty hmac_hex.
        assert!(
            !c.hmac_verified,
            "embedded default should be trust-on-first-use"
        );
    }

    #[test]
    fn charter_hmac_verified_when_key_and_hex_match() {
        let c = Charter::embedded().unwrap();
        let key = b"test-key-for-hmac-verification";
        let hex = c.compute_hmac(key).unwrap();

        let toml_with_hmac = format!(
            "{}\n\n[meta]\ncharter_version = 1\nhmac_hex = \"{hex}\"",
            &EMBEDDED_CHARTER[..EMBEDDED_CHARTER
                .rfind("[meta]")
                .unwrap_or(EMBEDDED_CHARTER.len())]
                .trim()
        );

        let result = Charter::from_toml_str(&toml_with_hmac, Some(key));
        assert!(
            result.is_ok(),
            "valid HMAC should verify: {:?}",
            result.err()
        );
        assert!(result.unwrap().hmac_verified);
    }

    #[test]
    fn charter_hmac_rejects_tampered_content() {
        let c = Charter::embedded().unwrap();
        let key = b"test-key-for-hmac-verification";
        let hex = c.compute_hmac(key).unwrap();

        // Inject a tampered purpose into a fresh TOML (prohibitions = [] satisfies
        // the required field; the HMAC will mismatch because content differs).
        let tampered = format!(
            "[core]\nversion = 1\npurpose = \"TAMPERED\"\ncorrigibility = \"x\"\n\
             prohibitions = []\n\
             [operator]\nversion = 1\nagent_id = \"anima\"\n\
             [meta]\ncharter_version = 1\nhmac_hex = \"{hex}\""
        );
        let result = Charter::from_toml_str(&tampered, Some(key));
        assert!(
            matches!(result, Err(CharterError::HmacMismatch)),
            "tampered charter must be rejected"
        );
    }

    #[test]
    fn compute_hmac_is_deterministic() {
        let c = Charter::embedded().unwrap();
        let key = b"determinism-key";
        let h1 = c.compute_hmac(key).unwrap();
        let h2 = c.compute_hmac(key).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn core_drive_bounds_are_present() {
        let c = Charter::embedded().unwrap();
        assert!(!c.core.drive_bounds.is_empty());
        for bound in &c.core.drive_bounds {
            assert!(
                (0.0..=1.0).contains(&bound.hard_ceiling),
                "drive ceiling must be in [0.0, 1.0]"
            );
        }
    }

    #[test]
    fn is_sealed_reflects_hmac_state() {
        assert!(
            !Charter::embedded().unwrap().is_sealed(),
            "embedded default is unsealed"
        );
        let c = Charter::embedded().unwrap();
        let key = b"seal-key";
        let hex = c.compute_hmac(key).unwrap();
        let sealed_toml = format!(
            "{}\n\n[meta]\ncharter_version = 1\nhmac_hex = \"{hex}\"",
            &EMBEDDED_CHARTER[..EMBEDDED_CHARTER
                .rfind("[meta]")
                .unwrap_or(EMBEDDED_CHARTER.len())]
                .trim()
        );
        assert!(Charter::from_toml_str(&sealed_toml, Some(key))
            .unwrap()
            .is_sealed());
    }

    #[test]
    fn from_path_strict_rejects_unsealed_charter() {
        // The embedded charter ships unsealed; a strict load must fail closed
        // while the permissive load still succeeds (with a stderr warning).
        let path = std::env::temp_dir().join(format!(
            "anima-test-charter-strict-{}.toml",
            std::process::id()
        ));
        std::fs::write(&path, EMBEDDED_CHARTER).unwrap();
        let strict = Charter::from_path_strict(&path);
        let lax = Charter::from_path(&path);
        let _ = std::fs::remove_file(&path);
        assert!(matches!(strict, Err(CharterError::Unsealed)));
        assert!(lax.is_ok());
    }

    #[test]
    fn ct_str_eq_matches_std_equality() {
        assert!(ct_str_eq("abc123", "abc123"));
        assert!(!ct_str_eq("abc123", "abc124"));
        assert!(!ct_str_eq("abc", "abcd"));
    }
}
