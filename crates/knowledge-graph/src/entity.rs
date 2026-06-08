//! Entity types for the AnimaOS knowledge graph.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

/// The kind of a knowledge-graph entity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum EntityKind {
    Person,
    Place,
    Project,
    Concept,
    Technology,
    Organization,
    /// Operator-defined kind with an arbitrary label.
    Custom(String),
}

impl fmt::Display for EntityKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EntityKind::Person => write!(f, "person"),
            EntityKind::Place => write!(f, "place"),
            EntityKind::Project => write!(f, "project"),
            EntityKind::Concept => write!(f, "concept"),
            EntityKind::Technology => write!(f, "technology"),
            EntityKind::Organization => write!(f, "organization"),
            EntityKind::Custom(label) => write!(f, "custom:{label}"),
        }
    }
}

impl FromStr for EntityKind {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "person" => Ok(EntityKind::Person),
            "place" => Ok(EntityKind::Place),
            "project" => Ok(EntityKind::Project),
            "concept" => Ok(EntityKind::Concept),
            "technology" => Ok(EntityKind::Technology),
            "organization" | "org" => Ok(EntityKind::Organization),
            other => {
                let label = other.strip_prefix("custom:").unwrap_or(other);
                if label.is_empty() {
                    Err(())
                } else {
                    Ok(EntityKind::Custom(label.to_string()))
                }
            }
        }
    }
}

/// A node in the knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    /// Stable identifier for this entity.
    pub id: String,
    /// Semantic kind.
    pub kind: EntityKind,
    /// Human-readable label.
    pub display_name: String,
    /// Freeform key-value attributes.
    pub attributes: HashMap<String, String>,
    /// Unix nanoseconds at creation time.
    pub created_at_ns: u64,
}

impl Entity {
    /// Create a new entity with an empty attribute map.
    pub fn new(id: impl Into<String>, kind: EntityKind, display_name: impl Into<String>) -> Self {
        Entity {
            id: id.into(),
            kind,
            display_name: display_name.into(),
            attributes: HashMap::new(),
            created_at_ns: now_ns(),
        }
    }

    /// Insert or update an attribute; returns the previous value if any.
    pub fn set_attribute(
        &mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Option<String> {
        self.attributes.insert(key.into(), value.into())
    }

    /// Retrieve an attribute value.
    pub fn get_attribute(&self, key: &str) -> Option<&str> {
        self.attributes.get(key).map(String::as_str)
    }
}

fn now_ns() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_new_has_empty_attributes() {
        let e = Entity::new("alice", EntityKind::Person, "Alice Smith");
        assert_eq!(e.id, "alice");
        assert!(matches!(e.kind, EntityKind::Person));
        assert_eq!(e.display_name, "Alice Smith");
        assert!(e.attributes.is_empty());
    }

    #[test]
    fn set_and_get_attribute_round_trips() {
        let mut e = Entity::new("proj", EntityKind::Project, "MyProject");
        let prev = e.set_attribute("lang", "Rust");
        assert!(prev.is_none());
        assert_eq!(e.get_attribute("lang"), Some("Rust"));
    }

    #[test]
    fn set_attribute_returns_previous_value() {
        let mut e = Entity::new("proj", EntityKind::Project, "MyProject");
        e.set_attribute("lang", "Rust");
        let prev = e.set_attribute("lang", "Python");
        assert_eq!(prev.as_deref(), Some("Rust"));
        assert_eq!(e.get_attribute("lang"), Some("Python"));
    }

    #[test]
    fn get_attribute_returns_none_for_missing_key() {
        let e = Entity::new("x", EntityKind::Concept, "X");
        assert!(e.get_attribute("nonexistent").is_none());
    }

    #[test]
    fn entity_kind_display_and_from_str_round_trip() {
        let kinds = [
            EntityKind::Person,
            EntityKind::Place,
            EntityKind::Project,
            EntityKind::Concept,
            EntityKind::Technology,
            EntityKind::Organization,
            EntityKind::Custom("dataset".to_string()),
        ];
        for kind in &kinds {
            let s = kind.to_string();
            let parsed: EntityKind = s.parse().expect("should parse");
            assert_eq!(&parsed, kind, "round-trip failed for {s}");
        }
    }

    #[test]
    fn entity_kind_from_str_rejects_empty_custom_label() {
        assert!("custom:".parse::<EntityKind>().is_err());
    }

    #[test]
    fn entity_kind_org_alias_parses() {
        let k: EntityKind = "org".parse().unwrap();
        assert_eq!(k, EntityKind::Organization);
    }

    #[test]
    fn entity_serializes_and_deserializes_through_json() {
        let mut e = Entity::new("rust-lang", EntityKind::Technology, "Rust");
        e.set_attribute("version", "1.78");
        let json = serde_json::to_string(&e).unwrap();
        let back: Entity = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, e.id);
        assert_eq!(back.display_name, e.display_name);
        assert_eq!(back.get_attribute("version"), Some("1.78"));
    }
}
