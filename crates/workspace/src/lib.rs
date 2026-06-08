#![forbid(unsafe_code)]

//! Multi-tenant workspace management — Epic E31.
//!
//! AnimaOS can serve multiple logical tenants through its channel gateways.
//! This crate provides the workspace abstraction that groups users under a
//! shared identity, enforces resource quotas, and records all membership
//! changes in the audit log.
//!
//! # Concepts
//!
//! - **[`WorkspaceProfile`]** — the top-level record for a tenant: ID, display
//!   name, owner, and lifecycle status (active / suspended / deleted).
//! - **[`WorkspaceRole`]** — the authority level a user holds within a workspace
//!   (`Guest < Member < Admin < Owner`).
//! - **[`WorkspaceMembership`]** — a user's membership record including their role
//!   and the time they joined.
//! - **[`WorkspaceQuota`]** — per-workspace resource limits (members, daily
//!   tokens, storage, active tasks).
//! - **[`WorkspaceRegistry`]** — the persistent store for all workspace records,
//!   backed by an atomic JSON file.
//!
//! # Quick-start
//!
//! ```rust
//! use workspace::{WorkspaceProfile, WorkspaceRegistry, WorkspaceRole};
//!
//! let mut reg = WorkspaceRegistry::in_memory();
//!
//! // Create a workspace owned by user "telegram:1".
//! let profile = WorkspaceProfile::new("acme", "Acme Corp", "telegram:1", 0);
//! reg.create(profile, 0).unwrap();
//!
//! // Add a second user as a member.
//! reg.add_member("acme", "telegram:2", WorkspaceRole::Member, 1_000)
//!    .unwrap();
//!
//! // Check the member's role.
//! assert_eq!(reg.member_role("acme", "telegram:2"), Some(WorkspaceRole::Member));
//! ```

pub mod membership;
pub mod quota;
pub mod registry;
pub mod workspace;

pub use membership::{WorkspaceMembership, WorkspaceRole};
pub use quota::{QuotaUsage, QuotaViolation, WorkspaceQuota};
pub use registry::{WorkspaceError, WorkspaceRecord, WorkspaceRegistry};
pub use workspace::{WorkspaceProfile, WorkspaceStatus};
