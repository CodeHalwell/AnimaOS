#![forbid(unsafe_code)]

//! Defence Layer — Epic E5.6 (Immune Analogue)
//!
//! Screens cortex proposals for:
//! - **S5.6.1** Prompt injection in tool outputs and externally-sourced text.
//! - **S5.6.2** Goal drift from the original invocation objective.
//! - **S5.6.3** Reward hacking: completion claims without observable evidence.
//! - **S5.6.4** Unsafe motor actions: critical-path filesystem writes,
//!   blocklisted network requests, and self-modification attempts, checked
//!   against the `anima-self` capability scope.
//! - **S5.6.5** Veto mechanics with per-invocation audit entries and
//!   repeated-veto escalation to user attention.
//!
//! # Architecture
//!
//! The crate is designed to be wired into the vita → cortex IPC layer
//! introduced in E5.1.  It deliberately has no dependency on `vita` so that
//! the defence logic can be tested and fuzzed in isolation; callers are
//! responsible for translating [`VetoResult`] into audit entries.
//!
//! ```text
//! cortex proposal
//!      │
//!      ▼
//! PromptInjectionDetector ──► veto? ──► AuditEntry::DefenceVeto
//!      │ allow
//!      ▼
//! UnsafeMotorActionGate  ──► veto?
//!      │ allow
//!      ▼
//! RewardHackingDetector  ──► veto?
//!      │ allow
//!      ▼
//! GoalDriftMonitor       ──► veto?
//!      │ allow
//!      ▼
//!    proceed
//! ```

pub mod goal_drift;
pub mod injection;
pub mod layer;
pub mod motor_gate;
pub mod reward_hacking;
pub mod types;

pub use goal_drift::{GoalDriftMonitor, ObjectiveSimilarity, TermOverlapSimilarity};
pub use injection::{HeuristicClassifier, InjectionClassifier, PromptInjectionDetector};
pub use layer::{DefenceLayer, ScreeningOutcome};
pub use motor_gate::UnsafeMotorActionGate;
pub use reward_hacking::RewardHackingDetector;
pub use types::{ActionKind, CortexProposal, DefenceConfig, VetoEvent, VetoReason, VetoResult};
