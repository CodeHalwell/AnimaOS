//! Deterministic hashing used to make the fixture layer reproducible.
//!
//! The fixture [`crate::tuner::FixtureFineTuner`] and the [`crate::eval`] harness
//! must produce *byte-identical* results across machines, runs, and Rust
//! versions so tests and recorded CI scores stay stable. `std`'s `DefaultHasher`
//! is explicitly **not** guaranteed stable across releases, so we use a tiny,
//! self-contained [FNV-1a] implementation instead.
//!
//! These hashes are **not** cryptographic and are never used for security — only
//! for deterministic fingerprints and fixture scoring.
//!
//! [FNV-1a]: https://en.wikipedia.org/wiki/Fowler%E2%80%93Noll%E2%80%93Vo_hash_function

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// A small incremental FNV-1a 64-bit hasher with stable, documented behaviour.
#[derive(Debug, Clone, Copy)]
pub struct Fnv1a {
    state: u64,
}

impl Default for Fnv1a {
    fn default() -> Self {
        Fnv1a {
            state: FNV_OFFSET_BASIS,
        }
    }
}

impl Fnv1a {
    /// Start a fresh hasher.
    pub fn new() -> Self {
        Self::default()
    }

    /// Absorb raw bytes.
    pub fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.state ^= b as u64;
            self.state = self.state.wrapping_mul(FNV_PRIME);
        }
    }

    /// Absorb a length-prefixed field. The length prefix makes the hash
    /// unambiguous: `["ab", "c"]` and `["a", "bc"]` hash differently.
    pub fn write_field(&mut self, bytes: &[u8]) {
        self.write(&(bytes.len() as u64).to_le_bytes());
        self.write(bytes);
    }

    /// Absorb a length-prefixed string field.
    pub fn write_str(&mut self, s: &str) {
        self.write_field(s.as_bytes());
    }

    /// Absorb a `u64` field.
    pub fn write_u64(&mut self, v: u64) {
        self.write(&v.to_le_bytes());
    }

    /// Finish and return the 64-bit digest.
    pub fn finish(&self) -> u64 {
        self.state
    }
}

/// Hash a slice of bytes in one call.
pub fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut h = Fnv1a::new();
    h.write(bytes);
    h.finish()
}

/// Render a 64-bit digest as a stable lowercase 16-char hex string.
pub fn hex64(value: u64) -> String {
    format!("{value:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_fnv1a_vector() {
        // FNV-1a 64-bit of the empty string is the offset basis.
        assert_eq!(hash_bytes(b""), FNV_OFFSET_BASIS);
        // Well-known FNV-1a 64-bit reference vector for "a".
        assert_eq!(hash_bytes(b"a"), 0xaf63dc4c8601ec8c);
    }

    #[test]
    fn hashing_is_deterministic() {
        assert_eq!(hash_bytes(b"hello world"), hash_bytes(b"hello world"));
    }

    #[test]
    fn distinct_inputs_differ() {
        assert_ne!(hash_bytes(b"hello"), hash_bytes(b"world"));
    }

    #[test]
    fn length_prefix_disambiguates_fields() {
        let mut a = Fnv1a::new();
        a.write_str("ab");
        a.write_str("c");

        let mut b = Fnv1a::new();
        b.write_str("a");
        b.write_str("bc");

        assert_ne!(a.finish(), b.finish());
    }

    #[test]
    fn hex64_is_zero_padded_16_chars() {
        assert_eq!(hex64(0), "0000000000000000");
        assert_eq!(hex64(0xab), "00000000000000ab");
        assert_eq!(hex64(u64::MAX), "ffffffffffffffff");
    }
}
