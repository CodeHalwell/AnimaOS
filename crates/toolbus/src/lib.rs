#![forbid(unsafe_code)]

//! Efferent actuator core: tool driver registry, routing, and isolation.

pub mod circuit;
pub mod envelope;
pub mod routing;

pub use circuit::{BreakerState, CircuitBreaker};
pub use envelope::{Bus, ToolEnvelope};
pub use routing::{length_robust_filter, ToolCandidate};

/// Trait implemented by every tool driver exposed under `/dev/tools/`.
pub trait ToolDriver: Send + Sync {
    /// Stable tool identifier.
    fn id(&self) -> &'static str;

    /// Returns the schema describing accepted input payloads.
    fn schema(&self) -> &'static str;

    /// Invokes the tool with a serialized payload, returning a serialized response.
    fn invoke(&self, payload: &[u8]) -> Result<Vec<u8>, ToolInvocationError>;
}

/// Errors returned from tool invocations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolInvocationError {
    /// Tool rejected the payload as malformed.
    InvalidPayload,
    /// Tool failed internally; circuit breaker should observe this.
    ExecutionFailed(String),
    /// Tool is currently blocked by an open circuit breaker.
    BreakerOpen,
}
