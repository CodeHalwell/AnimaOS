//! Built-in skills shipped with AnimaOS.
//!
//! These are read-only, versioned with the binary, and always active.
//! They cannot be modified or deleted at runtime.

/// Static descriptor for a built-in skill.
pub struct BuiltinSkill {
    pub name: &'static str,
    pub description: &'static str,
    pub capabilities: &'static [&'static str],
    pub instructions: &'static str,
}

/// All built-in skills.
pub const BUILTIN_SKILLS: &[BuiltinSkill] = &[
    BuiltinSkill {
        name: "web-research",
        description: "Search the web and synthesise findings into a concise, cited summary.",
        capabilities: &["network.read"],
        instructions: "\
## Procedure

1. Call the `web-search` tool with the user's query.
2. Review the top 3 results; browse one for detail if needed.
3. Synthesise findings into 3–5 bullet points with source citations.
4. If the query is time-sensitive, note the search date.

## Tips

- Prefer primary sources over aggregators.
- When results disagree, report the disagreement rather than picking a side.
- For programming topics, include working code snippets.
",
    },
    BuiltinSkill {
        name: "summarise-and-archive",
        description: "Summarise a document or conversation and store the result in long-term memory.",
        capabilities: &["memory.write"],
        instructions: "\
## Procedure

1. Identify key topics, decisions, and open questions in the content.
2. Write a structured summary:
   - **Headline**: one sentence.
   - **Decisions**: bullet list of resolved items.
   - **Open**: unresolved questions.
3. Tag the summary with relevant keywords.
4. Archive via the memory tool with action `archive`.

## Format

```
Headline: <one-sentence description>
Topics: <comma-separated keywords>
Decisions:
  - ...
Open:
  - ...
```
",
    },
    BuiltinSkill {
        name: "draft-a-tool",
        description: "Guide the agent through drafting a new WASM tool following the E11 safety requirements.",
        capabilities: &["self.propose"],
        instructions: "\
## Purpose

Use when a recurring task could be automated as a new sandboxed tool.

## Steps

1. Identify the exact input/output contract for the tool.
2. Write a description (used for semantic selection later).
3. Declare required capabilities — be minimal; do not over-scope.
4. Draft the logic. The implementation **must** be WASM-only; native code is rejected.
5. List fixture inputs for sandbox testing.
6. Submit via the `propose-tool` action and await operator approval.

## Safety requirements (non-negotiable)

- WASM sandbox with fuel limit and memory cap.
- Capabilities declared explicitly; none implied.
- Egress only through the egress guard.
- Fixture tests must pass before the proposal is submitted.
- Tools always require operator approval (no auto-promotion).
",
    },
    BuiltinSkill {
        name: "onboarding-interview",
        description: "Conduct a structured first-run interview to seed the agent's identity memory.",
        capabilities: &[],
        instructions: "\
## Purpose

Collect the user's name, goals, preferences, and context so the agent can
personalise its behaviour from the first real task.

## Questions (ask one at a time, in order)

1. \"What's your name, and what would you like me to call you?\"
2. \"What kinds of tasks do you expect to use me for most?\"
3. \"Are there any topics or capabilities you'd like me to avoid?\"
4. \"Do you prefer brief and direct replies, or detailed explanations?\"
5. \"Is there anything else I should know about your environment or context?\"

## After collecting answers

- Store each answer as an identity fact: `identity set <key> <value>`.
- Confirm completion: \"Thanks — I've saved your preferences. Let's get started.\"
",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_builtin_skills_have_non_empty_name_and_description() {
        for skill in BUILTIN_SKILLS {
            assert!(!skill.name.is_empty(), "skill has empty name");
            assert!(
                !skill.description.is_empty(),
                "skill '{}' has empty description",
                skill.name
            );
        }
    }

    #[test]
    fn all_builtin_skill_names_are_unique() {
        let names: Vec<&str> = BUILTIN_SKILLS.iter().map(|s| s.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "duplicate builtin skill name");
    }

    #[test]
    fn draft_a_tool_skill_exists() {
        let found = BUILTIN_SKILLS.iter().any(|s| s.name == "draft-a-tool");
        assert!(found, "draft-a-tool builtin skill not found");
    }

    #[test]
    fn onboarding_interview_skill_exists() {
        let found = BUILTIN_SKILLS
            .iter()
            .any(|s| s.name == "onboarding-interview");
        assert!(found, "onboarding-interview builtin skill not found");
    }
}
