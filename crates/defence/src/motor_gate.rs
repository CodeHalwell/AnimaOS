//! Unsafe motor action gate (S5.6.4).
//!
//! Screens filesystem operations, outbound network requests, and self-modification
//! attempts for safety.  Integrates with the `anima-self` object-capability
//! system (E1.2) to enforce least-privilege access on critical resources.
//!
//! # Design
//!
//! The gate is purely static: it applies configurable path and host lists
//! without any learned component.  This is intentional — the gate is a
//! hard safety boundary, not a value-sensitive policy, and interpretability
//! matters more than expressiveness here.

use std::net::{IpAddr, Ipv4Addr};

use anima_self::{Capability, Verified};

use crate::types::{VetoReason, VetoResult};

// ── Default critical path prefixes ────────────────────────────────────────────

/// Filesystem path prefixes that are treated as critical by default.
///
/// Write, delete, move, rename, chmod, and chown operations targeting paths
/// under these prefixes require a `Capability<Verified>` with capability name
/// `"motor.filesystem.critical"` or `"motor.filesystem.*"`.
pub const DEFAULT_CRITICAL_PREFIXES: &[&str] = &[
    "/etc",
    "/boot",
    "/sys",
    "/proc",
    "/dev",
    "/usr/lib",
    "/usr/bin",
    "/usr/sbin",
    "/sbin",
    "/bin",
    "/lib",
    "/lib64",
];

/// Operation names that constitute write-class filesystem actions.
const WRITE_OPS: &[&str] = &[
    "write",
    "delete",
    "remove",
    "move",
    "rename",
    "chmod",
    "chown",
    "truncate",
    "overwrite",
    "append",
];

// ── UnsafeMotorActionGate ─────────────────────────────────────────────────────

/// Unsafe motor action gate (S5.6.4).
///
/// Screens three categories of motor action:
/// 1. Filesystem operations — write-class ops on critical-path prefixes require
///    a `motor.filesystem.critical` or `motor.filesystem.*` capability.
/// 2. Network requests — requests to blocklisted hosts are unconditionally
///    blocked.
/// 3. Self-modification — blocked by default; requires `allow_self_modification`
///    **and** a `self.modify` or `self.*` capability.
#[derive(Debug, Clone)]
pub struct UnsafeMotorActionGate {
    /// Critical filesystem path prefixes.
    pub critical_prefixes: Vec<String>,
    /// Blocklisted host strings (URL substring match).
    pub blocklisted_hosts: Vec<String>,
    /// Whether self-modification is allowed at all (default: `false`).
    ///
    /// Even when `true`, a verified `self.modify` capability is required.
    pub allow_self_modification: bool,
}

impl UnsafeMotorActionGate {
    /// Creates a gate with the default critical prefixes and an empty blocklist.
    pub fn new() -> Self {
        Self {
            critical_prefixes: DEFAULT_CRITICAL_PREFIXES
                .iter()
                .map(|s| s.to_string())
                .collect(),
            blocklisted_hosts: Vec::new(),
            allow_self_modification: false,
        }
    }

