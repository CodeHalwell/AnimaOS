#![forbid(unsafe_code)]

//! Per-user identity profile — E17 S17.1.
//!
//! Each user that contacts the agent through any E10 channel (Telegram, Slack, …)
//! is represented as a [`UserProfile`].  Profiles are keyed by a stable
//! `user_id` string that concatenates the channel name and the platform's
//! sender identifier (e.g. `"telegram:123456789"`).
//!
//! [`TrustTier`] controls how much latitude the agent gives a user:
//! - `Unknown`  — first contact; no prior data, minimal trust.
//! - `Verified` — the operator has confirmed this is a real person.
//! - `Trusted`  — an established relationship; elevated priority and context.
//! - `Operator` — full operator-level trust (treated like the local operator).
//!
//! The `facts` map mirrors [`vita::identity::IdentityMemory`]'s `facts` dict
//! but is scoped per user rather than global.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// ── TrustTier ─────────────────────────────────────────────────────────────────

/// The level of trust granted to a user.
///
/// Tiers are ordered from least to most trusted; [`TrustTier::Operator`] is
/// equivalent to the local operator and must be assigned explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustTier {
    /// First contact; no relationship established.
    #[default]
    Unknown = 0,
    /// Identity confirmed by the operator (e.g. phone verification).
    Verified = 1,
    /// Established relationship with a track record.
    Trusted = 2,
    /// Full operator-level permissions; must be granted explicitly.
    Operator = 3,
}

impl TrustTier {
    /// Returns a human-readable label for audit entries.
    pub fn as_str(self) -> &'static str {
        match self {
            TrustTier::Unknown => "unknown",
            TrustTier::Verified => "verified",
            TrustTier::Trusted => "trusted",
            TrustTier::Operator => "operator",
        }
    }

    /// Returns `true` when the tier grants at least the given minimum level.
    pub fn at_least(self, minimum: TrustTier) -> bool {
        self >= minimum
    }
}

impl std::fmt::Display for TrustTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for TrustTier {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "unknown" => Ok(TrustTier::Unknown),
            "verified" => Ok(TrustTier::Verified),
            "trusted" => Ok(TrustTier::Trusted),
            "operator" => Ok(TrustTier::Operator),
            other => Err(format!("unknown trust tier: {other:?}")),
        }
    }
}

// ── UserProfile ───────────────────────────────────────────────────────────────

/// A per-user identity profile stored in the [`crate::registry::UserRegistry`].
///
/// The `user_id` is the canonical key and is always of the form
/// `"<channel>:<platform_id>"` (e.g. `"telegram:987654321"`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserProfile {
    /// Stable canonical identifier (`"<channel>:<platform_id>"`).
    pub user_id: String,
    /// Human-readable display name (from the channel, or operator-overridden).
    pub display_name: String,
    /// Source channel identifier (`"telegram"`, `"slack"`, …).
    pub channel: String,
    /// Trust level assigned by the operator.
    #[serde(default)]
    pub trust_tier: TrustTier,
    /// Unix nanoseconds when the profile was first created.
    pub created_at_ns: u64,
    /// Unix nanoseconds of the most recent inbound message.
    pub last_seen_ns: u64,
    /// Free-form key/value facts about this user (operator-editable).
    #[serde(default)]
    pub facts: HashMap<String, String>,
    /// Schema version; increment when the format changes.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
}

fn default_schema_version() -> u32 {
    1
}

impl UserProfile {
    /// Creates a new profile with `Unknown` trust and no facts.
    pub fn new(
        user_id: impl Into<String>,
        display_name: impl Into<String>,
        channel: impl Into<String>,
        now_ns: u64,
    ) -> Self {
        Self {
            user_id: user_id.into(),
            display_name: display_name.into(),
            channel: channel.into(),
            trust_tier: TrustTier::Unknown,
            created_at_ns: now_ns,
            last_seen_ns: now_ns,
            facts: HashMap::new(),
            schema_version: 1,
        }
    }

    /// Returns a `"<channel>:<platform_id>"` key from raw parts.
    pub fn make_id(channel: &str, platform_id: &str) -> String {
        format!("{channel}:{platform_id}")
    }

    /// Sets a fact; returns the previous value if one existed.
    pub fn set_fact(&mut self, key: impl Into<String>, value: impl Into<String>) -> Option<String> {
        self.facts.insert(key.into(), value.into())
    }

