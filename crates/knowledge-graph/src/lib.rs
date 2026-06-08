#![forbid(unsafe_code)]

//! Structured entity and relationship graph for AnimaOS — Epic E27.
//!
//! # Scope
//!
//! AnimaOS's E14 (Higher Cognition) introduces a document-level knowledge corpus
//! backed by the L3 archive.  E27 adds a **structural layer** on top: a typed
//! entity and relationship graph so the agent can reason about *who*, *what*, and
//! *how things connect*, not just retrieve similar documents.
//!
//! # Architecture
//!
//! ```text
//!  ChannelMessage / Cortex observation
//!          │
//!          ▼  extract entities & relations
//!  KnowledgeGraph ── add_entity() ─► Entity { id, kind, display_name, attributes }
//!          │        ── add_relation() ► Relation { from, to, kind, weight }
//!          │
//!          ▼  query
//!  find_neighbors(id, depth)   ──► Vec<&Entity>   (BFS, undirected)
//!  find_by_kind(kind)          ──► Vec<&Entity>
//!  find_by_attribute(key, val) ──► Vec<&Entity>
//!          │
//!          ▼  persist
//!  KnowledgeGraph::flush()     ──► ~/.anima/<agent_id>/knowledge_graph.json
//! ```
//!
//! # Modules
//!
//! | Module | Contents |
//! |---|---|
//! | [`entity`] | [`entity::Entity`], [`entity::EntityKind`] |
//! | [`relation`] | [`relation::Relation`], [`relation::RelationKind`] |
//! | [`graph`] | [`graph::KnowledgeGraph`], [`graph::GraphError`] |

pub mod entity;
pub mod graph;
pub mod relation;

// Re-export the most commonly used types.
pub use entity::{Entity, EntityKind};
pub use graph::{GraphError, KnowledgeGraph};
pub use relation::{Relation, RelationKind};