    /// Appends a custom critical-path prefix.
    pub fn with_critical_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.critical_prefixes.push(prefix.into());
        self
    }

    /// Appends a blocklisted host (matched as a URL substring).
    pub fn with_blocklisted_host(mut self, host: impl Into<String>) -> Self {
        self.blocklisted_hosts.push(host.into());
        self
    }

    // ── Filesystem screening ──────────────────────────────────────────────────

    /// Screens a filesystem operation.
    ///
    /// Read-class operations are always permitted.  Write-class operations
    /// targeting critical-path prefixes require `capability` to be
    /// `Some(cap)` with `cap.capability` matching `"motor.filesystem.critical"`
    /// or `"motor.filesystem.*"`.
    ///
    /// # Parameters
    ///
    /// - `operation` — one of `"read"`, `"write"`, `"delete"`, `"move"`, …
    /// - `path` — the target filesystem path.
    /// - `capability` — optional verified capability from `anima-self`.
    pub fn screen_filesystem(
        &self,
        operation: &str,
        path: &str,
        capability: Option<&Capability<Verified>>,
    ) -> VetoResult {
        let op_lower = operation.to_ascii_lowercase();
        let is_write = WRITE_OPS.iter().any(|&w| op_lower == w);

        if !is_write {
            return VetoResult::Allow;
        }

        // Resolve `.`/`..` lexically and match on component boundaries so
        // `/var/../etc/passwd` and `./etc/passwd` cannot dodge the critical
        // prefixes, and `/etcd` does not falsely match `/etc` (MEM-1).
        let normalized = normalize_path(path);
        let targets_critical = self
            .critical_prefixes
            .iter()
            .any(|prefix| path_under_prefix(&normalized, prefix.as_str()));

        if !targets_critical {
            return VetoResult::Allow;
        }

        // Write to a critical path — capability required.
        if let Some(cap) = capability {
            if matches!(
                cap.capability,
                "motor.filesystem.critical" | "motor.filesystem.*"
            ) {
                return VetoResult::Allow;
            }
        }

        VetoResult::Veto(VetoReason::UnsafeMotorAction {
            action: format!("{operation} {path}"),
            policy: "motor.filesystem.critical".to_string(),
        })
    }

    // ── Network screening ─────────────────────────────────────────────────────

    /// Screens an outbound network request.
    ///
    /// Requests to blocklisted hosts (matched as URL substrings) are always
    /// blocked; all other requests are allowed.
    pub fn screen_network(&self, url: &str, method: &str) -> VetoResult {
        let host = extract_host(url);

        // Fail-closed SSRF default: a cloud-isolated agent must never reach
        // loopback, private/link-local ranges, or the cloud metadata endpoint,
        // regardless of the configured blocklist (MEM-6).
        if is_private_or_metadata_host(&host) {
            return VetoResult::Veto(VetoReason::UnsafeMotorAction {
                action: format!("{method} {url}"),
                policy: "ssrf-private-address".to_string(),
            });
        }

        for blocked in &self.blocklisted_hosts {
            // Match on the host component (equal or a subdomain) rather than a
            // raw URL substring, so `evil.example.com` blocks
            // `cdn.evil.example.com` but not `notevil.example.com`, and a
            // `10.0.0.1` entry no longer also blocks `10.0.0.10` (MEM-6).
            if host_matches_block(&host, blocked) {
                return VetoResult::Veto(VetoReason::UnsafeMotorAction {
                    action: format!("{method} {url}"),
                    policy: format!("blocklisted-host:{blocked}"),
                });
            }
        }
        VetoResult::Allow
    }

    // ── Self-modification screening ───────────────────────────────────────────

    /// Screens a self-modification attempt.
    ///
    /// Blocked unless **both**:
    /// 1. `self.allow_self_modification` is `true`, and
    /// 2. `capability` is `Some(cap)` with `cap.capability` matching
    ///    `"self.modify"` or `"self.*"`.
    ///
    /// # Parameters
    ///
    /// - `target` — what is being modified (e.g. `"config/routes.toml"`).
    /// - `change` — human-readable description of the proposed change.
    /// - `capability` — optional verified capability.
    pub fn screen_self_modification(
        &self,
        target: &str,
        change: &str,
        capability: Option<&Capability<Verified>>,
    ) -> VetoResult {
        if self.allow_self_modification {
            if let Some(cap) = capability {
                if matches!(cap.capability, "self.modify" | "self.*") {
                    return VetoResult::Allow;
                }
            }
        }

        VetoResult::Veto(VetoReason::UnsafeMotorAction {
            action: format!("self-modification of {target}: {change}"),
            policy: "self.modify".to_string(),
        })
    }
}

// ── Path / host normalisation helpers ─────────────────────────────────────────

/// Lexically resolves `.` and `..` segments in `path` without touching the
/// filesystem, so `/var/../etc/passwd` and `./etc/passwd` both normalise under
/// `/etc`. The result is always treated as absolute (fail-closed): a relative
/// path is rooted at `/` so it cannot dodge a critical prefix (MEM-1).
fn normalize_path(path: &str) -> String {
    let mut stack: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            s => stack.push(s),
        }
    }
    let mut out = String::with_capacity(path.len() + 1);
    out.push('/');
    out.push_str(&stack.join("/"));
    out
}

