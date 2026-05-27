#![forbid(unsafe_code)]

//! Efferent actuator core: tool driver registry, routing, and isolation.
//!
//! # Structure
//!
//! - [`circuit`] — Closed/Open/HalfOpen circuit breaker per tool pathway.
//! - [`compute`] — Wasmtime sandbox for untrusted tool execution (E2.5) — std only.
//! - [`envelope`] — [`ToolEnvelope`] type carrying calls across MCP/A2A buses.
//! - [`registry`] — [`ToolRegistry`]: registration, discovery, and dispatch.
//! - [`routing`] — [`length_robust_filter`]: relative-score tool selection.
//!
//! # `no_std` note (E4.5)
//!
//! `praxis` requires `std` because the Wasmtime sandbox is not `no_std`-compatible.
//! The `std` feature (default = true) must be enabled for all current functionality.
//! A future micro-wasm interpreter could unlock a genuine `no_std` compute layer.

pub mod circuit;
pub mod envelope;
pub mod registry;
pub mod routing;

// Wasmtime sandbox is std-only.
#[cfg(feature = "std")]
pub mod compute;

pub use circuit::{BreakerState, CircuitBreaker};
#[cfg(feature = "std")]
pub use compute::{
    SandboxCapabilities, SandboxConfig, SandboxError, SandboxResult, SandboxedMathEvaluator,
    WasmSandbox,
};
pub use envelope::{Bus, ToolEnvelope};
pub use registry::{ClockTool, EchoTool, TextIoTool, ToolRegistry};
pub use routing::{length_robust_filter, ToolCandidate};

/// Trait implemented by every tool driver exposed under `/dev/anima/praxis/tools/`.
pub trait ToolDriver: Send + Sync {
    /// Stable tool identifier used for registration and routing.
    fn id(&self) -> &'static str;

    /// JSON schema string describing the accepted input payload.
    fn schema(&self) -> &'static str;

    /// Synchronously invokes the tool with a serialized payload.
    ///
    /// # Errors
    ///
    /// Returns a [`ToolInvocationError`] describing why the invocation failed.
    fn invoke(&self, payload: &[u8]) -> Result<Vec<u8>, ToolInvocationError>;
}

/// Errors returned from tool invocations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolInvocationError {
    /// Tool rejected the payload as malformed or non-UTF-8.
    InvalidPayload,
    /// Tool failed internally; the circuit breaker will record this failure.
    ExecutionFailed(String),
    /// The tool's circuit breaker is open — dispatch is blocked.
    BreakerOpen,
}
