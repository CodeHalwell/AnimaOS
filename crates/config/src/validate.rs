//! Semantic validation for `AnimaConfig`.

use crate::schema::AnimaConfig;

/// A validation error produced by [`validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    /// Field path in `section.field` notation (e.g. `"gate.base_threshold"`).
    pub field: &'static str,
    /// Human-readable description of the constraint violation.
    pub message: String,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

/// Validate an `AnimaConfig`, returning the first error encountered.
pub fn validate(cfg: &AnimaConfig) -> Result<(), ValidationError> {
    // ── [agent] ───────────────────────────────────────────────────────────────
    if cfg.agent.id.trim().is_empty() {
        return Err(err("agent.id", "must not be empty"));
    }
    if cfg.agent.id.contains('/') || cfg.agent.id.contains('\\') || cfg.agent.id.contains("..") {
        return Err(err("agent.id", "must not contain path separators or '..'"));
    }
    if cfg.agent.state_dir.trim().is_empty() {
        return Err(err("agent.state_dir", "must not be empty"));
    }

    // ── [gate] ────────────────────────────────────────────────────────────────
    let g = &cfg.gate;
    check_unit_interval(g.base_threshold, "gate.base_threshold")?;
    check_unit_interval(g.urgency_weight, "gate.urgency_weight")?;
    check_unit_interval(g.novelty_weight, "gate.novelty_weight")?;
    check_unit_interval(g.user_facing_bonus, "gate.user_facing_bonus")?;
    check_unit_interval(g.operator_command_bonus, "gate.operator_command_bonus")?;
    check_unit_interval(g.thermal_penalty, "gate.thermal_penalty")?;
    check_unit_interval(g.memory_penalty, "gate.memory_penalty")?;
    check_unit_interval(g.financial_penalty, "gate.financial_penalty")?;
    check_unit_interval(g.attention_boost, "gate.attention_boost")?;
    if g.cheap_local_ceiling >= g.frontier_floor {
        return Err(err(
            "gate.cheap_local_ceiling",
            "must be strictly less than gate.frontier_floor",
        ));
    }

    // ── [memory] ──────────────────────────────────────────────────────────────
    let m = &cfg.memory;
    if m.max_context_tokens == 0 {
        return Err(err("memory.max_context_tokens", "must be greater than 0"));
    }
    if m.block_size_tokens == 0 {
        return Err(err("memory.block_size_tokens", "must be greater than 0"));
    }
    if m.block_size_tokens > m.max_context_tokens {
        return Err(err(
            "memory.block_size_tokens",
            "must not exceed memory.max_context_tokens",
        ));
    }
    if !(0.0..=1.0).contains(&m.high_water_ratio) {
        return Err(err("memory.high_water_ratio", "must be in [0.0, 1.0]"));
    }
    if !(0.0..=1.0).contains(&m.prune_floor) {
        return Err(err("memory.prune_floor", "must be in [0.0, 1.0]"));
    }
    if m.l2_capacity == 0 {
        return Err(err("memory.l2_capacity", "must be greater than 0"));
    }
    if m.l3_dir.trim().is_empty() {
        return Err(err("memory.l3_dir", "must not be empty"));
    }

    // ── [scheduler] ───────────────────────────────────────────────────────────
    let s = &cfg.scheduler;
    if s.boost_interval_ms == 0 {
        return Err(err("scheduler.boost_interval_ms", "must be greater than 0"));
    }
    if s.default_token_budget == 0 {
        return Err(err(
            "scheduler.default_token_budget",
            "must be greater than 0",
        ));
    }
    if s.max_queue_depth == 0 {
        return Err(err("scheduler.max_queue_depth", "must be greater than 0"));
    }

    // ── [logging] ─────────────────────────────────────────────────────────────
    if cfg.logging.audit_dir.trim().is_empty() {
        return Err(err("logging.audit_dir", "must not be empty"));
    }
    if cfg.logging.max_entries_in_memory == 0 {
        return Err(err(
            "logging.max_entries_in_memory",
            "must be greater than 0",
        ));
    }

    // ── [schema] ──────────────────────────────────────────────────────────────
    if cfg.schema.version == 0 {
        return Err(err("schema.version", "must be greater than 0"));
    }

    Ok(())
}