/// Returns `true` when `path` lies at or under `prefix` on a path-component
/// boundary, so `/etc` matches `/etc` and `/etc/passwd` but not `/etcd`.
fn path_under_prefix(path: &str, prefix: &str) -> bool {
    let prefix = prefix.trim_end_matches('/');
    match path.strip_prefix(prefix) {
        Some(rest) => rest.is_empty() || rest.starts_with('/'),
        None => false,
    }
}

/// Extracts the lowercase host component from a URL, stripping scheme,
/// userinfo, port, and path. Best-effort: input without a scheme is treated as
/// a bare authority. IPv6 literals keep their inner address (`[::1]:8080` → `::1`).
fn extract_host(url: &str) -> String {
    let after_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    // Strip any `user:pass@` userinfo.
    let host_port = authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority);
    let host = if let Some(rest) = host_port.strip_prefix('[') {
        // IPv6 literal: `[::1]:port`.
        rest.split(']').next().unwrap_or(rest)
    } else {
        host_port.split(':').next().unwrap_or(host_port)
    };
    // Trim any trailing DNS root dot so `localhost.` / `evil.example.com.`
    // normalise to the same host the range and blocklist checks compare
    // against, closing the trailing-dot bypass (MEM-6).
    host.trim_end_matches('.').to_ascii_lowercase()
}

/// Returns `true` when `host` equals `blocked` or is a subdomain of it, matching
/// on the host component so `evil.example.com` blocks `cdn.evil.example.com`
/// but not `notevil.example.com` (MEM-6).
fn host_matches_block(host: &str, blocked: &str) -> bool {
    // Normalise URL-shaped or port-suffixed blocklist entries (e.g.
    // `https://evil.example.com` or `evil.example.com:8443`) down to their host
    // component, so host-vs-host comparison stays correct after the switch away
    // from raw URL-substring matching (MEM-6).
    let blocked = extract_host(blocked.trim());
    if blocked.is_empty() {
        return false;
    }
    host == blocked || host.ends_with(&format!(".{blocked}"))
}

/// Returns `true` for hosts a cloud-isolated agent must never reach: loopback,
/// RFC1918 / link-local / ULA private ranges, the unspecified address, and the
/// cloud metadata endpoint. Numeric hosts are canonicalised first so the check
/// cannot be dodged with `inet_aton` integer/octal/short forms or IPv4-mapped
/// IPv6 literals (MEM-6).
fn is_private_or_metadata_host(host: &str) -> bool {
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    if host == "metadata.google.internal" {
        return true;
    }
    parse_host_ip(host).is_some_and(ip_is_private_or_metadata)
}

/// Parses `host` as an IP address, accepting the legacy `inet_aton` numeric
/// IPv4 forms (decimal integer, `0`-octal, `0x`-hex, and 1–3 part short forms)
/// that many resolvers still honour, and unwrapping IPv4-mapped IPv6
/// (`::ffff:a.b.c.d`). Returns `None` for genuine hostnames. The IPv6 brackets
/// are already stripped by [`extract_host`].
fn parse_host_ip(host: &str) -> Option<IpAddr> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Some(match ip {
            IpAddr::V6(v6) => v6
                .to_ipv4_mapped()
                .map(IpAddr::V4)
                .unwrap_or(IpAddr::V6(v6)),
            v4 => v4,
        });
    }
    parse_ipv4_relaxed(host).map(IpAddr::V4)
}

/// Returns `true` for loopback, RFC1918/link-local/unique-local private ranges,
/// and the unspecified address (which reaches localhost on many stacks). The
/// AWS/GCP metadata IP `169.254.169.254` falls under link-local.
fn ip_is_private_or_metadata(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
        }
        IpAddr::V6(v6) => {
            let seg0 = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || (seg0 & 0xfe00) == 0xfc00 // fc00::/7 unique-local
                || (seg0 & 0xffc0) == 0xfe80 // fe80::/10 link-local
        }
    }
}

