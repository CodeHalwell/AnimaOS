//! SKILL.md parsing — manifest (frontmatter) and body.
//!
//! A SKILL.md file has the following layout:
//!
//! ```text
//! ---
//! name: web-research
//! description: Search the web and summarise findings for a query.
//! version: 0.1.0
//! capabilities: network.read
//! ---
//!
//! ## Procedure
//! ...instruction body...
//! ```
//!
//! `SkillManifest` holds only the frontmatter fields and is always loaded.
//! `SkillBody` additionally carries the instruction prose and is loaded on
//! demand when the cortex selects the skill (stage-2 progressive disclosure).

use serde::{Deserialize, Serialize};

// ── SkillManifest ─────────────────────────────────────────────────────────────

/// Lightweight skill descriptor — the only part loaded eagerly.
///
/// Exposed to the cortex as a compact list so the agent can decide which skills
/// are relevant without loading every instruction body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillManifest {
    /// Unique display name for the skill.
    pub name: String,
    /// One-to-two sentence description used for semantic skill selection.
    pub description: String,
    /// Optional semver version string.
    pub version: Option<String>,
    /// Capability names required by this skill (checked against `anima-self`).
    pub capabilities: Vec<String>,
}

impl SkillManifest {
    /// Parse the frontmatter of a SKILL.md text.
    ///
    /// Returns `(manifest, remaining_body)` on success.  The body starts
    /// immediately after the closing `---` line.
    pub fn from_frontmatter(text: &str) -> Result<(Self, &str), ParseError> {
        let text = text.trim_start_matches('\n');
        let rest = text
            .strip_prefix("---")
            .ok_or(ParseError::MissingFrontmatter)?;

        // Accept `---\n` or `---` (end of string).
        let rest = rest.strip_prefix('\n').unwrap_or(rest);

        let (front, after_close) = rest
            .split_once("\n---")
            .ok_or(ParseError::UnclosedFrontmatter)?;

        // Strip leading newline from body.
        let body = after_close.strip_prefix('\n').unwrap_or(after_close);

        let mut name = None;
        let mut description = None;
        let mut version = None;
        let mut capabilities = Vec::new();

        for line in front.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once(':') {
                let k = k.trim();
                let v = v.trim();
                match k {
                    "name" => name = Some(v.to_string()),
                    "description" => description = Some(v.to_string()),
                    "version" => version = Some(v.to_string()),
                    "capabilities" => {
                        for cap in v.split(',') {
                            let cap = cap.trim();
                            if !cap.is_empty() {
                                capabilities.push(cap.to_string());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        let manifest = SkillManifest {
            name: name.ok_or(ParseError::MissingName)?,
            description: description.unwrap_or_default(),
            version,
            capabilities,
        };

        Ok((manifest, body))
    }
}

// ── SkillBody ─────────────────────────────────────────────────────────────────

/// Full skill content — loaded on demand when the skill is selected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillBody {
    /// Metadata parsed from the frontmatter.
    pub manifest: SkillManifest,
    /// Instruction prose from the SKILL.md body.
    pub instructions: String,
    /// Filenames of linked resources (relative to the skill directory).
    ///
    /// These are returned on stage-3 demand: the caller reads the file from
    /// disk and injects its contents only when the body explicitly references it.
    pub linked_files: Vec<String>,
}

impl SkillBody {
    /// Parse a complete SKILL.md text (frontmatter + body).
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        let (manifest, body_text) = SkillManifest::from_frontmatter(text)?;
        let instructions = body_text.to_string();
        let linked_files = extract_local_links(&instructions);
        Ok(SkillBody {
            manifest,
            instructions,
            linked_files,
        })
    }
}

// ── ParseError ────────────────────────────────────────────────────────────────

/// Errors produced when parsing a SKILL.md file.
#[derive(Debug, Clone, PartialEq)]
pub enum ParseError {
    /// The file does not start with a `---` frontmatter block.
    MissingFrontmatter,
    /// The frontmatter block has no closing `---`.
    UnclosedFrontmatter,
    /// The `name` field is absent from the frontmatter.
    MissingName,
}

impl core::fmt::Display for ParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ParseError::MissingFrontmatter => write!(f, "SKILL.md has no frontmatter block"),
            ParseError::UnclosedFrontmatter => {
                write!(f, "SKILL.md frontmatter is not closed with ---")
            }
            ParseError::MissingName => write!(f, "SKILL.md frontmatter missing 'name' field"),
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Extract local file references from Markdown link syntax `[label](target)`.
///
/// URL targets (`://`) and fragment-only anchors (`#`) are skipped.
fn extract_local_links(text: &str) -> Vec<String> {
    let mut links = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("](") {
        let after_open = &rest[open + 2..];
        if let Some(close) = after_open.find(')') {
            let target = &after_open[..close];
            if !target.is_empty()
                && !target.contains("://")
                && !target.starts_with('#')
            {
                links.push(target.to_string());
            }
            rest = &after_open[close + 1..];
        } else {
            break;
        }
    }
    links
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
---
name: web-research
description: Searches the web and summarises findings for a given query.
version: 0.1.0
capabilities: network.read
---

## Procedure

1. Use the `web-search` tool with the user's query.
2. Browse the top result if needed.

See [reference.md](reference.md) for scoring guidance.
";

    #[test]
    fn parse_manifest_extracts_all_fields() {
        let (m, _) = SkillManifest::from_frontmatter(SAMPLE).unwrap();
        assert_eq!(m.name, "web-research");
        assert_eq!(
            m.description,
            "Searches the web and summarises findings for a given query."
        );
        assert_eq!(m.version.as_deref(), Some("0.1.0"));
        assert_eq!(m.capabilities, vec!["network.read"]);
    }

    #[test]
    fn body_text_is_non_empty() {
        let (_, body) = SkillManifest::from_frontmatter(SAMPLE).unwrap();
        assert!(body.contains("web-search"));
    }

    #[test]
    fn linked_local_files_are_extracted() {
        let body = SkillBody::parse(SAMPLE).unwrap();
        assert_eq!(body.linked_files, vec!["reference.md"]);
    }

    #[test]
    fn url_links_are_not_extracted() {
        let text = "---\nname: test\n---\nSee [docs](https://example.com) and [local](local.md).";
        let body = SkillBody::parse(text).unwrap();
        assert_eq!(body.linked_files, vec!["local.md"]);
    }

    #[test]
    fn missing_frontmatter_returns_error() {
        let err = SkillManifest::from_frontmatter("just text").unwrap_err();
        assert_eq!(err, ParseError::MissingFrontmatter);
    }

    #[test]
    fn unclosed_frontmatter_returns_error() {
        let err = SkillManifest::from_frontmatter("---\nname: x\n").unwrap_err();
        assert_eq!(err, ParseError::UnclosedFrontmatter);
    }

    #[test]
    fn missing_name_returns_error() {
        let err = SkillManifest::from_frontmatter("---\ndescription: no name\n---\nbody")
            .unwrap_err();
        assert_eq!(err, ParseError::MissingName);
    }

    #[test]
    fn multiple_capabilities_parsed() {
        let text = "---\nname: multi\ncapabilities: cap.a, cap.b, cap.c\n---\nbody";
        let (m, _) = SkillManifest::from_frontmatter(text).unwrap();
        assert_eq!(m.capabilities, vec!["cap.a", "cap.b", "cap.c"]);
    }

    #[test]
    fn description_defaults_to_empty_when_absent() {
        let text = "---\nname: no-desc\n---\nbody";
        let (m, _) = SkillManifest::from_frontmatter(text).unwrap();
        assert!(m.description.is_empty());
    }
}