fn err(field: &'static str, message: impl Into<String>) -> ValidationError {
    ValidationError {
        field,
        message: message.into(),
    }
}

fn check_unit_interval(v: f64, field: &'static str) -> Result<(), ValidationError> {
    if !(0.0..=1.0).contains(&v) {
        return Err(err(field, "must be in [0.0, 1.0]"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::AnimaConfig;

    #[test]
    fn defaults_validate_clean() {
        assert!(validate(&AnimaConfig::from_defaults()).is_ok());
    }

    #[test]
    fn empty_agent_id_is_rejected() {
        let mut cfg = AnimaConfig::from_defaults();
        cfg.agent.id = "".to_string();
        let e = validate(&cfg).unwrap_err();
        assert_eq!(e.field, "agent.id");
    }

    #[test]
    fn path_traversal_in_agent_id_is_rejected() {
        let mut cfg = AnimaConfig::from_defaults();
        cfg.agent.id = "../etc".to_string();
        assert!(validate(&cfg).is_err());
    }

    #[test]
    fn zero_base_threshold_is_rejected() {
        let mut cfg = AnimaConfig::from_defaults();
        cfg.gate.base_threshold = 0.0;
        // 0.0 is at the boundary and accepted; test 2.0 instead
        cfg.gate.base_threshold = 2.0;
        let e = validate(&cfg).unwrap_err();
        assert_eq!(e.field, "gate.base_threshold");
    }

    #[test]
    fn ceiling_above_floor_is_rejected() {
        let mut cfg = AnimaConfig::from_defaults();
        cfg.gate.cheap_local_ceiling = 0.90;
        cfg.gate.frontier_floor = 0.85;
        let e = validate(&cfg).unwrap_err();
        assert_eq!(e.field, "gate.cheap_local_ceiling");
    }

    #[test]
    fn zero_max_context_tokens_is_rejected() {
        let mut cfg = AnimaConfig::from_defaults();
        cfg.memory.max_context_tokens = 0;
        let e = validate(&cfg).unwrap_err();
        assert_eq!(e.field, "memory.max_context_tokens");
    }

    #[test]
    fn block_size_larger_than_context_is_rejected() {
        let mut cfg = AnimaConfig::from_defaults();
        cfg.memory.block_size_tokens = cfg.memory.max_context_tokens + 1;
        let e = validate(&cfg).unwrap_err();
        assert_eq!(e.field, "memory.block_size_tokens");
    }

    #[test]
    fn zero_boost_interval_is_rejected() {
        let mut cfg = AnimaConfig::from_defaults();
        cfg.scheduler.boost_interval_ms = 0;
        let e = validate(&cfg).unwrap_err();
        assert_eq!(e.field, "scheduler.boost_interval_ms");
    }

    #[test]
    fn zero_schema_version_is_rejected() {
        let mut cfg = AnimaConfig::from_defaults();
        cfg.schema.version = 0;
        let e = validate(&cfg).unwrap_err();
        assert_eq!(e.field, "schema.version");
    }

    #[test]
    fn empty_logging_dir_is_rejected() {
        let mut cfg = AnimaConfig::from_defaults();
        cfg.logging.audit_dir = "   ".to_string();
        let e = validate(&cfg).unwrap_err();
        assert_eq!(e.field, "logging.audit_dir");
    }

    #[test]
    fn validation_error_display_is_non_empty() {
        let e = ValidationError {
            field: "gate.base_threshold",
            message: "must be in [0.0, 1.0]".to_string(),
        };
        assert!(!format!("{e}").is_empty());
        assert!(format!("{e}").contains("gate.base_threshold"));
    }
}