/// Parses `inet_aton`-style IPv4: 1–4 dot-separated parts, each decimal,
/// `0`-prefixed octal, or `0x`-prefixed hex; the final part fills all remaining
/// low-order bytes (so `2130706433`, `0177.0.0.1`, and `127.1` all resolve to
/// `127.0.0.1`).
fn parse_ipv4_relaxed(host: &str) -> Option<Ipv4Addr> {
    if host.is_empty() {
        return None;
    }
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() > 4 {
        return None;
    }
    let nums: Vec<u32> = parts
        .iter()
        .map(|p| parse_inet_number(p))
        .collect::<Option<Vec<_>>>()?;
    let addr = match nums.as_slice() {
        [a] => *a,
        [a, b] if *a <= 0xff && *b <= 0x00ff_ffff => (*a << 24) | *b,
        [a, b, c] if *a <= 0xff && *b <= 0xff && *c <= 0xffff => (*a << 24) | (*b << 16) | *c,
        [a, b, c, d] if *a <= 0xff && *b <= 0xff && *c <= 0xff && *d <= 0xff => {
            (*a << 24) | (*b << 16) | (*c << 8) | *d
        }
        _ => return None,
    };
    Some(Ipv4Addr::from(addr))
}

/// Parses one `inet_aton` numeric component (decimal / `0`-octal / `0x`-hex).
fn parse_inet_number(s: &str) -> Option<u32> {
    if s == "0" {
        return Some(0);
    }
    let (radix, digits) = if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        (16, hex)
    } else if let Some(oct) = s.strip_prefix('0') {
        (8, oct)
    } else {
        (10, s)
    };
    if digits.is_empty() {
        return None;
    }
    u32::from_str_radix(digits, radix).ok()
}

impl Default for UnsafeMotorActionGate {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use anima_self::{Capability, Unverified};

    fn make_verified(capability: &'static str) -> Capability<Verified> {
        Capability::<Unverified>::new(1000, 1000, capability)
            .verify(|_| true)
            .expect("test: capability verification must succeed")
    }

    // ── Filesystem ────────────────────────────────────────────────────────────

    #[test]
    fn read_on_critical_path_is_allowed() {
        let g = UnsafeMotorActionGate::new();
        assert_eq!(
            g.screen_filesystem("read", "/etc/passwd", None),
            VetoResult::Allow
        );
    }

    #[test]
    fn write_to_critical_path_without_capability_is_vetoed() {
        let g = UnsafeMotorActionGate::new();
        let r = g.screen_filesystem("write", "/etc/passwd", None);
        assert!(r.is_vetoed());
        match r {
            VetoResult::Veto(VetoReason::UnsafeMotorAction { action, policy }) => {
                assert!(action.contains("/etc/passwd"));
                assert_eq!(policy, "motor.filesystem.critical");
            }
            _ => panic!("expected UnsafeMotorAction veto"),
        }
    }

    #[test]
    fn write_to_critical_path_with_correct_capability_is_allowed() {
        let g = UnsafeMotorActionGate::new();
        let cap = make_verified("motor.filesystem.critical");
        assert_eq!(
            g.screen_filesystem("write", "/etc/passwd", Some(&cap)),
            VetoResult::Allow
        );
    }

    #[test]
    fn write_to_critical_path_with_wildcard_capability_is_allowed() {
        let g = UnsafeMotorActionGate::new();
        let cap = make_verified("motor.filesystem.*");
        assert_eq!(
            g.screen_filesystem("delete", "/boot/grub/grub.cfg", Some(&cap)),
            VetoResult::Allow
        );
    }

    #[test]
    fn write_to_critical_path_with_wrong_capability_is_vetoed() {
        let g = UnsafeMotorActionGate::new();
        let cap = make_verified("tool.dispatch");
        let r = g.screen_filesystem("write", "/etc/shadow", Some(&cap));
        assert!(r.is_vetoed());
    }

    #[test]
    fn write_to_non_critical_path_is_allowed() {
        let g = UnsafeMotorActionGate::new();
        assert_eq!(
            g.screen_filesystem("write", "/home/user/notes.txt", None),
            VetoResult::Allow
        );
    }

    #[test]
    fn delete_on_boot_path_is_vetoed() {
        let g = UnsafeMotorActionGate::new();
        assert!(g
            .screen_filesystem("delete", "/boot/vmlinuz", None)
            .is_vetoed());
    }

