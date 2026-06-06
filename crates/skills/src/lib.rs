//! AnimaOS Skills subsystem — Epic E11 (Self-Extension).
//!
//! Implements the Anthropic Agent Skills model with progressive disclosure,
//! a promotion / safety gate, and a self-improvement reflection loop.
//!
//! # Architecture
//!
//! ```text
//! SkillRegistry ────── list_active()    ──► cortex context (stage 1: name+desc)
//!               ├───── load_body(id)   ──► cortex selects skill (stage 2: instructions)
//!               └───── body.linked_files ──► cortex loads resources (stage 3: on demand)
//!
//! SkillProposal ──► evaluate_skill_proposal() ──► SkillRegistry (Proposed | Active)
//!                                                        │
//!                                                 OperatorApproval ──► promote()
//!
//! ToolProposal ──► evaluate_tool_proposal_with_summary() ──► PendingApproval
//!                                                                   │
//!                                                           OperatorApproval ──► ToolRegistry
//!
//! EpisodeSummaries ──► reflect_on_episodes() ──► FrictionPattern ──► generate_skill_draft()
//! ```
//!
//! # Safety model
//!
//! | Risk | Control |
//! |---|---|
//! | Injection via skill text | `SkillContentScreen` blocks known patterns |
//! | Agent skill privilege escalation | `PromotionGateConfig::auto_promote_agent_skills` + `vita` defence layer |
//! | Tool (WASM) auto-execution | Tools **always** require operator approval |
//! | Silent capability creep | Every registration emits an audit entry (via vita) |
//! | Post-promotion misbehaviour | `kill_switch()` + per-skill `quarantine()` + `rollback()` |

#![forbid(unsafe_code)]

pub mod builtins;
pub mod manifest;
pub mod proposal;
pub mod provenance;
pub mod reflection;
pub mod registry;

// ── Public re-exports ─────────────────────────────────────────────────────────

pub use builtins::{BuiltinSkill, BUILTIN_SKILLS};
pub use manifest::{ParseError, SkillBody, SkillManifest};
pub use proposal::{
    evaluate_skill_proposal, evaluate_tool_proposal_with_summary, ProposalAction, ProposalOutcome,
    PromotionGateConfig, ScreenResult, SkillContentScreen, SkillProposal, ToolProposal,
    ToolProposalAction, ToolProposalOutcome,
};
pub use provenance::{SkillAuthor, SkillProvenance, SkillState};
pub use reflection::{
    generate_skill_draft, reflect_on_episodes, EpisodeSummary, FrictionPattern, ReflectionConfig,
    ReflectionReport,
};
pub use registry::{RegistryError, SkillEntry, SkillRegistry};
