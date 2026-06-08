//! Core knowledge-graph structure, mutation API, and query methods.

use crate::entity::{Entity, EntityKind};
use crate::relation::{Relation, RelationKind};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

/// Errors that can occur during graph operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphError {
    /// An entity with this id is already in the graph.
    EntityAlreadyExists(String),
    /// The referenced entity id does not exist.
    EntityNotFound(String),
    /// A relation between these two entities with this kind already exists.
    RelationAlreadyExists {
        from: String,
        to: String,
        kind: String,
    },
    /// Persistent I/O error.
    Io(String),
}

impl std::fmt::Display for GraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GraphError::EntityAlreadyExists(id) => write!(f, "entity '{id}' already exists"),
            GraphError::EntityNotFound(id) => write!(f, "entity '{id}' not found"),
            GraphError::RelationAlreadyExists { from, to, kind } => {
                write!(f, "relation {from} --[{kind}]--> {to} already exists")
            }
            GraphError::Io(msg) => write!(f, "I/O error: {msg}"),
        }
    }
}

/// Serialisable snapshot used for JSON persistence.
#[derive(Serialize, Deserialize)]
struct GraphSnapshot {
    entities: Vec<Entity>,
    relations: Vec<Relation>,
}

/// Directed, attributed knowledge graph with atomic JSON persistence.
pub struct KnowledgeGraph {
    entities: HashMap<String, Entity>,
    /// Forward adjacency: `from_id → set of to_ids`.
    adjacency: HashMap<String, Vec<String>>,
    /// Backward adjacency: `to_id → set of from_ids`.
    back_adjacency: HashMap<String, Vec<String>>,
    relations: Vec<Relation>,
    path: Option<PathBuf>,
}

impl KnowledgeGraph {
    /// Create a transient, in-memory graph (no persistence).
    pub fn in_memory() -> Self {
        KnowledgeGraph {
            entities: HashMap::new(),
            adjacency: HashMap::new(),
            back_adjacency: HashMap::new(),
            relations: Vec::new(),
            path: None,
        }
    }

