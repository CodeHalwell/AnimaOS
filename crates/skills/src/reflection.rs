//! Self-improvement loop — episode reflection (S11.5).
//!
//! During the dreaming / consolidation sleep phase the agent reflects on
//! recent episode summaries and proposes new skills to collapse recurring
//! friction patterns.  This module provides the reflection logic; the vita
//! layer wires it into the sleep cycle.

use serde::{Deserialize, Serialize};

// ── FrictionPattern ───────────────────────────────────────────────────────────

/// A recurring friction pattern identified across multiple episodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrictionPattern {
    /// Short description of the pattern (e.g. "repeatedly assembles web-search + summarise pipeline").
    pub description: String,
    /// How many episodes exhibit this pattern.
    pub occurrence_count: usize,
    /// IDs of the episodes where this pattern was observed.
    pub episode_ids: Vec<String>,
    /// Suggested skill name if a new skill would address this pattern.
    pub suggested_skill_name: Option<String>,
}

// ── ReflectionConfig ──────────────────────────────────────────────────────────

/// Parameters for the self-improvement reflection pass.
#[derive(Debug, Clone)]
pub struct ReflectionConfig {
    /// Minimum number of occurrences before a pattern is reported.
    pub min_occurrence_threshold: usize,
    /// Maximum number of friction patterns to report per pass.
    pub max_patterns: usize,
    /// Minimum number of episodes to analyse before reporting patterns.
    pub min_episodes: usize,
}

impl Default for ReflectionConfig {
    fn default() -> Self {
        ReflectionConfig {
            min_occurrence_threshold: 2,
            max_patterns: 5,
            min_episodes: 3,
        }
    }
}

// ── EpisodeSummary ────────────────────────────────────────────────────────────

/// Lightweight episode descriptor used for reflection input.
#[derive(Debug, Clone)]
pub struct EpisodeSummary {
    /// Stable episode identifier.
    pub episode_id: String,
    /// Free-text summary of what happened in the episode.
    pub summary: String,
    /// Names of tools called during the episode.
    pub tools_used: Vec<String>,
    /// Whether the episode succeeded.
    pub success: bool,
}

// ── ReflectionReport ─────────────────────────────────────────────────────────

/// Output of a reflection pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectionReport {
    /// Number of episodes analysed.
    pub episodes_analysed: usize,
    /// Identified friction patterns (ordered by occurrence count, descending).
    pub patterns: Vec<FrictionPattern>,
    /// How many new skill proposals were generated.
    pub proposals_generated: usize,
}

// ── reflect_on_episodes ───────────────────────────────────────────────────────

/// Reflect on a set of episode summaries and identify recurring friction patterns.
///
/// Currently applies simple co-occurrence analysis: if the same pair of tools is
/// called together in at least `min_occurrence_threshold` episodes, that is
/// flagged as a candidate for a new skill.
///
/// This is the entry point for S11.5: the vita layer calls this from the
/// dreaming/consolidation phase, collects `FrictionPattern`s with
/// `suggested_skill_name`, and queues them as `SkillProposal`s for the next
/// wake cycle.
pub fn reflect_on_episodes(
    episodes: &[EpisodeSummary],
    config: &ReflectionConfig,
) -> ReflectionReport {
    if episodes.len() < config.min_episodes {
        return ReflectionReport {
            episodes_analysed: episodes.len(),
            patterns: Vec::new(),
            proposals_generated: 0,
        };
    }

    let mut patterns = find_tool_co_occurrence_patterns(episodes, config.min_occurrence_threshold);

    // Sort by occurrence count descending, then alphabetically by description.
    patterns.sort_by(|a, b| {
        b.occurrence_count
            .cmp(&a.occurrence_count)
            .then(a.description.cmp(&b.description))
    });
    patterns.truncate(config.max_patterns);

    let proposals_generated = patterns
        .iter()
        .filter(|p| p.suggested_skill_name.is_some())
        .count();

    ReflectionReport {
        episodes_analysed: episodes.len(),
        patterns,
        proposals_generated,
    }
}

/// Find tool pairs that co-occur in at least `threshold` episodes.
fn find_tool_co_occurrence_patterns(
    episodes: &[EpisodeSummary],
    threshold: usize,
) -> Vec<FrictionPattern> {
    use std::collections::HashMap;

    // Map each tool-pair (sorted) to the episodes in which it appeared.
    let mut pair_episodes: HashMap<(String, String), Vec<String>> = HashMap::new();

    for ep in episodes {
        let mut tools = ep.tools_used.clone();
        tools.sort_unstable();
        tools.dedup();

        // All pairs in this episode.
        for i in 0..tools.len() {
            for j in (i + 1)..tools.len() {
                let key = (tools[i].clone(), tools[j].clone());
                pair_episodes
                    .entry(key)
                    .or_default()
                    .push(ep.episode_id.clone());
            }
        }
    }

    let mut patterns = Vec::new();
    for ((tool_a, tool_b), ep_ids) in pair_episodes {
        if ep_ids.len() >= threshold {
            let skill_name =
                format!("{}-and-{}", tool_a.replace('_', "-"), tool_b.replace('_', "-"));
            patterns.push(FrictionPattern {
                description: format!(
                    "tools '{tool_a}' and '{tool_b}' called together in {} episodes",
                    ep_ids.len()
                ),
                occurrence_count: ep_ids.len(),
                episode_ids: ep_ids,
                suggested_skill_name: Some(skill_name),
            });
        }
    }

    patterns
}

