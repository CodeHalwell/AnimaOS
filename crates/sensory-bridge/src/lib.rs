#![forbid(unsafe_code)]

/// External policy bounds from `/dev/sensors/human`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HumanGuidance {
    /// Free-form policy directives provided by the operator.
    pub policy_hint: String,
}

/// Errors raised when sensory input cannot be consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SensoryBridgeError {
    /// Input stream did not contain valid guidance.
    InvalidInput,
}

/// Minimal sensory bridge for human intent signals.
#[derive(Debug, Clone)]
pub struct SensoryBridge {
    active_bounds: HumanGuidance,
}

impl SensoryBridge {
    /// Creates a new sensory bridge with initial human guidance.
    pub fn new(active_bounds: HumanGuidance) -> Self {
        Self { active_bounds }
    }

    /// Reads current human policy bounds.
    pub async fn read_active_bounds(&self) -> Result<HumanGuidance, SensoryBridgeError> {
        Ok(self.active_bounds.clone())
    }

    /// Updates active policy bounds.
    pub fn set_active_bounds(&mut self, guidance: HumanGuidance) {
        self.active_bounds = guidance;
    }
}
