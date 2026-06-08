//! Structured runtime configuration for AnimaOS — Epic E20.
//!
//! [`AnimaConfig`] is a TOML-backed configuration document that covers the
//! gate coefficients, memory limits, scheduler knobs, and logging settings
//! that were previously scattered across individual struct defaults.
//!
//! # Usage
//!
//! ```rust
//! use config::AnimaConfig;
//!
//! // Load from a file, falling back to built-in defaults.
//! let cfg = AnimaConfig::from_file("~/.anima/anima.toml")
//!     .unwrap_or_else(|_| AnimaConfig::from_defaults());
//!
//! cfg.validate().expect("config is valid");
//!
//! // Write a template to disk so operators can inspect and edit it.
//! let toml = cfg.to_toml_string().expect("serialize");
//! println!("{toml}");
//! ```
//!
//! # CLI
//!
//! ```text
//! cargo run --bin anima-hosted -- config show
//! cargo run --bin anima-hosted -- config validate [<path>]
//! cargo run --bin anima-hosted -- config init [--path <p>]
//! ```

#![forbid(unsafe_code)]

mod loader;
mod schema;
mod validate;

pub use loader::{load_or_defaults, ConfigError, ConfigSource};
pub use schema::{
    AgentSection, AnimaConfig, GateSection, LoggingSection, MemorySection, SchedulerSection,
    SchemaSection,
};
pub use validate::ValidationError;
