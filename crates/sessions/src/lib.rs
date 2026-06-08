#![forbid(unsafe_code)]

//! Conversation history and session management for AnimaOS — Epic E22.
//!
//! A *session* is a bounded sequence of [`ConversationTurn`]s owned by a
//! specific user and managed by a specific agent.  Sessions are the durable
//! record of every conversation: they survive process restarts, can be searched
//! and exported, and are consolidated into the episodic memory during sleep
//! cycles.
//!
//! # Core types
//!
//! | Type | Role |
//! |------|------|
//! | [`SessionRecord`] | Top-level session container (id, user, turns, status, summary) |
//! | [`ConversationTurn`] | One message exchanged in a session |
//! | [`ConversationRole`] | Speaker role (`user`, `assistant`, `system`, `tool`) |
//! | [`SessionStatus`] | Lifecycle state (`active`, `archived`, `deleted`) |
//! | [`SessionStore`] | Durable, atomic-write collection of `SessionRecord`s |
//! | [`SessionQuery`] | Filtering criteria for [`SessionStore::list`] |
//! | [`ExportFormat`] | Output format for [`SessionStore::export`] |
//! | [`SessionError`] | Error type for all session operations |
//!
//! # Persistence
//!
//! [`SessionStore::open`] loads or creates a file-backed store; writes are
//! atomic (write to `.tmp`, then rename).  [`SessionStore::in_memory`] provides
//! a transient store for tests.
//!
//! # Example
//!
//! ```rust
//! use sessions::{SessionRecord, SessionStore, SessionQuery, ConversationTurn, ConversationRole};
//!
//! let mut store = SessionStore::in_memory();
//! let session = SessionRecord::new("sess-0001", "user:alice", "agent-a");
//! store.insert(session).unwrap();
//! store.append_turn(
//!     "sess-0001",
//!     ConversationTurn::new(0, ConversationRole::User, "Hello!"),
//! ).unwrap();
//! let results = store.list(&SessionQuery::for_user("user:alice"));
//! assert_eq!(results.len(), 1);
//! assert_eq!(results[0].turn_count(), 1);
//! ```

pub mod record;
pub mod store;

pub use record::{
    make_session_id, ConversationRole, ConversationTurn, SessionError, SessionRecord, SessionStatus,
};
pub use store::{ExportFormat, SessionQuery, SessionStore};
