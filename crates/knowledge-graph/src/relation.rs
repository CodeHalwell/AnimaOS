//! Relation (edge) types for the AnimaOS knowledge graph.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// The kind of a directed relationship between two entities.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum RelationKind {
    /// Subject works at / is employed by the object.
    WorksAt,
    /// Subject is semantically related to the object.
    RelatedTo,
    /// Subject is a component or sub-item of the object.
    PartOf,
    /// Subject was created by the object.
    CreatedBy,
    /// Subject depends on the object (build / runtime dependency).
    DependsOn,
    /// Subject collaborates with the object.
    Collaborates,
    /// Subject is an instance of the object (type relationship).
    IsA,
    /// Operator-defined relationship kind.
    Custom(String),
}

impl fmt::Display for RelationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RelationKind::WorksAt => write!(f, "works_at"),
            RelationKind::RelatedTo => write!(f, "related_to"),
            RelationKind::PartOf => write!(f, "part_of"),
            RelationKind::CreatedBy => write!(f, "created_by"),
            RelationKind::DependsOn => write!(f, "depends_on"),
            RelationKind::Collaborates => write!(f, "collaborates"),
            RelationKind::IsA => write!(f, "is_a"),
            RelationKind::Custom(label) => write!(f, "custom:{label}"),
        }
    }
}

impl FromStr for RelationKind {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "works_at" | "works-at" => Ok(RelationKind::WorksAt),
            "related_to" | "related-to" => Ok(RelationKind::RelatedTo),
            "part_of" | "part-of" => Ok(RelationKind::PartOf),
            "created_by" | "created-by" => Ok(RelationKind::CreatedBy),
            "depends_on" | "depends-on" => Ok(RelationKind::DependsOn),
            "collaborates" => Ok(RelationKind::Collaborates),
            "is_a" | "is-a" => Ok(RelationKind::IsA),
            other => {
                let label = other.strip_prefix("custom:").unwrap_or(other);
                if label.is_empty() {
                    Err(())
                } else {
                    Ok(RelationKind::Custom(label.to_string()))
                }
            }
        }
    }
}

/// A directed, weighted edge in the knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relation {
    /// Source entity id.
    pub from: String,
    /// Target entity id.
    pub to: String,
    /// Semantic kind of this relationship.
    pub kind: RelationKind,
    /// Salience / confidence weight in `[0.0, 1.0]`.
    pub weight: f32,
    /// Unix nanoseconds at creation time.
    pub created_at_ns: u64,
}

impl Relation {
    /// Create a relation with the default weight of `1.0`.
    pub fn new(from: impl Into<String>, to: impl Into<String>, kind: RelationKind) -> Self {
        Relation {
            from: from.into(),
            to: to.into(),
            kind,
            weight: 1.0,
            created_at_ns: now_ns(),
        }
    }

    /// Create a relation with an explicit weight, clamped to `[0.0, 1.0]`.
    pub fn with_weight(
        from: impl Into<String>,
        to: impl Into<String>,
        kind: RelationKind,
        weight: f32,
    ) -> Self {
        Relation {
            from: from.into(),
            to: to.into(),
            kind,
            weight: weight.clamp(0.0, 1.0),
            created_at_ns: now_ns(),
        }
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
    fn relation_new_has_weight_one() {
        let r = Relation::new("a", "b", RelationKind::RelatedTo);
        assert_eq!(r.from, "a");
        assert_eq!(r.to, "b");
        assert_eq!(r.weight, 1.0);
    }

    #[test]
    fn relation_weight_is_clamped_above_one() {
        let r = Relation::with_weight("a", "b", RelationKind::DependsOn, 2.5);
        assert_eq!(r.weight, 1.0);
    }

    #[test]
    fn relation_weight_is_clamped_below_zero() {
        let r = Relation::with_weight("a", "b", RelationKind::DependsOn, -0.5);
        assert_eq!(r.weight, 0.0);
    }

    #[test]
    fn relation_kind_display_and_from_str_round_trip() {
        let kinds = [
            RelationKind::WorksAt,
            RelationKind::RelatedTo,
            RelationKind::PartOf,
            RelationKind::CreatedBy,
            RelationKind::DependsOn,
            RelationKind::Collaborates,
            RelationKind::IsA,
            RelationKind::Custom("owns".to_string()),
        ];
        for kind in &kinds {
            let s = kind.to_string();
            let parsed: RelationKind = s.parse().expect("should parse");
            assert_eq!(&parsed, kind, "round-trip failed for {s}");
        }
    }

    #[test]
    fn relation_kind_hyphenated_aliases_parse() {
        assert_eq!(
            "works-at".parse::<RelationKind>().unwrap(),
            RelationKind::WorksAt
        );
        assert_eq!(
            "part-of".parse::<RelationKind>().unwrap(),
            RelationKind::PartOf
        );
        assert_eq!(
            "created-by".parse::<RelationKind>().unwrap(),
            RelationKind::CreatedBy
        );
        assert_eq!(
            "depends-on".parse::<RelationKind>().unwrap(),
            RelationKind::DependsOn
        );
        assert_eq!("is-a".parse::<RelationKind>().unwrap(), RelationKind::IsA);
    }

    #[test]
    fn relation_kind_from_str_rejects_empty_custom_label() {
        assert!("custom:".parse::<RelationKind>().is_err());
    }

    #[test]
    fn relation_serializes_and_deserializes_through_json() {
        let r = Relation::with_weight("alice", "acme", RelationKind::WorksAt, 0.9);
        let json = serde_json::to_string(&r).unwrap();
        let back: Relation = serde_json::from_str(&json).unwrap();
        assert_eq!(back.from, "alice");
        assert_eq!(back.to, "acme");
        assert_eq!(back.weight, 0.9);
    }
}
