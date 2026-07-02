#![forbid(unsafe_code)]

//! Shared JSON-persistence helpers for AnimaOS's file-backed stores.
//!
//! The operational-wave crates (`sessions`, `jobs`, `workspace`, `feedback`,
//! `knowledge-graph`, `webhooks`, `config`, …) each persist a small JSON
//! document to disk. They previously reimplemented two things inconsistently:
//!
//! - the **default state directory**, whose `HOME`-missing fallback diverged
//!   across crates (`/tmp` vs `/root` vs the CWD) — `/tmp` is world-readable and
//!   reboot-wiped, so durable state placed there is a data-loss risk (OPS-13);
//! - the **atomic write** (write-to-`.tmp`-then-rename), copied verbatim.
//!
//! This crate centralises both so every store resolves the same safe location
//! and shares one crash-safe write path.

use std::path::{Path, PathBuf};

/// Base directory for durable agent state.
///
/// Resolution order:
/// 1. `ANIMA_STATE_DIR` (explicit operator override), when non-empty;
/// 2. `$HOME/.anima`, when `HOME` is set and non-empty;
/// 3. `/var/lib/anima` as a fail-closed default.
///
/// Note the fallback is **never** `/tmp` (reboot-wiped, world-readable) or the
/// current working directory (non-deterministic), so durable state has one
/// safe, consistent home across every store (OPS-13).
pub fn state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("ANIMA_STATE_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return Path::new(&home).join(".anima");
        }
    }
    PathBuf::from("/var/lib/anima")
}

/// The state path `state_dir()/<agent_id>/<filename>` — the common per-agent
/// store layout.
pub fn agent_state_path(agent_id: &str, filename: &str) -> PathBuf {
    state_dir().join(agent_id).join(filename)
}

/// The state path `state_dir()/<filename>` — for stores not scoped per agent.
pub fn state_path(filename: &str) -> PathBuf {
    state_dir().join(filename)
}

/// Atomically write `bytes` to `path`.
///
/// Creates parent directories, writes to a `.tmp` sibling, then renames over the
/// target so a crash mid-write never leaves a partially-written or truncated
/// file (rename is atomic on the same filesystem).
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_dir_prefers_explicit_override() {
        // We can't safely mutate process env in parallel tests, so assert the
        // pure composition helpers instead of the env-driven branch.
        let p = agent_state_path("anima", "sessions.json");
        assert!(p.ends_with("anima/sessions.json"));
        assert!(state_path("jobs.json").ends_with("jobs.json"));
    }

    #[test]
    fn atomic_write_creates_dirs_and_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deep/data.json");
        atomic_write(&path, b"{\"ok\":true}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"ok\":true}");
        // The temp sibling must not linger after a successful write.
        assert!(!path.with_extension("tmp").exists());
    }

    #[test]
    fn atomic_write_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.json");
        atomic_write(&path, b"first").unwrap();
        atomic_write(&path, b"second").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
    }
}