    /// Retrieves a fact by key.
    pub fn get_fact(&self, key: &str) -> Option<&str> {
        self.facts.get(key).map(String::as_str)
    }

    /// Updates `last_seen_ns` to `now_ns`.
    pub fn touch(&mut self, now_ns: u64) {
        self.last_seen_ns = now_ns;
    }

    /// Returns a compact JSON representation suitable for cortex context injection.
    pub fn to_context_json(&self) -> serde_json::Value {
        serde_json::json!({
            "user_id": self.user_id,
            "display_name": self.display_name,
            "channel": self.channel,
            "trust_tier": self.trust_tier.as_str(),
            "facts": self.facts,
        })
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn trust_tier_ordering_is_correct() {
        assert!(TrustTier::Unknown < TrustTier::Verified);
        assert!(TrustTier::Verified < TrustTier::Trusted);
        assert!(TrustTier::Trusted < TrustTier::Operator);
    }

    #[test]
    fn trust_tier_at_least() {
        assert!(TrustTier::Trusted.at_least(TrustTier::Verified));
        assert!(TrustTier::Trusted.at_least(TrustTier::Trusted));
        assert!(!TrustTier::Trusted.at_least(TrustTier::Operator));
        assert!(TrustTier::Unknown.at_least(TrustTier::Unknown));
    }

    #[test]
    fn trust_tier_from_str_round_trips() {
        for tier in [
            TrustTier::Unknown,
            TrustTier::Verified,
            TrustTier::Trusted,
            TrustTier::Operator,
        ] {
            let parsed = TrustTier::from_str(tier.as_str()).expect("parse");
            assert_eq!(parsed, tier);
        }
    }

    #[test]
    fn trust_tier_from_str_rejects_unknown_label() {
        assert!(TrustTier::from_str("superuser").is_err());
    }

    #[test]
    fn user_profile_new_has_unknown_trust_and_no_facts() {
        let p = UserProfile::new("telegram:42", "Alice", "telegram", 1_000_000);
        assert_eq!(p.trust_tier, TrustTier::Unknown);
        assert!(p.facts.is_empty());
        assert_eq!(p.created_at_ns, 1_000_000);
        assert_eq!(p.last_seen_ns, 1_000_000);
    }

    #[test]
    fn make_id_produces_channel_prefixed_key() {
        assert_eq!(UserProfile::make_id("slack", "U12345"), "slack:U12345");
    }

    #[test]
    fn set_and_get_fact_round_trip() {
        let mut p = UserProfile::new("slack:U1", "Bob", "slack", 0);
        let old = p.set_fact("timezone", "Europe/London");
        assert!(old.is_none());
        assert_eq!(p.get_fact("timezone"), Some("Europe/London"));
    }

    #[test]
    fn set_fact_returns_previous_value() {
        let mut p = UserProfile::new("slack:U1", "Bob", "slack", 0);
        p.set_fact("lang", "en");
        let old = p.set_fact("lang", "fr");
        assert_eq!(old.as_deref(), Some("en"));
        assert_eq!(p.get_fact("lang"), Some("fr"));
    }

    #[test]
    fn touch_updates_last_seen() {
        let mut p = UserProfile::new("telegram:1", "Carol", "telegram", 100);
        p.touch(999);
        assert_eq!(p.last_seen_ns, 999);
        assert_eq!(p.created_at_ns, 100);
    }

    #[test]
    fn to_context_json_contains_expected_fields() {
        let mut p = UserProfile::new("telegram:99", "Dave", "telegram", 0);
        p.trust_tier = TrustTier::Trusted;
        p.set_fact("key", "val");
        let v = p.to_context_json();
        assert_eq!(v["user_id"], "telegram:99");
        assert_eq!(v["display_name"], "Dave");
        assert_eq!(v["trust_tier"], "trusted");
        assert_eq!(v["facts"]["key"], "val");
    }

    #[test]
    fn profile_round_trips_through_json() {
        let mut p = UserProfile::new("slack:X", "Eve", "slack", 42);
        p.trust_tier = TrustTier::Verified;
        p.set_fact("role", "admin");
        let json = serde_json::to_string(&p).unwrap();
        let restored: UserProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, p);
    }
}
