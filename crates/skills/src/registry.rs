//! Skill registry with three-stage progressive disclosure (S11.1).
//!
//! ## Progressive disclosure
//!
//! | Stage | What is loaded | When |
//! |---|---|---|
//! | 1 | `name` + `description` only | Always — on every cortex context build |
//! | 2 | Full `SkillBody` (instructions) | When the cortex selects a skill |
//! | 3 | Linked files referenced by the body | When the body's procedure needs them |
//!
//! Skill selection between stages 1 and 2 reuses `length_robust_filter`
//! from `praxis::routing` — the same scorer used for tool selection in E2.3.

use std::collections::HashSet;

use praxis::routing::{length_robust_filter, ToolCandidate};

use crate::builtins::BUILTIN_SKILLS;
use crate::manifest::{ParseError, SkillBody, SkillManifest};
use crate::provenance::{SkillAuthor, SkillProvenance, SkillState};

// ── SkillEntry ────────────────────────────────────────────────────────────────

/// One entry in the registry.
#[derive(Debug, Clone)]
pub struct SkillEntry {
    /// Stable identifier: `name` lowercased with spaces replaced by `-`.
    pub id: String,
    /// Eagerly-loaded manifest (stage-1 data).
    pub manifest: SkillManifest,
    /// Stage-2 body, loaded on demand or pre-loaded for built-ins.
    pub body: Option<SkillBody>,
    /// Immutable provenance.
    pub provenance: SkillProvenance,
    /// Mutable lifecycle state.
    pub state: SkillState,
}

impl SkillEntry {
    /// Derive a stable ID from a skill name.
    pub fn id_from_name(name: &str) -> String {
        name.to_lowercase().replace(' ', "-")
    }
}

// ── RegistryError ─────────────────────────────────────────────────────────────

/// Errors produced by the skill registry.
#[derive(Debug, Clone, PartialEq)]
pub enum RegistryError {
    /// A skill with this ID already exists.
    DuplicateId(String),
    /// The SKILL.md text could not be parsed.
    ParseFailed(ParseError),
    /// No skill with this ID exists.
    NotFound(String),
    /// Body has not been loaded for this skill.
    BodyNotLoaded(String),
}

impl core::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            RegistryError::DuplicateId(id) => write!(f, "skill id already registered: {id}"),
            RegistryError::ParseFailed(e) => write!(f, "SKILL.md parse error: {e}"),
            RegistryError::NotFound(id) => write!(f, "skill not found: {id}"),
            RegistryError::BodyNotLoaded(id) => write!(f, "skill body not loaded: {id}"),
        }
    }
}

// ── SkillRegistry ─────────────────────────────────────────────────────────────

/// The skill registry (S11.1).
///
/// All mutations go through `register`, `promote`, `rollback`, `quarantine`,
/// and `kill_switch` so the caller can emit the appropriate audit entries
/// before and after each state change.
#[derive(Debug, Default)]
pub struct SkillRegistry {
    entries: Vec<SkillEntry>,
}

impl SkillRegistry {
    /// Creates a registry pre-populated with the four built-in skills.
    pub fn with_builtins() -> Self {
        let mut registry = SkillRegistry::default();
        for skill in BUILTIN_SKILLS {
            let manifest = SkillManifest {
                name: skill.name.to_string(),
                description: skill.description.to_string(),
                version: Some("builtin".to_string()),
                capabilities: skill.capabilities.iter().map(|s| s.to_string()).collect(),
            };
            let body = SkillBody {
                manifest: manifest.clone(),
                instructions: skill.instructions.to_string(),
                linked_files: Vec::new(),
            };
            let id = SkillEntry::id_from_name(&manifest.name);
            registry.entries.push(SkillEntry {
                id,
                manifest,
                body: Some(body),
                provenance: SkillProvenance::builtin(),
                state: SkillState::Active,
            });
        }
        registry
    }

    // ── Registration ─────────────────────────────────────────────────────────

    /// Register a skill from a raw SKILL.md text.
    ///
    /// Returns the new skill's stable `id` on success.
    pub fn register_from_text(
        &mut self,
        text: &str,
        provenance: SkillProvenance,
        state: SkillState,
    ) -> Result<String, RegistryError> {
        let body = SkillBody::parse(text).map_err(RegistryError::ParseFailed)?;
        let manifest = body.manifest.clone();
        self.register(manifest, Some(body), provenance, state)
    }

    /// Register a skill from an explicit manifest + optional body.
    ///
    /// Returns the new skill's stable `id` on success.
    pub fn register(
        &mut self,
        manifest: SkillManifest,
        body: Option<SkillBody>,
        provenance: SkillProvenance,
        state: SkillState,
    ) -> Result<String, RegistryError> {
        let id = SkillEntry::id_from_name(&manifest.name);
        if self.entries.iter().any(|e| e.id == id) {
            return Err(RegistryError::DuplicateId(id));
        }
        self.entries.push(SkillEntry {
            id: id.clone(),
            manifest,
            body,
            provenance,
            state,
        });
        Ok(id)
    }

