//! Egress guard — E7 S7.0.2.
//!
//! Screens outbound URLs before tool execution to prevent SSRF, data
//! exfiltration via forbidden hosts, and accidental plain-HTTP use.
//!
//! # Threat model
//!
//! - **SSRF**: an adversarial tool payload triggers a request to an internal
//!   service (e.g. `169.254.169.254` cloud metadata, `127.0.0.1`, private
//!   subnets).  The guard rejects IP-literal hosts that resolve to private
//!   ranges at parse time; hostname-based SSRF is mitigated via the deny list.
//! - **Forbidden scheme**: only `https` is permitted by default; `http`,
//!   `file://`, `ftp://` etc. are rejected.
//! - **Blocklisted host**: operator-supplied explicit deny list.
//! - **Allow-list mode**: when [`EgressGuard::allowed_hosts`] is `Some`, only
//!   hosts on the list are permitted (defence-in-depth for constrained deploys).

use std::net::{Ipv4Addr, Ipv6Addr};

// ── Public types ──────────────────────────────────────────────────────────────

/// Verdict returned by [`EgressGuard::check_url`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressVerdict {
    /// The URL is permitted; proceed with the request.
    Allow,
    /// The URL is denied for the stated reason.
    Deny(EgressDenialReason),
}

impl EgressVerdict {
    /// `true` when the request is allowed.
    pub fn is_allowed(&self) -> bool {
        matches!(self, EgressVerdict::Allow)
    }

    /// `true` when the request is denied.
    pub fn is_denied(&self) -> bool {
        matches!(self, EgressVerdict::Deny(_))
    }

    /// Human-readable denial reason, or `None` if allowed.
    pub fn denial_reason_str(&self) -> Option<String> {
        match self {
            EgressVerdict::Allow => None,
            EgressVerdict::Deny(r) => Some(r.description()),
        }
    }
}

/// Specific reason an outbound request was denied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressDenialReason {
    /// URL scheme not in the allowed-scheme list (default: only `https`).
    ForbiddenScheme {
        /// The scheme that was rejected.
        scheme: String,
    },
    /// IP-literal host falls within a private, loopback, or reserved range
    /// (SSRF protection).
    SsrfPrivateAddress {
        /// The IP address string.
        address: String,
    },
    /// Host matches an entry in the operator-configured deny list.
    BlocklistedHost {
        /// The denied host.
        host: String,
    },
    /// Allow-list mode is active and this host is not on the list.
    HostNotAllowed {
        /// The host that was rejected.
        host: String,
    },
}

impl EgressDenialReason {
    /// One-line human-readable description.
    pub fn description(&self) -> String {
        match self {
            EgressDenialReason::ForbiddenScheme { scheme } => {
                format!("forbidden scheme {scheme:?}: only https is permitted")
            }
            EgressDenialReason::SsrfPrivateAddress { address } => {
                format!("SSRF protection: {address:?} is a private/loopback address")
            }
            EgressDenialReason::BlocklistedHost { host } => {
                format!("host {host:?} is on the egress blocklist")
            }
            EgressDenialReason::HostNotAllowed { host } => {
                format!("host {host:?} is not on the egress allow-list")
            }
        }
    }
}

/// Guards outbound network requests against SSRF and policy violations.
///
/// # Usage
///
/// ```rust
/// use actuators::egress::{EgressGuard, EgressVerdict};
///
/// let guard = EgressGuard::default();
/// assert!(guard.check_url("https://example.com/search").is_allowed());
/// assert!(guard.check_url("http://example.com/search").is_denied()); // http forbidden
/// assert!(guard.check_url("https://127.0.0.1/admin").is_denied());   // SSRF
/// ```
#[derive(Debug, Clone)]
pub struct EgressGuard {
    /// URL schemes that are permitted. Defaults to `["https"]`.
    pub allowed_schemes: Vec<String>,
    /// Hosts (exact match or suffix `.example.com`) that are unconditionally
    /// denied. Takes priority over `allowed_hosts`.
    pub blocklisted_hosts: Vec<String>,
    /// When `Some`, only these hosts are permitted. When `None`, all
    /// non-blocklisted, non-private hosts are permitted.
    pub allowed_hosts: Option<Vec<String>>,
}

impl Default for EgressGuard {
    fn default() -> Self {
        Self {
            allowed_schemes: vec!["https".to_string()],
            blocklisted_hosts: Vec::new(),
            allowed_hosts: None,
        }
    }
}

