//! `AnimaConfig` schema — typed TOML document.

use serde::{Deserialize, Serialize};

/// Current schema version.  Bump on any breaking change to the structure.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Top-level AnimaOS runtime configuration.
///
/// All fields have sensible defaults so `AnimaConfig::from_defaults()` produces
/// a production-ready configuration without any file on disk.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AnimaConfig {
    /// Schema version — used for migration guards.
    pub schema: SchemaSection,
    /// Agent identity and state directory.
    pub agent: AgentSection,
    /// Striatal Gate coefficients (mirrors `vita::gate::GateConfig`).
    pub gate: GateSection,
    /// Memory tier limits.
    pub memory: MemorySection,
    /// MLFQ scheduler knobs.
    pub scheduler: SchedulerSection,
    /// Audit log and telemetry settings.
    pub logging: LoggingSection,
}

impl AnimaConfig {
    /// Construct an `AnimaConfig` filled with documented, production-ready defaults.
    pub fn from_defaults() -> Self {
        Self::default()
    }

    /// Serialize the configuration to a TOML string suitable for writing to disk.
    pub fn to_toml_string(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }

    /// Load from a TOML file.  Returns `ConfigError` if the file cannot be read
    /// or parsed.
    pub fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self, crate::ConfigError> {
        crate::loader::load_from_file(path)
    }

    /// Return the canonical default config path: `~/.anima/<agent_id>/anima.toml`.
    pub fn default_path(agent_id: &str) -> std::path::PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        std::path::PathBuf::from(home)
            .join(".anima")
            .join(agent_id)
            .join("anima.toml")
    }

    /// Validate the configuration, returning the first error found.
    pub fn validate(&self) -> Result<(), crate::ValidationError> {
        crate::validate::validate(self)
    }

    /// Produce a human-readable summary of every section for `config show`.
    pub fn to_display_string(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("[schema]  version={}\n", self.schema.version));
        out.push_str(&format!(
            "[agent]   id={} state_dir={}\n",
            self.agent.id, self.agent.state_dir
        ));
        out.push_str(&format!(
            "[gate]    base_threshold={:.3} urgency_weight={:.3} novelty_weight={:.3}\n",
            self.gate.base_threshold, self.gate.urgency_weight, self.gate.novelty_weight
        ));
        out.push_str(&format!(
            "[memory]  max_context_tokens={} block_size={} high_water_ratio={:.2}\n",
            self.memory.max_context_tokens,
            self.memory.block_size_tokens,
            self.memory.high_water_ratio
        ));
        out.push_str(&format!(
            "[sched]   boost_interval_ms={} default_token_budget={}\n",
            self.scheduler.boost_interval_ms, self.scheduler.default_token_budget
        ));
        out.push_str(&format!(
            "[logging] audit_dir={} max_entries_in_memory={}\n",
            self.logging.audit_dir, self.logging.max_entries_in_memory
        ));
        out
    }
}

/// `[schema]` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SchemaSection {
    /// Monotonically increasing schema version.  The loader rejects configs
    /// with a version higher than `CURRENT_SCHEMA_VERSION`.
    pub version: u32,
}

impl Default for SchemaSection {
    fn default() -> Self {
        Self {
            version: CURRENT_SCHEMA_VERSION,
        }
    }
}

/// `[agent]` section — identity and storage paths.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentSection {
    /// Stable agent identifier used as a directory key under `~/.anima/`.
    pub id: String,
    /// Directory where state files (identity, L3 archive, snapshots) are stored.
    pub state_dir: String,
}

impl Default for AgentSection {
    fn default() -> Self {
        Self {
            id: "anima".to_string(),
            state_dir: "~/.anima/anima".to_string(),
        }
    }
}

/// `[gate]` section — Striatal Gate coefficients (E5.2).
///
/// These mirror the fields of `vita::gate::GateConfig`.  When an
/// `AnimaConfig` is loaded at startup, the hosted kernel constructs a
/// `GateConfig` from these values so the gate behaviour is fully operator-
/// controllable without recompilation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GateSection {
    /// Baseline threshold before homeostatic adjustments (`[0.0, 1.0]`).
    pub base_threshold: f64,
    /// Weight applied to the event's urgency score.
    pub urgency_weight: f64,
    /// Weight applied to the event's novelty score.
    pub novelty_weight: f64,
    /// Bonus added when the event is user-facing.
    pub user_facing_bonus: f64,
    /// Bonus added when the event is an operator command.
    pub operator_command_bonus: f64,
    /// Penalty raised to the threshold per unit of `thermal_load`.
    pub thermal_penalty: f64,
    /// Penalty raised to the threshold per unit of `memory_pressure`.
    pub memory_penalty: f64,
    /// Penalty raised to the threshold per unit of `financial_budget` depletion.
    pub financial_penalty: f64,
    /// Reduction applied when `attention_demand` is high.
    pub attention_boost: f64,
    /// Value score ceiling for `CheapLocal` routing.
    pub cheap_local_ceiling: f64,
    /// Value score floor for `Frontier` routing.
    pub frontier_floor: f64,
}