    // ── Lifecycle ─────────────────────────────────────────────────────────────

    /// Promote a `Proposed` skill to `Active`.
    pub fn promote(&mut self, id: &str) -> Result<(), RegistryError> {
        let entry = self.find_mut(id)?;
        entry.state = SkillState::Active;
        Ok(())
    }

    /// Roll back a skill to `RolledBack` state (no longer selectable).
    pub fn rollback(&mut self, id: &str) -> Result<(), RegistryError> {
        let entry = self.find_mut(id)?;
        entry.state = SkillState::RolledBack;
        Ok(())
    }

    /// Quarantine a skill with a human-readable reason.
    pub fn quarantine(&mut self, id: &str, reason: impl Into<String>) -> Result<(), RegistryError> {
        let entry = self.find_mut(id)?;
        entry.state = SkillState::Quarantined {
            reason: reason.into(),
        };
        Ok(())
    }

    /// Kill switch: quarantine all active agent-authored skills.
    ///
    /// Built-in and operator skills are unaffected.  Returns the IDs of
    /// every skill that was quarantined.
    pub fn kill_switch(&mut self, reason: &str) -> Vec<String> {
        let mut affected = Vec::new();
        for entry in &mut self.entries {
            if entry.state.is_active() && entry.provenance.authored_by == SkillAuthor::Agent {
                entry.state = SkillState::Quarantined {
                    reason: reason.to_string(),
                };
                affected.push(entry.id.clone());
            }
        }
        affected
    }

    // ── Stage-1 queries (metadata only) ──────────────────────────────────────

    /// Returns metadata for all active skills (stage-1 progressive disclosure).
    pub fn list_active(&self) -> Vec<&SkillManifest> {
        self.entries
            .iter()
            .filter(|e| e.state.is_active())
            .map(|e| &e.manifest)
            .collect()
    }

    /// Returns all entries regardless of lifecycle state.
    pub fn list_all(&self) -> Vec<&SkillEntry> {
        self.entries.iter().collect()
    }

    /// Total number of registered skills.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when no skills have been registered.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    // ── Stage-2 queries (body on demand) ─────────────────────────────────────

    /// Returns the full body for a skill (stage-2 progressive disclosure).
    pub fn load_body(&self, id: &str) -> Result<&SkillBody, RegistryError> {
        let entry = self.find(id)?;
        entry
            .body
            .as_ref()
            .ok_or_else(|| RegistryError::BodyNotLoaded(id.to_string()))
    }

    // ── Semantic selection ────────────────────────────────────────────────────