// ── generate_skill_draft ─────────────────────────────────────────────────────

/// Generate a SKILL.md draft for a friction pattern.
///
/// The resulting text is suitable for passing to
/// `SkillRegistry::register_from_text` after operator or auto-promotion
/// screening.
pub fn generate_skill_draft(pattern: &FrictionPattern) -> String {
    let name = pattern
        .suggested_skill_name
        .as_deref()
        .unwrap_or("auto-generated-skill");

    format!(
        "\
---
name: {name}
description: Automatically generated skill to handle the pattern: {desc}
version: 0.1.0-draft
capabilities:
---

## Context

This skill was proposed by the self-improvement loop after observing the
following recurring pattern in {count} episode(s):

{desc}

## Procedure

1. Review the relevant tool outputs and combine them efficiently.
2. Apply domain-specific logic as needed.
3. Return a concise, structured result.

## Notes

- This is an auto-generated draft.  Review and refine before activating.
- Source episodes: {episodes}
",
        name = name,
        desc = pattern.description,
        count = pattern.occurrence_count,
        episodes = pattern.episode_ids.join(", "),
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_episode(id: &str, tools: &[&str], success: bool) -> EpisodeSummary {
        EpisodeSummary {
            episode_id: id.to_string(),
            summary: format!("Episode {id}"),
            tools_used: tools.iter().map(|t| t.to_string()).collect(),
            success,
        }
    }

    #[test]
    fn reflect_returns_empty_when_too_few_episodes() {
        let episodes = vec![
            make_episode("e1", &["web-search"], true),
            make_episode("e2", &["web-search"], true),
        ];
        let report = reflect_on_episodes(&episodes, &ReflectionConfig::default());
        assert_eq!(report.episodes_analysed, 2);
        assert!(report.patterns.is_empty());
    }

    #[test]
    fn reflect_identifies_tool_co_occurrence_pattern() {
        let episodes = vec![
            make_episode("e1", &["web-search", "summarise"], true),
            make_episode("e2", &["web-search", "summarise"], true),
            make_episode("e3", &["web-search", "summarise"], true),
        ];
        let report = reflect_on_episodes(&episodes, &ReflectionConfig::default());
        assert_eq!(report.episodes_analysed, 3);
        assert!(!report.patterns.is_empty());
        let p = &report.patterns[0];
        assert_eq!(p.occurrence_count, 3);
        assert!(p.suggested_skill_name.is_some());
        assert_eq!(report.proposals_generated, 1);
    }

    #[test]
    fn reflect_does_not_report_below_threshold() {
        let episodes = vec![
            make_episode("e1", &["tool-a", "tool-b"], true),
            make_episode("e2", &["tool-a"], true),
            make_episode("e3", &["tool-c"], true),
        ];
        let config = ReflectionConfig {
            min_occurrence_threshold: 2,
            min_episodes: 2,
            max_patterns: 5,
        };
        let report = reflect_on_episodes(&episodes, &config);
        // tool-a and tool-b co-occur only once — below threshold.
        assert!(report.patterns.is_empty());
    }

    #[test]
    fn reflect_respects_max_patterns_limit() {
        // Create 6 distinct tool pairs that each appear 3 times.
        let tool_pairs: Vec<(&str, &str)> = vec![
            ("a", "b"),
            ("c", "d"),
            ("e", "f"),
            ("g", "h"),
            ("i", "j"),
            ("k", "l"),
        ];
        let mut episodes = Vec::new();
        for (idx, (t1, t2)) in tool_pairs.iter().enumerate() {
            for rep in 0..3 {
                episodes.push(make_episode(
                    &format!("e-{idx}-{rep}"),
                    &[t1, t2],
                    true,
                ));
            }
        }
        let config = ReflectionConfig {
            min_occurrence_threshold: 2,
            min_episodes: 1,
            max_patterns: 3,
        };
        let report = reflect_on_episodes(&episodes, &config);
        assert!(report.patterns.len() <= 3);
    }

    #[test]
    fn generate_skill_draft_produces_valid_skill_text() {
        let pattern = FrictionPattern {
            description: "tools 'search' and 'summarise' called together".to_string(),
            occurrence_count: 4,
            episode_ids: vec!["ep-1".to_string(), "ep-2".to_string()],
            suggested_skill_name: Some("search-and-summarise".to_string()),
        };
        let draft = generate_skill_draft(&pattern);
        assert!(draft.contains("search-and-summarise"));
        assert!(draft.contains("---"), "draft should have frontmatter");
    }

    #[test]
    fn patterns_sorted_by_occurrence_count_descending() {
        let episodes: Vec<EpisodeSummary> = (0..5)
            .map(|i| make_episode(&format!("e{i}"), &["tool-x", "tool-y"], true))
            .chain(
                (0..3).map(|i| make_episode(&format!("f{i}"), &["tool-a", "tool-b"], true)),
            )
            .collect();
        let report = reflect_on_episodes(
            &episodes,
            &ReflectionConfig {
                min_occurrence_threshold: 2,
                min_episodes: 1,
                max_patterns: 10,
            },
        );
        if report.patterns.len() >= 2 {
            assert!(
                report.patterns[0].occurrence_count >= report.patterns[1].occurrence_count,
                "patterns not sorted by occurrence count"
            );
        }
    }
}