impl EgressGuard {
    /// Create a new guard with HTTPS-only policy and no host restrictions.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a host to the blocklist.
    pub fn with_blocklisted_host(mut self, host: impl Into<String>) -> Self {
        self.blocklisted_hosts.push(host.into());
        self
    }

    /// Restrict to exactly these hosts (allow-list mode).
    pub fn with_allowed_hosts(mut self, hosts: Vec<String>) -> Self {
        self.allowed_hosts = Some(hosts);
        self
    }

    /// Screen `url` against all configured policies.
    ///
    /// Returns [`EgressVerdict::Allow`] only when:
    /// 1. The scheme is in `allowed_schemes`.
    /// 2. The host is not a private/loopback IP address (SSRF protection).
    /// 3. The host is not on the blocklist.
    /// 4. If `allowed_hosts` is set, the host is on that list.
    pub fn check_url(&self, url: &str) -> EgressVerdict {
        let (scheme, host) = match parse_scheme_host(url) {
            Some(v) => v,
            None => {
                // Unparseable URL — deny to be safe.
                return EgressVerdict::Deny(EgressDenialReason::ForbiddenScheme {
                    scheme: "<unparseable>".to_string(),
                });
            }
        };

        // 1. Scheme check.
        if !self
            .allowed_schemes
            .iter()
            .any(|s| s.eq_ignore_ascii_case(&scheme))
        {
            return EgressVerdict::Deny(EgressDenialReason::ForbiddenScheme { scheme });
        }

        // 2. SSRF — IP literal check.
        let host_clean = strip_brackets(&host); // removes [ ] from IPv6 literals
        if let Some(reason) = ssrf_check(host_clean) {
            return EgressVerdict::Deny(reason);
        }

        // 3. Blocklist check (exact match or domain suffix).
        for blocked in &self.blocklisted_hosts {
            if host_matches(host_clean, blocked) {
                return EgressVerdict::Deny(EgressDenialReason::BlocklistedHost {
                    host: host_clean.to_string(),
                });
            }
        }

        // 4. Allow-list (if configured).
        if let Some(ref allow) = self.allowed_hosts {
            if !allow.iter().any(|a| host_matches(host_clean, a)) {
                return EgressVerdict::Deny(EgressDenialReason::HostNotAllowed {
                    host: host_clean.to_string(),
                });
            }
        }

        EgressVerdict::Allow
    }
}

// ── Parsing helpers ───────────────────────────────────────────────────────────

/// Extract `(scheme, host)` from a URL string.
///
/// Handles `scheme://host`, `scheme://host:port`, `scheme://[ipv6]:port`.
/// Returns `None` if the URL lacks a `://` separator.
fn parse_scheme_host(url: &str) -> Option<(String, String)> {
    let sep = url.find("://")?;
    let scheme = url[..sep].to_lowercase();
    let rest = &url[sep + 3..];
    // rest is `host/path`, `host:port/path`, `[ipv6]:port/path`, etc.
    // Take everything up to the first `/` or `?` or end-of-string.
    let authority = rest.split('/').next().unwrap_or(rest);
    let authority = authority.split('?').next().unwrap_or(authority);
    let authority = authority.split('#').next().unwrap_or(authority);
    // Strip userinfo (e.g. `user:pass@host`) to prevent bypass of SSRF/blocklist checks.
    let authority = if let Some(at) = authority.find('@') {
        &authority[at + 1..]
    } else {
        authority
    };
    // Strip port: for IPv6 literals `[::1]:8080` strip `:port` after `]`;
    // for plain hostnames/IPv4 strip last `:port`.
    let host = if authority.starts_with('[') {
        // IPv6 literal: `[addr]` or `[addr]:port`
        let end_bracket = authority
            .find(']')
            .map(|i| i + 1)
            .unwrap_or(authority.len());
        authority[..end_bracket].to_string()
    } else {
        // Remove port suffix if present (last colon, but only if it contains
        // digits after it — avoids stripping a bare IPv6 without brackets).
        if let Some(colon) = authority.rfind(':') {
            let after = &authority[colon + 1..];
            if after.chars().all(|c| c.is_ascii_digit()) {
                authority[..colon].to_string()
            } else {
                authority.to_string()
            }
        } else {
            authority.to_string()
        }
    };
    // Deny empty authority (e.g. `https:///path` has no host).
    if host.is_empty() {
        return None;
    }
    Some((scheme, host))
}