    /// Load a graph from `path`, or create a new empty one if the file is absent.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, GraphError> {
        let path = path.as_ref().to_path_buf();
        if path.exists() {
            let json = std::fs::read_to_string(&path).map_err(|e| GraphError::Io(e.to_string()))?;
            let snap: GraphSnapshot =
                serde_json::from_str(&json).map_err(|e| GraphError::Io(e.to_string()))?;
            let mut g = KnowledgeGraph {
                entities: HashMap::new(),
                adjacency: HashMap::new(),
                back_adjacency: HashMap::new(),
                relations: Vec::new(),
                path: Some(path),
            };
            for entity in snap.entities {
                g.entities.insert(entity.id.clone(), entity);
            }
            for relation in snap.relations {
                g.adjacency
                    .entry(relation.from.clone())
                    .or_default()
                    .push(relation.to.clone());
                g.back_adjacency
                    .entry(relation.to.clone())
                    .or_default()
                    .push(relation.from.clone());
                g.relations.push(relation);
            }
            Ok(g)
        } else {
            Ok(KnowledgeGraph {
                entities: HashMap::new(),
                adjacency: HashMap::new(),
                back_adjacency: HashMap::new(),
                relations: Vec::new(),
                path: Some(path),
            })
        }
    }

    // ── Mutation API ──────────────────────────────────────────────────────────

    /// Add a new entity. Returns `EntityAlreadyExists` if the id is taken.
    pub fn add_entity(&mut self, entity: Entity) -> Result<(), GraphError> {
        if self.entities.contains_key(&entity.id) {
            return Err(GraphError::EntityAlreadyExists(entity.id));
        }
        self.entities.insert(entity.id.clone(), entity);
        Ok(())
    }

    /// Remove an entity and all relations that reference it.
    /// Returns `true` if the entity was present.
    pub fn remove_entity(&mut self, id: &str) -> bool {
        if self.entities.remove(id).is_none() {
            return false;
        }
        // Remove all relations touching this entity.
        self.relations.retain(|r| r.from != id && r.to != id);
        // Rebuild adjacency from scratch (simpler than patching in-place).
        self.rebuild_adjacency();
        true
    }

    /// Retrieve an entity by id.
    pub fn get_entity(&self, id: &str) -> Option<&Entity> {
        self.entities.get(id)
    }

    /// Retrieve a mutable entity reference by id.
    pub fn get_entity_mut(&mut self, id: &str) -> Option<&mut Entity> {
        self.entities.get_mut(id)
    }

    /// Add a directed relation. Both endpoint entities must already exist.
    /// Duplicate (from, to, kind) triples are rejected.
    pub fn add_relation(&mut self, relation: Relation) -> Result<(), GraphError> {
        if !self.entities.contains_key(&relation.from) {
            return Err(GraphError::EntityNotFound(relation.from));
        }
        if !self.entities.contains_key(&relation.to) {
            return Err(GraphError::EntityNotFound(relation.to));
        }
        let kind_str = relation.kind.to_string();
        let already_exists = self.relations.iter().any(|r| {
            r.from == relation.from && r.to == relation.to && r.kind.to_string() == kind_str
        });
        if already_exists {
            return Err(GraphError::RelationAlreadyExists {
                from: relation.from,
                to: relation.to,
                kind: kind_str,
            });
        }
        self.adjacency
            .entry(relation.from.clone())
            .or_default()
            .push(relation.to.clone());
        self.back_adjacency
            .entry(relation.to.clone())
            .or_default()
            .push(relation.from.clone());
        self.relations.push(relation);
        Ok(())
    }

    /// Remove the relation matching (from, to, kind). Returns `true` if found.
    pub fn remove_relation(&mut self, from: &str, to: &str, kind: &RelationKind) -> bool {
        let kind_str = kind.to_string();
        let before = self.relations.len();
        self.relations
            .retain(|r| !(r.from == from && r.to == to && r.kind.to_string() == kind_str));
        let removed = self.relations.len() < before;
        if removed {
            self.rebuild_adjacency();
        }
        removed
    }

    // ── Query API ─────────────────────────────────────────────────────────────

    /// Return all entities directly connected to `entity_id` (depth 1), or up
    /// to `max_depth` hops away (BFS, undirected, excluding the source).
    pub fn find_neighbors(&self, entity_id: &str, max_depth: usize) -> Vec<&Entity> {
        if !self.entities.contains_key(entity_id) {
            return vec![];
        }
        let mut visited: HashSet<&str> = HashSet::new();
        let mut queue: VecDeque<(&str, usize)> = VecDeque::new();
        let mut result: Vec<&Entity> = Vec::new();

        visited.insert(entity_id);
        queue.push_back((entity_id, 0));

        while let Some((current, depth)) = queue.pop_front() {
            if depth > 0 {
                if let Some(entity) = self.entities.get(current) {
                    result.push(entity);
                }
            }
            if depth < max_depth {
                // Forward neighbours.
                if let Some(fwd) = self.adjacency.get(current) {
                    for next in fwd {
                        if visited.insert(next.as_str()) {
                            queue.push_back((next.as_str(), depth + 1));
                        }
                    }
                }
                // Backward neighbours (treat graph as undirected for traversal).
                if let Some(bwd) = self.back_adjacency.get(current) {
                    for next in bwd {
                        if visited.insert(next.as_str()) {
                            queue.push_back((next.as_str(), depth + 1));
                        }
                    }
                }
            }
        }
        result
    }

    /// Return all entities whose kind matches the given `EntityKind`.
    pub fn find_by_kind(&self, kind: &EntityKind) -> Vec<&Entity> {
        self.entities.values().filter(|e| &e.kind == kind).collect()
    }

    /// Return all entities that have an attribute `key` with value `value`.
    pub fn find_by_attribute(&self, key: &str, value: &str) -> Vec<&Entity> {
        self.entities
            .values()
            .filter(|e| e.get_attribute(key) == Some(value))
            .collect()
    }

    /// Return all relations where `from` or `to` matches `entity_id`.
    pub fn relations_for(&self, entity_id: &str) -> Vec<&Relation> {
        self.relations
            .iter()
            .filter(|r| r.from == entity_id || r.to == entity_id)
            .collect()
    }

    /// All relations in the graph.
    pub fn all_relations(&self) -> &[Relation] {
        &self.relations
    }

    /// All entities in the graph, sorted by id for deterministic output.
    pub fn all_entities(&self) -> Vec<&Entity> {
        let mut v: Vec<&Entity> = self.entities.values().collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    // ── Metrics ───────────────────────────────────────────────────────────────

    /// Number of entities.
    pub fn entity_count(&self) -> usize {
        self.entities.len()
    }

    /// Number of relations.
    pub fn relation_count(&self) -> usize {
        self.relations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    // ── Persistence ───────────────────────────────────────────────────────────

    /// Atomically write the graph to its backing file (write-to-`.tmp`-then-rename).
    /// Returns `Ok(())` for an in-memory graph without a backing file.
    pub fn flush(&self) -> Result<(), GraphError> {
        let path = match &self.path {
            Some(p) => p,
            None => return Ok(()),
        };
        let snap = GraphSnapshot {
            entities: self.entities.values().cloned().collect(),
            relations: self.relations.clone(),
        };
        let json =
            serde_json::to_string_pretty(&snap).map_err(|e| GraphError::Io(e.to_string()))?;
        let tmp = path.with_extension("tmp");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| GraphError::Io(e.to_string()))?;
        }
        std::fs::write(&tmp, json.as_bytes()).map_err(|e| GraphError::Io(e.to_string()))?;
        std::fs::rename(&tmp, path).map_err(|e| GraphError::Io(e.to_string()))?;
        Ok(())
    }

    /// Default backing-file path: `~/.anima/<agent_id>/knowledge_graph.json`.
    pub fn default_path(agent_id: &str) -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(home)
            .join(".anima")
            .join(agent_id)
            .join("knowledge_graph.json")
    }

    // ── Internal helpers ─────────────────────────────────────────────────────

    fn rebuild_adjacency(&mut self) {
        self.adjacency.clear();
        self.back_adjacency.clear();
        for r in &self.relations {
            self.adjacency
                .entry(r.from.clone())
                .or_default()
                .push(r.to.clone());
            self.back_adjacency
                .entry(r.to.clone())
                .or_default()
                .push(r.from.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::EntityKind;
    use crate::relation::RelationKind;

    fn alice() -> Entity {
        Entity::new("alice", EntityKind::Person, "Alice Smith")
    }

    fn acme() -> Entity {
        Entity::new("acme", EntityKind::Organization, "Acme Corp")
    }

    fn rust_tech() -> Entity {
        Entity::new("rust", EntityKind::Technology, "Rust Language")
    }

    #[test]
    fn new_graph_is_empty() {
        let g = KnowledgeGraph::in_memory();
        assert_eq!(g.entity_count(), 0);
        assert_eq!(g.relation_count(), 0);
        assert!(g.is_empty());
    }

    #[test]
    fn add_entity_increases_count() {
        let mut g = KnowledgeGraph::in_memory();
        g.add_entity(alice()).unwrap();
        assert_eq!(g.entity_count(), 1);
        assert!(!g.is_empty());
    }

    #[test]
    fn add_entity_rejects_duplicate_id() {
        let mut g = KnowledgeGraph::in_memory();
        g.add_entity(alice()).unwrap();
        let err = g.add_entity(alice()).unwrap_err();
        assert!(matches!(err, GraphError::EntityAlreadyExists(_)));
    }

    #[test]
    fn get_entity_returns_correct_entity() {
        let mut g = KnowledgeGraph::in_memory();
        g.add_entity(alice()).unwrap();
        let e = g.get_entity("alice").unwrap();
        assert_eq!(e.display_name, "Alice Smith");
    }

    #[test]
    fn get_entity_returns_none_for_missing_id() {
        let g = KnowledgeGraph::in_memory();
        assert!(g.get_entity("ghost").is_none());
    }

    #[test]
    fn remove_entity_decreases_count() {
        let mut g = KnowledgeGraph::in_memory();
        g.add_entity(alice()).unwrap();
        assert!(g.remove_entity("alice"));
        assert_eq!(g.entity_count(), 0);
    }

    #[test]
    fn remove_entity_returns_false_when_not_present() {
        let mut g = KnowledgeGraph::in_memory();
        assert!(!g.remove_entity("ghost"));
    }

    #[test]
    fn remove_entity_cascades_to_relations() {
        let mut g = KnowledgeGraph::in_memory();
        g.add_entity(alice()).unwrap();
        g.add_entity(acme()).unwrap();
        g.add_relation(Relation::new("alice", "acme", RelationKind::WorksAt))
            .unwrap();
        assert_eq!(g.relation_count(), 1);
        g.remove_entity("alice");
        assert_eq!(g.relation_count(), 0);
    }

    #[test]
    fn add_relation_requires_both_entities() {
        let mut g = KnowledgeGraph::in_memory();
        g.add_entity(alice()).unwrap();
        let err = g
            .add_relation(Relation::new("alice", "ghost", RelationKind::RelatedTo))
            .unwrap_err();
        assert!(matches!(err, GraphError::EntityNotFound(_)));
    }

    #[test]
    fn add_relation_rejects_duplicate() {
        let mut g = KnowledgeGraph::in_memory();
        g.add_entity(alice()).unwrap();
        g.add_entity(acme()).unwrap();
        g.add_relation(Relation::new("alice", "acme", RelationKind::WorksAt))
            .unwrap();
        let err = g
            .add_relation(Relation::new("alice", "acme", RelationKind::WorksAt))
            .unwrap_err();
        assert!(matches!(err, GraphError::RelationAlreadyExists { .. }));
    }

    #[test]
    fn add_relation_increases_count() {
        let mut g = KnowledgeGraph::in_memory();
        g.add_entity(alice()).unwrap();
        g.add_entity(acme()).unwrap();
        g.add_relation(Relation::new("alice", "acme", RelationKind::WorksAt))
            .unwrap();
        assert_eq!(g.relation_count(), 1);
    }

    #[test]
    fn find_neighbors_depth_1_returns_direct_neighbors() {
        let mut g = KnowledgeGraph::in_memory();
        g.add_entity(alice()).unwrap();
        g.add_entity(acme()).unwrap();
        g.add_entity(rust_tech()).unwrap();
        g.add_relation(Relation::new("alice", "acme", RelationKind::WorksAt))
            .unwrap();
        g.add_relation(Relation::new("alice", "rust", RelationKind::DependsOn))
            .unwrap();

        let mut neighbors: Vec<_> = g
            .find_neighbors("alice", 1)
            .into_iter()
            .map(|e| e.id.as_str())
            .collect();
        neighbors.sort();
        assert_eq!(neighbors, vec!["acme", "rust"]);
    }

    #[test]
    fn find_neighbors_depth_2_reaches_two_hops() {
        let mut g = KnowledgeGraph::in_memory();
        g.add_entity(alice()).unwrap();
        g.add_entity(acme()).unwrap();
        let bob = Entity::new("bob", EntityKind::Person, "Bob");
        g.add_entity(bob).unwrap();
        g.add_relation(Relation::new("alice", "acme", RelationKind::WorksAt))
            .unwrap();
        g.add_relation(Relation::new("acme", "bob", RelationKind::Collaborates))
            .unwrap();

        let neighbors = g.find_neighbors("alice", 2);
        let ids: HashSet<&str> = neighbors.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains("acme"));
        assert!(ids.contains("bob"));
        assert!(!ids.contains("alice"));
    }

    #[test]
    fn find_neighbors_backward_edge_traversed_undirected() {
        let mut g = KnowledgeGraph::in_memory();
        g.add_entity(alice()).unwrap();
        g.add_entity(acme()).unwrap();
        // Relation goes acme → alice.
        g.add_relation(Relation::new("acme", "alice", RelationKind::CreatedBy))
            .unwrap();

        // BFS from alice should still reach acme via the backward edge.
        let neighbors: Vec<_> = g
            .find_neighbors("alice", 1)
            .iter()
            .map(|e| e.id.as_str())
            .collect();
        assert!(neighbors.contains(&"acme"));
    }

    #[test]
    fn find_neighbors_missing_entity_returns_empty() {
        let g = KnowledgeGraph::in_memory();
        assert!(g.find_neighbors("ghost", 1).is_empty());
    }

    #[test]
    fn find_by_kind_filters_correctly() {
        let mut g = KnowledgeGraph::in_memory();
        g.add_entity(alice()).unwrap();
        g.add_entity(acme()).unwrap();
        g.add_entity(rust_tech()).unwrap();

        let people = g.find_by_kind(&EntityKind::Person);
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].id, "alice");
    }

    #[test]
    fn find_by_attribute_returns_matching_entities() {
        let mut g = KnowledgeGraph::in_memory();
        let mut e = alice();
        e.set_attribute("dept", "engineering");
        g.add_entity(e).unwrap();

        let bob = Entity::new("bob", EntityKind::Person, "Bob");
        g.add_entity(bob).unwrap();

        let matched = g.find_by_attribute("dept", "engineering");
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].id, "alice");
    }

    #[test]
    fn all_entities_returns_sorted_by_id() {
        let mut g = KnowledgeGraph::in_memory();
        g.add_entity(rust_tech()).unwrap();
        g.add_entity(alice()).unwrap();
        g.add_entity(acme()).unwrap();

        let ids: Vec<&str> = g.all_entities().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["acme", "alice", "rust"]);
    }

    #[test]
    fn relations_for_returns_connected_relations() {
        let mut g = KnowledgeGraph::in_memory();
        g.add_entity(alice()).unwrap();
        g.add_entity(acme()).unwrap();
        g.add_entity(rust_tech()).unwrap();
        g.add_relation(Relation::new("alice", "acme", RelationKind::WorksAt))
            .unwrap();
        g.add_relation(Relation::new("alice", "rust", RelationKind::DependsOn))
            .unwrap();

        let rels = g.relations_for("alice");
        assert_eq!(rels.len(), 2);
    }

    #[test]
    fn remove_relation_decreases_count() {
        let mut g = KnowledgeGraph::in_memory();
        g.add_entity(alice()).unwrap();
        g.add_entity(acme()).unwrap();
        g.add_relation(Relation::new("alice", "acme", RelationKind::WorksAt))
            .unwrap();
        let removed = g.remove_relation("alice", "acme", &RelationKind::WorksAt);
        assert!(removed);
        assert_eq!(g.relation_count(), 0);
    }

    #[test]
    fn remove_relation_returns_false_when_not_present() {
        let mut g = KnowledgeGraph::in_memory();
        g.add_entity(alice()).unwrap();
        g.add_entity(acme()).unwrap();
        assert!(!g.remove_relation("alice", "acme", &RelationKind::WorksAt));
    }

    #[test]
    fn graph_persistence_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.json");

        {
            let mut g = KnowledgeGraph::open(&path).unwrap();
            g.add_entity(alice()).unwrap();
            g.add_entity(acme()).unwrap();
            g.add_relation(Relation::new("alice", "acme", RelationKind::WorksAt))
                .unwrap();
            g.flush().unwrap();
        }

        {
            let g = KnowledgeGraph::open(&path).unwrap();
            assert_eq!(g.entity_count(), 2);
            assert_eq!(g.relation_count(), 1);
            assert_eq!(g.get_entity("alice").unwrap().display_name, "Alice Smith");
        }
    }

    #[test]
    fn open_creates_empty_graph_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new_graph.json");
        let g = KnowledgeGraph::open(&path).unwrap();
        assert!(g.is_empty());
    }

    #[test]
    fn in_memory_graph_flush_is_no_op() {
        let mut g = KnowledgeGraph::in_memory();
        g.add_entity(alice()).unwrap();
        assert!(g.flush().is_ok());
    }
}
