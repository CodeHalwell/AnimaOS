#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

//! Efferent actuator core: tool driver registry, routing, and isolation.
//!
//! # Structure
//!
//! - [`circuit`] — Closed/Open/HalfOpen circuit breaker per tool pathway.
//! - [`compute`] — Wasmtime sandbox for untrusted tool execution (E2.5).
//!   Available only with the `wasm-sandbox` feature (std-only).
//! - [`envelope`] — [`ToolEnvelope`] type carrying calls across MCP/A2A buses.
//! - [`registry`] — [`ToolRegistry`]: registration, discovery, and dispatch.
//! - [`routing`] — [`length_robust_filter`]: relative-score tool selection.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

pub mod circuit;
#[cfg(feature = "wasm-sandbox")]
pub mod compute;
pub mod envelope;
pub mod registry;
pub mod routing;

pub use circuit::{BreakerState, CircuitBreaker};
#[cfg(feature = "wasm-sandbox")]
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