/// Remove `[` and `]` from IPv6 literal hosts.
fn strip_brackets(host: &str) -> &str {
    if host.starts_with('[') && host.ends_with(']') {
        &host[1..host.len() - 1]
    } else {
        host
    }
}

/// Returns `Some(EgressDenialReason::SsrfPrivateAddress)` if `host` is an
/// IP-literal address (in any encoding a resolver/`connect` would accept) that
/// falls in a private/loopback/reserved range.
///
/// Hardened against SSRF-bypass tricks that a naïve `Ipv4Addr::from_str` misses:
/// - the literal hostnames `localhost` / `*.localhost`,
/// - decimal-integer (`2130706433`), hex (`0x7f000001`), and octal (`0177.0.0.1`)
///   IPv4 encodings, which `libc` `getaddrinfo`/`inet_aton` happily resolve,
/// - IPv4-mapped / IPv4-compatible IPv6 (`::ffff:127.0.0.1`).
fn ssrf_check(host: &str) -> Option<EgressDenialReason> {
    let host = strip_brackets(host);

    // `localhost` and any `*.localhost` name resolve to loopback by convention
    // (RFC 6761) — reject before any other parsing.
    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".localhost") {
        return Some(EgressDenialReason::SsrfPrivateAddress {
            address: host.to_string(),
        });
    }

    // Try IPv6 first: a bracketed/colon-bearing host can only be IPv6.
    if host.contains(':') {
        if let Ok(addr) = host.parse::<Ipv6Addr>() {
            // IPv4-mapped (::ffff:a.b.c.d) and IPv4-compatible (::a.b.c.d):
            // extract the embedded v4 and classify it as v4.
            if let Some(v4) = embedded_ipv4(&addr) {
                if is_private_ipv4(&v4) {
                    return Some(EgressDenialReason::SsrfPrivateAddress {
                        address: host.to_string(),
                    });
                }
            }
            if is_private_ipv6(&addr) {
                return Some(EgressDenialReason::SsrfPrivateAddress {
                    address: host.to_string(),
                });
            }
        }
        return None;
    }

    // IPv4 — accept dotted-quad *and* the alternate encodings a resolver honours.
    if let Some(addr) = parse_ipv4_any(host) {
        if is_private_ipv4(&addr) {
            return Some(EgressDenialReason::SsrfPrivateAddress {
                address: host.to_string(),
            });
        }
    }
    None
}

/// Extract the embedded IPv4 address from an IPv4-mapped (`::ffff:a.b.c.d`) or
/// IPv4-compatible (`::a.b.c.d`) IPv6 address, if present.
fn embedded_ipv4(addr: &Ipv6Addr) -> Option<Ipv4Addr> {
    let s = addr.segments();
    // IPv4-mapped: ::ffff:a.b.c.d  → [0,0,0,0,0,0xffff,a.b, c.d]
    if s[0..5] == [0, 0, 0, 0, 0] && s[5] == 0xffff {
        return Some(Ipv4Addr::new(
            (s[6] >> 8) as u8,
            (s[6] & 0xff) as u8,
            (s[7] >> 8) as u8,
            (s[7] & 0xff) as u8,
        ));
    }
    // IPv4-compatible: ::a.b.c.d → [0,0,0,0,0,0,a.b,c.d] (deprecated but real).
    // Exclude :: and ::1 which are handled as native v6.
    if s[0..6] == [0, 0, 0, 0, 0, 0] && (s[6] != 0 || s[7] > 1) {
        return Some(Ipv4Addr::new(
            (s[6] >> 8) as u8,
            (s[6] & 0xff) as u8,
            (s[7] >> 8) as u8,
            (s[7] & 0xff) as u8,
        ));
    }
    None
}