    #[test]
    fn chmod_on_sbin_is_vetoed() {
        let g = UnsafeMotorActionGate::new();
        assert!(g.screen_filesystem("chmod", "/sbin/init", None).is_vetoed());
    }

    #[test]
    fn custom_critical_prefix_is_enforced() {
        let g = UnsafeMotorActionGate::new().with_critical_prefix("/opt/animaos");
        assert!(g
            .screen_filesystem("write", "/opt/animaos/state/identity.json", None)
            .is_vetoed());
    }

    // ── Network ───────────────────────────────────────────────────────────────

    #[test]
    fn non_blocklisted_host_is_allowed() {
        let g = UnsafeMotorActionGate::new().with_blocklisted_host("evil.example.com");
        assert_eq!(
            g.screen_network("https://api.anthropic.com/v1/messages", "POST"),
            VetoResult::Allow
        );
    }

    #[test]
    fn blocklisted_host_is_vetoed() {
        let g = UnsafeMotorActionGate::new().with_blocklisted_host("evil.example.com");
        let r = g.screen_network("https://evil.example.com/steal?data=all", "GET");
        assert!(r.is_vetoed());
        match r {
            VetoResult::Veto(VetoReason::UnsafeMotorAction { policy, .. }) => {
                assert!(policy.contains("blocklisted-host:evil.example.com"));
            }
            _ => panic!("expected UnsafeMotorAction veto"),
        }
    }

    #[test]
    fn subdomain_of_blocklisted_host_is_also_vetoed() {
        let g = UnsafeMotorActionGate::new().with_blocklisted_host("evil.example.com");
        // URL contains the blocked string.
        assert!(g
            .screen_network("https://cdn.evil.example.com/malware.exe", "GET")
            .is_vetoed());
    }

    #[test]
    fn empty_blocklist_allows_all_hosts() {
        let g = UnsafeMotorActionGate::new();
        assert_eq!(
            g.screen_network("https://suspicious-but-not-blocked.io/api", "POST"),
            VetoResult::Allow
        );
    }

    // ── Self-modification ─────────────────────────────────────────────────────

    #[test]
    fn self_modification_is_vetoed_by_default() {
        let g = UnsafeMotorActionGate::new();
        assert!(g
            .screen_self_modification("config/routes.toml", "add a new route", None)
            .is_vetoed());
    }

    #[test]
    fn self_modification_vetoed_even_with_flag_but_no_capability() {
        let g = UnsafeMotorActionGate {
            allow_self_modification: true,
            ..UnsafeMotorActionGate::new()
        };
        assert!(g
            .screen_self_modification("config/routes.toml", "add a route", None)
            .is_vetoed());
    }

    #[test]
    fn self_modification_allowed_with_flag_and_capability() {
        let g = UnsafeMotorActionGate {
            allow_self_modification: true,
            ..UnsafeMotorActionGate::new()
        };
        let cap = make_verified("self.modify");
        assert_eq!(
            g.screen_self_modification("config/routes.toml", "add a route", Some(&cap)),
            VetoResult::Allow
        );
    }

    #[test]
    fn self_modification_blocked_when_flag_false_even_with_capability() {
        let g = UnsafeMotorActionGate::new(); // allow_self_modification = false
        let cap = make_verified("self.modify");
        assert!(g
            .screen_self_modification("config/prompts.txt", "append new persona", Some(&cap))
            .is_vetoed());
    }

    #[test]
    fn self_modification_allowed_with_wildcard_self_capability() {
        let g = UnsafeMotorActionGate {
            allow_self_modification: true,
            ..UnsafeMotorActionGate::new()
        };
        let cap = make_verified("self.*");
        assert_eq!(
            g.screen_self_modification("config/routes.toml", "update", Some(&cap)),
            VetoResult::Allow
        );
    }

    // ── Path-traversal (MEM-1) ────────────────────────────────────────────────

