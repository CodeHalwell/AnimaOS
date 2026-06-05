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
    // Strip port: for IPv6 literals `[::1]:8080` strip `:port` after `]`;
    // for plain hostnames/IPv4 strip last `:port`.
    let host = if authority.starts_with('[') {
        // IPv6 literal: `[addr]` or `[addr]:port`
        let end_bracket = authority.find(']').map(|i| i + 1).unwrap_or(authority.len());
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
/// IP-literal address that falls in a private/loopback/reserved range.
fn ssrf_check(host: &str) -> Option<EgressDenialReason> {
    // Try IPv4
    if let Ok(addr) = host.parse::<Ipv4Addr>() {
        if is_private_ipv4(&addr) {
            return Some(EgressDenialReason::SsrfPrivateAddress {
                address: host.to_string(),
            });
        }
    }
    // Try IPv6
    if let Ok(addr) = host.parse::<Ipv6Addr>() {
        if addr.is_loopback() || addr.is_unspecified() {
            return Some(EgressDenialReason::SsrfPrivateAddress {
                address: host.to_string(),
            });
        }
    }
    None
}

/// Returns `true` for loopback, private, link-local, or cloud-metadata IPv4.
fn is_private_ipv4(addr: &Ipv4Addr) -> bool {
    addr.is_loopback()
        || addr.is_private()
        || addr.is_link_local()
        || addr.is_unspecified()
        || addr.is_broadcast()
        // Cloud metadata IP (169.254.169.254) is already covered by is_link_local,
        // but we call it out explicitly for clarity.
        || *addr == Ipv4Addr::new(169, 254, 169, 254)
}

/// `true` when `host` equals `pattern` or is a subdomain of `.pattern`.
fn host_matches(host: &str, pattern: &str) -> bool {
    let h = host.trim_end_matches('.');
    let p = pattern.trim_start_matches('.').trim_end_matches('.');
    h.eq_ignore_ascii_case(p)
        || h.ends_with(&format!(".{p}"))
        || h.to_lowercase().ends_with(&format!(".{}", p.to_lowercase()))
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
        assert!(matches!(v, EgressVerdict::Deny(EgressDenialReason::ForbiddenScheme { .. })));
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
        assert!(guard().check_url("https://169.254.169.254/latest/meta-data/").is_denied());
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
        let g = EgressGuard::default()
            .with_allowed_hosts(vec!["searxng.example.com".to_string()]);
        assert!(g.check_url("https://searxng.example.com/search").is_allowed());
    }

    #[test]
    fn allow_list_mode_rejects_unlisted_host() {
        let g = EgressGuard::default()
            .with_allowed_hosts(vec!["searxng.example.com".to_string()]);
        let v = g.check_url("https://other.example.com/");
        assert!(v.is_denied());
        assert!(matches!(v, EgressVerdict::Deny(EgressDenialReason::HostNotAllowed { .. })));
    }

    // ── Parsing edge cases ────────────────────────────────────────────────────

    #[test]
    fn unparseable_url_is_denied() {
        assert!(guard().check_url("not-a-url").is_denied());
    }

    #[test]
    fn url_with_port_is_handled() {
        assert!(guard().check_url("https://example.com:8443/search").is_allowed());
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
}
