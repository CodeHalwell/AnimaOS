//! Message envelopes used to ferry tool calls across MCP / A2A buses.

use alloc::string::String;
use alloc::vec::Vec;

/// Stable identifier for the bus a message was routed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bus {
    /// Model Context Protocol bus.
    Mcp,
    /// Agent-to-Agent direct bus.
    A2a,
}

/// Envelope carrying a tool invocation across the bus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolEnvelope {
    /// Originating bus.
    pub bus: Bus,
    /// Tool identifier (matches `ToolDriver::id`).
    pub tool_id: String,
    /// Opaque payload bytes.
    pub payload: Vec<u8>,
    /// Correlation identifier for response routing.
    pub correlation_id: u64,
}

impl ToolEnvelope {
    /// Convenience constructor.
    pub fn new(
        bus: Bus,
        tool_id: impl Into<String>,
        payload: Vec<u8>,
        correlation_id: u64,
    ) -> Self {
        Self {
            bus,
            tool_id: tool_id.into(),
            payload,
            correlation_id,
        }
    }
}