impl Default for GateSection {
    fn default() -> Self {
        Self {
            base_threshold: 0.40,
            urgency_weight: 0.65,
            novelty_weight: 0.35,
            user_facing_bonus: 0.15,
            operator_command_bonus: 0.20,
            thermal_penalty: 0.30,
            memory_penalty: 0.20,
            financial_penalty: 0.15,
            attention_boost: 0.20,
            cheap_local_ceiling: 0.60,
            frontier_floor: 0.85,
        }
    }
}

/// `[memory]` section — L1/L2/L3 limits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemorySection {
    /// Maximum context window in tokens (L1 ceiling).
    pub max_context_tokens: u32,
    /// Page size in tokens for the PagedAttention block table.
    pub block_size_tokens: u32,
    /// Fraction of `max_context_tokens` at which L1 emits `HighWater` pressure.
    pub high_water_ratio: f64,
    /// Activation floor below which L1 and L2 nodes are pruned during sleep.
    pub prune_floor: f64,
    /// Maximum number of entries in the L2 ARC warm cache.
    pub l2_capacity: usize,
    /// Path to the L3 archive directory.
    pub l3_dir: String,
}

impl Default for MemorySection {
    fn default() -> Self {
        Self {
            max_context_tokens: 8192,
            block_size_tokens: 64,
            high_water_ratio: 0.80,
            prune_floor: 0.10,
            l2_capacity: 1000,
            l3_dir: "~/.anima/anima/l3".to_string(),
        }
    }
}

/// `[scheduler]` section — MLFQ dispatcher settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SchedulerSection {
    /// Interval in milliseconds between starvation-prevention boost sweeps.
    pub boost_interval_ms: u64,
    /// Default per-task token budget when no budget is specified at enqueue time.
    pub default_token_budget: u32,
    /// Maximum queue depth across all tiers.
    pub max_queue_depth: usize,
}

impl Default for SchedulerSection {
    fn default() -> Self {
        Self {
            boost_interval_ms: 5000,
            default_token_budget: 2048,
            max_queue_depth: 1024,
        }
    }
}

/// `[logging]` section — audit log and telemetry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LoggingSection {
    /// Directory where JSONL audit logs are written.  Maps to `ANIMA_AUDIT_DIR`.
    pub audit_dir: String,
    /// Maximum number of `AuditEntry` values held in the in-memory ring buffer.
    /// Older entries are dropped when the limit is exceeded.
    pub max_entries_in_memory: usize,
    /// Whether to emit interoceptive snapshots to the audit log at 1 Hz.
    pub emit_interoceptive_snapshots: bool,
}

impl Default for LoggingSection {
    fn default() -> Self {
        Self {
            audit_dir: "~/.anima/anima/logs".to_string(),
            max_entries_in_memory: 10_000,
            emit_interoceptive_snapshots: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_consistent() {
        let cfg = AnimaConfig::from_defaults();
        assert_eq!(cfg.schema.version, CURRENT_SCHEMA_VERSION);
        assert_eq!(cfg.agent.id, "anima");
        assert!(cfg.gate.base_threshold > 0.0 && cfg.gate.base_threshold < 1.0);
        assert!(cfg.memory.max_context_tokens > 0);
        assert!(cfg.memory.block_size_tokens > 0);
        assert!(cfg.scheduler.boost_interval_ms > 0);
        assert!(cfg.logging.max_entries_in_memory > 0);
    }

    #[test]
    fn gate_defaults_match_e5_2_coefficients() {
        let g = GateSection::default();
        // These values must stay in sync with vita::gate::GateConfig::default().
        assert!((g.base_threshold - 0.40).abs() < 1e-9);
        assert!((g.urgency_weight - 0.65).abs() < 1e-9);
        assert!((g.novelty_weight - 0.35).abs() < 1e-9);
        assert!((g.cheap_local_ceiling - 0.60).abs() < 1e-9);
        assert!((g.frontier_floor - 0.85).abs() < 1e-9);
    }

    #[test]
    fn config_round_trips_through_toml() {
        let original = AnimaConfig::from_defaults();
        let toml_str = original.to_toml_string().expect("serialize");
        let restored: AnimaConfig = toml::from_str(&toml_str).expect("deserialize");
        assert_eq!(original, restored);
    }

    #[test]
    fn to_display_string_contains_all_sections() {
        let s = AnimaConfig::from_defaults().to_display_string();
        assert!(s.contains("[schema]"));
        assert!(s.contains("[agent]"));
        assert!(s.contains("[gate]"));
        assert!(s.contains("[memory]"));
        assert!(s.contains("[sched]"));
        assert!(s.contains("[logging]"));
    }

    #[test]
    fn default_path_contains_agent_id() {
        let p = AnimaConfig::default_path("my-agent");
        let s = p.to_string_lossy();
        assert!(s.contains("my-agent"));
        assert!(s.contains(".anima"));
        assert!(s.ends_with("anima.toml"));
    }

    #[test]
    fn schema_section_default_version_is_current() {
        assert_eq!(SchemaSection::default().version, CURRENT_SCHEMA_VERSION);
    }
}