/// Parse an IPv4 host in any encoding a libc resolver (`inet_aton`) would accept:
/// dotted quad, dotted with octal/hex octets, or a single decimal/octal/hex
/// integer. Returns `None` for genuine DNS hostnames (which the caller then
/// subjects to the deny/allow lists rather than SSRF classification).
fn parse_ipv4_any(host: &str) -> Option<Ipv4Addr> {
    // Fast path: the normal dotted quad.
    if let Ok(addr) = host.parse::<Ipv4Addr>() {
        return Some(addr);
    }

    // Each part must be a numeric literal (decimal / 0x-hex / 0-octal); a part
    // that fails to parse means this is not an all-numeric host (it's a DNS
    // name), so we bail out and let the deny/allow lists handle it.
    let parts: Vec<&str> = host.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut nums = Vec::with_capacity(parts.len());
    for p in &parts {
        nums.push(parse_numeric_part(p)?);
    }

    // inet_aton semantics: the final part absorbs the remaining low-order bytes.
    let value: u64 = match nums.len() {
        // a            → 32-bit value
        1 => nums[0],
        // a.b          → a.<24-bit b>
        2 => {
            if nums[0] > 0xff || nums[1] > 0x00ff_ffff {
                return None;
            }
            (nums[0] << 24) | nums[1]
        }
        // a.b.c        → a.b.<16-bit c>
        3 => {
            if nums[0] > 0xff || nums[1] > 0xff || nums[2] > 0xffff {
                return None;
            }
            (nums[0] << 24) | (nums[1] << 16) | nums[2]
        }
        // a.b.c.d      → standard quad
        4 => {
            if nums.iter().any(|&n| n > 0xff) {
                return None;
            }
            (nums[0] << 24) | (nums[1] << 16) | (nums[2] << 8) | nums[3]
        }
        _ => return None,
    };
    if value > u32::MAX as u64 {
        return None;
    }
    Some(Ipv4Addr::from(value as u32))
}

/// Parse a single IPv4 part as decimal, `0x`/`0X` hex, or leading-zero octal.
/// Returns `None` if the part is empty or contains non-numeric characters
/// (i.e. it's part of a DNS label, not an IP literal).
fn parse_numeric_part(p: &str) -> Option<u64> {
    if p.is_empty() {
        return None;
    }
    let lower = p.to_ascii_lowercase();
    if let Some(hex) = lower.strip_prefix("0x") {
        if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        u64::from_str_radix(hex, 16).ok()
    } else if p.len() > 1 && p.starts_with('0') {
        if !p.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
            return None;
        }
        u64::from_str_radix(p, 8).ok()
    } else {
        if !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        p.parse::<u64>().ok()
    }
}

/// Returns `true` for loopback, private, link-local, CGNAT, or
/// cloud-metadata IPv4 — i.e. any address that must never be reached by an
/// outbound tool request.
fn is_private_ipv4(addr: &Ipv4Addr) -> bool {
    let o = addr.octets();
    addr.is_loopback()              // 127.0.0.0/8
        || addr.is_private()        // 10/8, 172.16/12, 192.168/16
        || addr.is_link_local()     // 169.254.0.0/16
        || addr.is_unspecified()    // 0.0.0.0
        || addr.is_broadcast()      // 255.255.255.255
        // 0.0.0.0/8 ("this host" — routes to loopback on many stacks).
        || o[0] == 0
        // CGNAT / shared address space: 100.64.0.0/10.
        || (o[0] == 100 && (64..=127).contains(&o[1]))
        // Cloud metadata IP (169.254.169.254) is already covered by is_link_local,
        // but we call it out explicitly for clarity.
        || *addr == Ipv4Addr::new(169, 254, 169, 254)
}

/// Returns `true` for loopback, unspecified, link-local, or unique-local IPv6.
///
/// Covers:
/// - `::1` (loopback)
/// - `::` (unspecified)
/// - `fe80::/10` (link-local)
/// - `fc00::/7` (unique-local: `fc00::` and `fd00::` ranges)
fn is_private_ipv6(addr: &Ipv6Addr) -> bool {
    if addr.is_loopback() || addr.is_unspecified() {
        return true;
    }
    let segs = addr.segments();
    let first = segs[0];
    // Link-local: fe80::/10  (first 10 bits = 1111 1110 10)
    if first & 0xffc0 == 0xfe80 {
        return true;
    }
    // Unique-local: fc00::/7  (first 7 bits = 1111 110)
    if first & 0xfe00 == 0xfc00 {
        return true;
    }
    false
}

