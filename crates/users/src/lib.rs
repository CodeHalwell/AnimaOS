#![forbid(unsafe_code)]

//! Per-user identity, trust tiers, and consent model — Epic E17.
//!
//! # Scope
//!
//! AnimaOS's E10 (Presence) comms layer currently treats every inbound message
//! as anonymous: `ChannelMessage::from` is a plain string with no associated
//! state.  E17 adds a **per-user identity layer** on top of that string so the
//! agent can:
//!
//! 1. Recognise returning users and greet them appropriately.
//! 2. Enforce trust-tier policies (e.g. only `Trusted` users may trigger
//!    operator-grade cortex invocations).
//! 3. Honour per-user data-retention consent (e.g. only retain episodic
//!    memories for users who have consented to `EpisodicMemory`).
//! 4. Inject a compact user-profile JSON object into every cortex invocation
//!    so the cortex is always aware of who it is speaking to.
//!
//! # Architecture
//!
//! ```text
//!  ChannelMessage { from: "telegram:123" }
//!          │
//!          ▼  lookup / upsert
//!  UserRegistry ───► UserRecord { profile: UserProfile, consent: ConsentRecord }
//!          │
//!          ▼  to_context_json()
//!  vita::InvokeRequest.user_profile = Some(json)
//! ```
//!
//! # Modules
//!
//! | Module | Contents |
//! |---|---|
//! | [`profile`] | [`profile::UserProfile`], [`profile::TrustTier`] |
//! | [`consent`] | [`consent::ConsentRecord`], [`consent::DataCategory`], [`consent::Grant`] |
//! | [`registry`] | [`registry::UserRegistry`], [`registry::UserRecord`], [`registry::RegistryError`] |

pub mod consent;
pub mod profile;
pub mod registry;

// Re-export the most commonly used types.
pub use consent::{ConsentRecord, DataCategory, Grant};
pub use profile::{TrustTier, UserProfile};
pub use registry::{RegistryError, UserRecord, UserRegistry};
