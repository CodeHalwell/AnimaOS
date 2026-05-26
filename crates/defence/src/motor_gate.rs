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

        let targets_critical = self
            .critical_prefixes
            .iter()
            .any(|prefix| path.starts_with(prefix.as_str()));

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
        for blocked in &self.blocklisted_hosts {
            if url.contains(blocked.as_str()) {
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
}