    #[test]
    fn traversal_paths_into_critical_tree_require_capability() {
        let g = UnsafeMotorActionGate::new();
        for path in [
            "/var/../etc/passwd",
            "./etc/passwd",
            "etc/passwd",
            "/tmp/../../etc/shadow",
            "/etc/./ssh/sshd_config",
        ] {
            assert!(
                g.screen_filesystem("write", path, None).is_vetoed(),
                "{path} resolves into a critical tree and must be vetoed"
            );
        }
    }

    #[test]
    fn prefix_lookalike_paths_are_not_critical() {
        let g = UnsafeMotorActionGate::new();
        for path in ["/etcd/data.db", "/binary/app", "/libreoffice/config"] {
            assert_eq!(
                g.screen_filesystem("write", path, None),
                VetoResult::Allow,
                "{path} only looks like a critical prefix"
            );
        }
    }

    // ── SSRF / host matching (MEM-6) ──────────────────────────────────────────

    #[test]
    fn private_and_metadata_hosts_are_blocked_by_default() {
        let g = UnsafeMotorActionGate::new(); // empty blocklist
        for url in [
            "http://169.254.169.254/latest/meta-data/",
            "http://127.0.0.1:8088/admin",
            "http://localhost/",
            "http://10.0.0.5/internal",
            "http://192.168.1.1/",
            "http://[::1]:9000/",
        ] {
            let r = g.screen_network(url, "GET");
            assert!(r.is_vetoed(), "{url} must be blocked as SSRF");
        }
    }

    #[test]
    fn blocklist_substring_misfires_are_fixed() {
        let g = UnsafeMotorActionGate::new().with_blocklisted_host("10.0.0.1");
        // A different address that merely shares a prefix must not be blocked.
        assert_eq!(
            g.screen_network("https://93.184.216.34/", "GET"),
            VetoResult::Allow
        );
        let g2 = UnsafeMotorActionGate::new().with_blocklisted_host("evil.com");
        assert_eq!(
            g2.screen_network("https://notevil.com/api", "GET"),
            VetoResult::Allow
        );
        // Userinfo trick must not smuggle a good host past the block.
        let g3 = UnsafeMotorActionGate::new().with_blocklisted_host("evil.com");
        assert!(g3
            .screen_network("https://good.com@evil.com/x", "GET")
            .is_vetoed());
    }

    #[test]
    fn non_canonical_loopback_encodings_are_blocked() {
        let g = UnsafeMotorActionGate::new(); // empty blocklist — SSRF default only
        for url in [
            "http://localhost./admin",    // trailing DNS root dot
            "http://127.0.0.1./x",        // trailing dot on an IP
            "http://2130706433/",         // 127.0.0.1 as a u32
            "http://0177.0.0.1/",         // octal first octet
            "http://127.1/",              // inet_aton short form
            "http://0x7f.0.0.1/",         // hex octet
            "http://[::ffff:127.0.0.1]/", // IPv4-mapped IPv6 loopback
            "http://[::ffff:10.0.0.5]/",  // IPv4-mapped IPv6 private
            "http://0.0.0.0/",            // unspecified → localhost
        ] {
            assert!(
                g.screen_network(url, "GET").is_vetoed(),
                "{url} must be blocked as SSRF"
            );
        }
    }

    #[test]
    fn public_hosts_and_ips_are_not_false_positives() {
        let g = UnsafeMotorActionGate::new();
        for url in [
            "https://api.anthropic.com/v1/messages",
            "https://93.184.216.34/",          // example.com
            "https://8.8.8.8/",                // public resolver
            "https://[2606:4700:4700::1111]/", // public IPv6
        ] {
            assert_eq!(
                g.screen_network(url, "GET"),
                VetoResult::Allow,
                "{url} is public and must be allowed"
            );
        }
    }

    #[test]
    fn url_shaped_blocklist_entries_still_match() {
        // Entries carrying a scheme or port must normalise to the host so they
        // keep matching after the move to host-component comparison.
        for entry in ["https://evil.example.com", "evil.example.com:8443"] {
            let g = UnsafeMotorActionGate::new().with_blocklisted_host(entry);
            assert!(
                g.screen_network("https://evil.example.com/x", "GET")
                    .is_vetoed(),
                "blocklist entry {entry:?} must still veto the host"
            );
        }
    }
}