/// `true` when `host` equals `pattern` or is a subdomain of `.pattern`.
fn host_matches(host: &str, pattern: &str) -> bool {
    let h = host.trim_end_matches('.');
    let p = pattern.trim_start_matches('.').trim_end_matches('.');
    h.eq_ignore_ascii_case(p)
        || h.ends_with(&format!(".{p}"))
        || h.to_lowercase()
            .ends_with(&format!(".{}", p.to_lowercase()))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn guard() -> EgressGuard {
        EgressGuard::default()
    }

    // ── Scheme checks ─────────────────────────────────────────────────────────

    #[test]
    fn https_is_allowed() {
        assert!(guard().check_url("https://example.com/search").is_allowed());
    }

    #[test]
    fn http_is_denied_by_default() {
        let v = guard().check_url("http://example.com/search");
        assert!(v.is_denied());
        assert!(matches!(
            v,
            EgressVerdict::Deny(EgressDenialReason::ForbiddenScheme { .. })
        ));
    }

    #[test]
    fn file_scheme_is_denied() {
        assert!(guard().check_url("file:///etc/passwd").is_denied());
    }

    #[test]
    fn ftp_scheme_is_denied() {
        assert!(guard().check_url("ftp://example.com/file").is_denied());
    }

    // ── SSRF checks ───────────────────────────────────────────────────────────

    #[test]
    fn loopback_ipv4_is_denied() {
        assert!(guard().check_url("https://127.0.0.1/admin").is_denied());
    }

    #[test]
    fn private_class_a_is_denied() {
        assert!(guard().check_url("https://10.0.0.1/internal").is_denied());
    }

    #[test]
    fn private_class_b_is_denied() {
        assert!(guard().check_url("https://172.16.0.1/").is_denied());
    }

    #[test]
    fn private_class_c_is_denied() {
        assert!(guard().check_url("https://192.168.1.1/").is_denied());
    }

    #[test]
    fn cloud_metadata_ip_is_denied() {
        assert!(guard()
            .check_url("https://169.254.169.254/latest/meta-data/")
            .is_denied());
    }

    #[test]
    fn ipv6_loopback_is_denied() {
        assert!(guard().check_url("https://[::1]/").is_denied());
    }

    #[test]
    fn public_ip_is_allowed() {
        assert!(guard().check_url("https://8.8.8.8/").is_allowed());
    }

    // ── Blocklist checks ──────────────────────────────────────────────────────

    #[test]
    fn blocklisted_host_is_denied() {
        let g = EgressGuard::default().with_blocklisted_host("evil.example.com");
        assert!(g.check_url("https://evil.example.com/").is_denied());
    }

    #[test]
    fn non_blocklisted_host_is_allowed() {
        let g = EgressGuard::default().with_blocklisted_host("evil.example.com");
        assert!(g.check_url("https://good.example.com/").is_allowed());
    }

    #[test]
    fn subdomain_of_blocklisted_host_is_denied() {
        let g = EgressGuard::default().with_blocklisted_host("evil.com");
        assert!(g.check_url("https://sub.evil.com/").is_denied());
    }

    // ── Allow-list checks ─────────────────────────────────────────────────────

    #[test]
    fn allow_list_mode_permits_listed_host() {
        let g = EgressGuard::default().with_allowed_hosts(vec!["searxng.example.com".to_string()]);
        assert!(g
            .check_url("https://searxng.example.com/search")
            .is_allowed());
    }

    #[test]
    fn allow_list_mode_rejects_unlisted_host() {
        let g = EgressGuard::default().with_allowed_hosts(vec!["searxng.example.com".to_string()]);
        let v = g.check_url("https://other.example.com/");
        assert!(v.is_denied());
        assert!(matches!(
            v,
            EgressVerdict::Deny(EgressDenialReason::HostNotAllowed { .. })
        ));
    }

    // ── Parsing edge cases ────────────────────────────────────────────────────

    #[test]
    fn unparseable_url_is_denied() {
        assert!(guard().check_url("not-a-url").is_denied());
    }

    #[test]
    fn url_with_port_is_handled() {
        assert!(guard()
            .check_url("https://example.com:8443/search")
            .is_allowed());
        assert!(guard().check_url("https://127.0.0.1:8080/").is_denied());
    }

    #[test]
    fn denial_reason_includes_description() {
        let v = guard().check_url("https://127.0.0.1/");
        if let EgressVerdict::Deny(reason) = v {
            let desc = reason.description();
            assert!(desc.contains("SSRF") || desc.contains("private") || desc.contains("loopback"));
        } else {
            panic!("expected denial");
        }
    }

    // ── Userinfo bypass (regression: CVE-class SSRF) ──────────────────────────

    #[test]
    fn userinfo_at_sign_does_not_bypass_ssrf_check() {
        // https://user:pass@127.0.0.1/ — real host is 127.0.0.1 (loopback).
        assert!(guard()
            .check_url("https://user:pass@127.0.0.1/")
            .is_denied());
    }

    #[test]
    fn userinfo_does_not_bypass_blocklist() {
        let g = EgressGuard::default().with_blocklisted_host("evil.com");
        // https://good.com@evil.com/ — real host is evil.com.
        assert!(g.check_url("https://good.com@evil.com/").is_denied());
    }

    // ── IPv6 private ranges ───────────────────────────────────────────────────

    #[test]
    fn ipv6_unique_local_fc_is_denied() {
        assert!(guard().check_url("https://[fc00::1]/").is_denied());
    }

    #[test]
    fn ipv6_unique_local_fd_is_denied() {
        assert!(guard()
            .check_url("https://[fd12:3456:789a::1]/")
            .is_denied());
    }

    #[test]
    fn ipv6_link_local_is_denied() {
        assert!(guard().check_url("https://[fe80::1]/").is_denied());
    }

    #[test]
    fn ipv6_public_is_allowed() {
        // 2001:4860:4860::8888 is Google's public DNS — should be allowed.
        assert!(guard()
            .check_url("https://[2001:4860:4860::8888]/")
            .is_allowed());
    }

    // ── SSRF bypass hardening (decimal/hex/octal/IPv4-mapped/localhost) ────────

    #[test]
    fn localhost_hostname_is_denied() {
        assert!(guard().check_url("https://localhost/admin").is_denied());
        assert!(guard().check_url("https://LOCALHOST/").is_denied());
        assert!(guard().check_url("https://foo.localhost/").is_denied());
    }

    #[test]
    fn decimal_encoded_loopback_is_denied() {
        // 2130706433 == 127.0.0.1
        assert!(guard().check_url("https://2130706433/").is_denied());
    }

    #[test]
    fn hex_encoded_loopback_is_denied() {
        // 0x7f000001 == 127.0.0.1
        assert!(guard().check_url("https://0x7f000001/").is_denied());
    }

    #[test]
    fn octal_encoded_loopback_is_denied() {
        // 0177.0.0.1 == 127.0.0.1
        assert!(guard().check_url("https://0177.0.0.1/").is_denied());
    }

    #[test]
    fn decimal_encoded_metadata_is_denied() {
        // 2852039166 == 169.254.169.254
        assert!(guard().check_url("https://2852039166/").is_denied());
    }

    #[test]
    fn ipv4_mapped_ipv6_loopback_is_denied() {
        assert!(guard().check_url("https://[::ffff:127.0.0.1]/").is_denied());
    }

    #[test]
    fn ipv4_mapped_ipv6_metadata_is_denied() {
        assert!(guard()
            .check_url("https://[::ffff:169.254.169.254]/")
            .is_denied());
    }

    #[test]
    fn ipv4_compatible_ipv6_loopback_is_denied() {
        assert!(guard().check_url("https://[::127.0.0.1]/").is_denied());
    }

    #[test]
    fn ipv6_unspecified_is_denied() {
        assert!(guard().check_url("https://[::]/").is_denied());
    }

    #[test]
    fn ipv6_explicit_loopback_is_denied() {
        assert!(guard().check_url("https://[::1]/").is_denied());
    }

    #[test]
    fn cgnat_range_is_denied() {
        assert!(guard().check_url("https://100.64.0.1/").is_denied());
        assert!(guard().check_url("https://100.127.255.254/").is_denied());
    }

    #[test]
    fn zero_prefix_is_denied() {
        assert!(guard().check_url("https://0.0.0.0/").is_denied());
        assert!(guard().check_url("https://0.1.2.3/").is_denied());
    }

    #[test]
    fn public_decimal_quad_still_allowed() {
        // Normal public dotted quad and a public hostname must still pass.
        assert!(guard().check_url("https://8.8.8.8/").is_allowed());
        assert!(guard().check_url("https://93.184.216.34/").is_allowed());
        assert!(guard().check_url("https://example.com/").is_allowed());
    }

    #[test]
    fn public_ipv4_mapped_ipv6_is_allowed() {
        // ::ffff:8.8.8.8 maps to a public address.
        assert!(guard().check_url("https://[::ffff:8.8.8.8]/").is_allowed());
    }

    #[test]
    fn numeric_looking_hostname_is_not_misclassified() {
        // A DNS label that merely starts with a digit is not an IP literal and
        // must not be treated as private.
        assert!(guard().check_url("https://1host.example.com/").is_allowed());
    }
}