    /// Select skills relevant to a task description using `length_robust_filter`.
    ///
    /// `tau_rel` is the relative-score threshold (`0.85` keeps skills within
    /// 15 % of the highest-scoring candidate).  Returns only active skills.
    pub fn select_for_task(&self, task_description: &str, tau_rel: f32) -> Vec<&SkillManifest> {
        let active: Vec<&SkillEntry> = self
            .entries
            .iter()
            .filter(|e| e.state.is_active())
            .collect();
        if active.is_empty() {
            return Vec::new();
        }

        let task_tokens: Vec<&str> = task_description.split_whitespace().collect();
        let candidates: Vec<ToolCandidate> = active
            .iter()
            .map(|e| ToolCandidate {
                id: e.id.clone(),
                score: token_overlap_score(&task_tokens, &e.manifest.description),
            })
            .collect();

        let kept = length_robust_filter(&candidates, tau_rel);
        let kept_ids: HashSet<&str> = kept.iter().map(|c| c.id.as_str()).collect();

        active
            .iter()
            .filter(|e| kept_ids.contains(e.id.as_str()))
            .map(|e| &e.manifest)
            .collect()
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    fn find(&self, id: &str) -> Result<&SkillEntry, RegistryError> {
        self.entries
            .iter()
            .find(|e| e.id == id)
            .ok_or_else(|| RegistryError::NotFound(id.to_string()))
    }

    fn find_mut(&mut self, id: &str) -> Result<&mut SkillEntry, RegistryError> {
        self.entries
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or_else(|| RegistryError::NotFound(id.to_string()))
    }
}

/// Jaccard-like token overlap score between task tokens and a description.
fn token_overlap_score(task_tokens: &[&str], description: &str) -> f32 {
    let desc_tokens: Vec<&str> = description.split_whitespace().collect();
    if task_tokens.is_empty() || desc_tokens.is_empty() {
        return 0.0;
    }
    let task_lower: HashSet<String> = task_tokens.iter().map(|t| t.to_lowercase()).collect();
    let overlap = desc_tokens
        .iter()
        .filter(|t| task_lower.contains(&t.to_lowercase()))
        .count();
    let union = task_tokens.len() + desc_tokens.len() - overlap;
    if union == 0 {
        0.0
    } else {
        overlap as f32 / union as f32
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_skill_text(name: &str) -> String {
        format!(
            "---\nname: {name}\ndescription: A skill for testing {name} functionality.\n---\n\nDo the thing.\n"
        )
    }

    #[test]
    fn registry_with_builtins_loads_four_skills() {
        let reg = SkillRegistry::with_builtins();
        assert_eq!(reg.len(), 4);
        assert_eq!(reg.list_active().len(), 4);
    }

    #[test]
    fn register_from_text_adds_skill() {
        let mut reg = SkillRegistry::default();
        let id = reg
            .register_from_text(
                &sample_skill_text("my-skill"),
                SkillProvenance::operator(1000),
                SkillState::Active,
            )
            .unwrap();
        assert_eq!(id, "my-skill");
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn duplicate_id_returns_error() {
        let mut reg = SkillRegistry::default();
        reg.register_from_text(
            &sample_skill_text("alpha"),
            SkillProvenance::operator(1),
            SkillState::Active,
        )
        .unwrap();
        let err = reg
            .register_from_text(
                &sample_skill_text("alpha"),
                SkillProvenance::operator(2),
                SkillState::Active,
            )
            .unwrap_err();
        assert_eq!(err, RegistryError::DuplicateId("alpha".to_string()));
    }

    #[test]
    fn proposed_skill_not_in_active_list() {
        let mut reg = SkillRegistry::default();
        reg.register_from_text(
            &sample_skill_text("pending"),
            SkillProvenance::agent(1, "ep-1"),
            SkillState::Proposed,
        )
        .unwrap();
        assert_eq!(reg.list_active().len(), 0);
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn promote_moves_proposed_to_active() {
        let mut reg = SkillRegistry::default();
        reg.register_from_text(
            &sample_skill_text("pending"),
            SkillProvenance::agent(1, "ep-1"),
            SkillState::Proposed,
        )
        .unwrap();
        reg.promote("pending").unwrap();
        assert_eq!(reg.list_active().len(), 1);
    }

    #[test]
    fn rollback_moves_active_to_rolled_back() {
        let mut reg = SkillRegistry::default();
        reg.register_from_text(
            &sample_skill_text("live"),
            SkillProvenance::operator(1),
            SkillState::Active,
        )
        .unwrap();
        reg.rollback("live").unwrap();
        assert_eq!(reg.list_active().len(), 0);
        let entry = reg.find("live").unwrap();
        assert_eq!(entry.state, SkillState::RolledBack);
    }

    #[test]
    fn quarantine_blocks_skill_from_active_list() {
        let mut reg = SkillRegistry::default();
        reg.register_from_text(
            &sample_skill_text("live"),
            SkillProvenance::operator(1),
            SkillState::Active,
        )
        .unwrap();
        reg.quarantine("live", "defence flag").unwrap();
        assert_eq!(reg.list_active().len(), 0);
        assert!(reg.find("live").unwrap().state.is_quarantined());
    }

    #[test]
    fn kill_switch_quarantines_only_agent_skills() {
        let mut reg = SkillRegistry::default(); // builtins not present here
        reg.register_from_text(
            &sample_skill_text("op-skill"),
            SkillProvenance::operator(1),
            SkillState::Active,
        )
        .unwrap();
        reg.register_from_text(
            &sample_skill_text("agent-skill"),
            SkillProvenance::agent(2, "ep-1"),
            SkillState::Active,
        )
        .unwrap();
        let affected = reg.kill_switch("emergency");
        assert_eq!(affected, vec!["agent-skill"]);
        // Operator skill is still active.
        assert!(reg.find("op-skill").unwrap().state.is_active());
        // Agent skill is quarantined.
        assert!(reg.find("agent-skill").unwrap().state.is_quarantined());
    }

    #[test]
    fn load_body_returns_instructions() {
        let reg = SkillRegistry::with_builtins();
        let body = reg.load_body("web-research").unwrap();
        assert!(!body.instructions.is_empty());
    }

    #[test]
    fn select_for_task_returns_relevant_skills() {
        let reg = SkillRegistry::with_builtins();
        // "search web research" should score highly against the web-research skill.
        let selected = reg.select_for_task("search web research query", 0.5);
        let names: Vec<&str> = selected.iter().map(|m| m.name.as_str()).collect();
        assert!(
            names.contains(&"web-research"),
            "expected web-research in: {names:?}"
        );
    }

    #[test]
    fn select_for_task_with_tight_threshold_narrows_results() {
        let mut reg = SkillRegistry::with_builtins();
        // Add a highly specific skill.
        reg.register_from_text(
            "---\nname: calendar-booking\ndescription: Books calendar appointments and meetings.\n---\nBook it.\n",
            SkillProvenance::operator(1),
            SkillState::Active,
        )
        .unwrap();
        // A calendar query should prefer the calendar skill.
        let selected = reg.select_for_task("book a calendar appointment", 0.9);
        let names: Vec<&str> = selected.iter().map(|m| m.name.as_str()).collect();
        assert!(
            names.contains(&"calendar-booking"),
            "expected calendar-booking in: {names:?}"
        );
    }

    #[test]
    fn not_found_returns_error() {
        let reg = SkillRegistry::default();
        let result = reg.load_body("nonexistent");
        assert!(matches!(result, Err(RegistryError::NotFound(id)) if id == "nonexistent"));
    }
}
