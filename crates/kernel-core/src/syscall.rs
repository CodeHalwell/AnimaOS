//! Syscall surface exposed by the autonomic substrate.

/// Enumerated kernel syscalls available to the somatic layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyscallEnum {
    /// Yield the remainder of the current quantum.
    Yield,
    /// Sleep until the next interoceptive tick.
    SleepUntilTick,
    /// Allocate frames from the kernel frame allocator.
    AllocateFrames {
        /// Number of frames to allocate.
        frames: usize,
    },
    /// Read the next sensory packet from `/dev/sensors/human`.
    ReadSensoryPacket,
    /// Dispatch an efferent tool call through the toolbus.
    DispatchTool {
        /// Stable tool identifier.
        tool_id: u32,
    },
}

/// Errors returned from syscall handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyscallError {
    /// The caller lacked the required capability token.
    PermissionDenied,
    /// Underlying subsystem unavailable.
    Unavailable,
    /// Invalid arguments.
    Invalid,
}
