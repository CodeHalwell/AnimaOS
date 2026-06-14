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
    /// Read the next sensory packet from `/dev/anima/senses/human`.
    ReadSensoryPacket,
    /// Dispatch an efferent tool call through praxis.
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

/// Successful per-syscall result.
///
/// Payloads that would otherwise couple `corpus` to higher kernel crates
/// (scheduler / praxis / senses) are returned as opaque handles/indices; the
/// caller resolves them against the owning subsystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyscallOutcome {
    /// Quantum was yielded back to the scheduler.
    Yielded,
    /// Caller was parked until the next interoceptive tick.
    Slept,
    /// Frames were allocated; carries the count granted.
    FramesAllocated(usize),
    /// A sensory packet is ready; carries an opaque buffer handle.
    SensoryPacket(SensoryHandle),
    /// A tool call was dispatched; carries an opaque dispatch ticket.
    ToolDispatched(DispatchTicket),
}

/// Opaque handle to a sensory packet buffer owned by the senses subsystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SensoryHandle(pub u64);

/// Opaque ticket identifying an in-flight tool dispatch in praxis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DispatchTicket(pub u64);

/// Handler seam implemented later by the kernel (scheduler / praxis / senses).
///
/// One method per [`SyscallEnum`] variant. Each may deny the call with
/// [`SyscallError::PermissionDenied`] when the caller lacks the required
/// capability token; capability tokens themselves are out of scope here and are
/// expected to be threaded through the concrete handler's own state.
pub trait SyscallHandler {
    /// Handle [`SyscallEnum::Yield`].
    fn yield_quantum(&mut self) -> Result<SyscallOutcome, SyscallError>;

    /// Handle [`SyscallEnum::SleepUntilTick`].
    fn sleep_until_tick(&mut self) -> Result<SyscallOutcome, SyscallError>;

    /// Handle [`SyscallEnum::AllocateFrames`].
    fn allocate_frames(&mut self, frames: usize) -> Result<SyscallOutcome, SyscallError>;

    /// Handle [`SyscallEnum::ReadSensoryPacket`].
    fn read_sensory_packet(&mut self) -> Result<SyscallOutcome, SyscallError>;

    /// Handle [`SyscallEnum::DispatchTool`].
    fn dispatch_tool(&mut self, tool_id: u32) -> Result<SyscallOutcome, SyscallError>;
}

/// Routes a syscall to the matching [`SyscallHandler`] method.
pub fn dispatch(
    syscall: SyscallEnum,
    handler: &mut impl SyscallHandler,
) -> Result<SyscallOutcome, SyscallError> {
    match syscall {
        SyscallEnum::Yield => handler.yield_quantum(),
        SyscallEnum::SleepUntilTick => handler.sleep_until_tick(),
        SyscallEnum::AllocateFrames { frames } => handler.allocate_frames(frames),
        SyscallEnum::ReadSensoryPacket => handler.read_sensory_packet(),
        SyscallEnum::DispatchTool { tool_id } => handler.dispatch_tool(tool_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock handler recording the last call and honouring a denial switch.
    #[derive(Default)]
    struct MockHandler {
        deny: bool,
        last_frames: usize,
        last_tool: u32,
    }

    impl SyscallHandler for MockHandler {
        fn yield_quantum(&mut self) -> Result<SyscallOutcome, SyscallError> {
            if self.deny {
                return Err(SyscallError::PermissionDenied);
            }
            Ok(SyscallOutcome::Yielded)
        }

        fn sleep_until_tick(&mut self) -> Result<SyscallOutcome, SyscallError> {
            if self.deny {
                return Err(SyscallError::PermissionDenied);
            }
            Ok(SyscallOutcome::Slept)
        }

        fn allocate_frames(&mut self, frames: usize) -> Result<SyscallOutcome, SyscallError> {
            if self.deny {
                return Err(SyscallError::PermissionDenied);
            }
            self.last_frames = frames;
            Ok(SyscallOutcome::FramesAllocated(frames))
        }

        fn read_sensory_packet(&mut self) -> Result<SyscallOutcome, SyscallError> {
            if self.deny {
                return Err(SyscallError::PermissionDenied);
            }
            Ok(SyscallOutcome::SensoryPacket(SensoryHandle(7)))
        }

        fn dispatch_tool(&mut self, tool_id: u32) -> Result<SyscallOutcome, SyscallError> {
            if self.deny {
                return Err(SyscallError::PermissionDenied);
            }
            self.last_tool = tool_id;
            Ok(SyscallOutcome::ToolDispatched(DispatchTicket(
                tool_id as u64,
            )))
        }
    }

    #[test]
    fn dispatch_yield() {
        let mut h = MockHandler::default();
        assert_eq!(
            dispatch(SyscallEnum::Yield, &mut h),
            Ok(SyscallOutcome::Yielded)
        );
    }

    #[test]
    fn dispatch_sleep() {
        let mut h = MockHandler::default();
        assert_eq!(
            dispatch(SyscallEnum::SleepUntilTick, &mut h),
            Ok(SyscallOutcome::Slept)
        );
    }

    #[test]
    fn dispatch_allocate_frames() {
        let mut h = MockHandler::default();
        assert_eq!(
            dispatch(SyscallEnum::AllocateFrames { frames: 4 }, &mut h),
            Ok(SyscallOutcome::FramesAllocated(4))
        );
        assert_eq!(h.last_frames, 4);
    }

    #[test]
    fn dispatch_read_sensory_packet() {
        let mut h = MockHandler::default();
        assert_eq!(
            dispatch(SyscallEnum::ReadSensoryPacket, &mut h),
            Ok(SyscallOutcome::SensoryPacket(SensoryHandle(7)))
        );
    }

    #[test]
    fn dispatch_tool() {
        let mut h = MockHandler::default();
        assert_eq!(
            dispatch(SyscallEnum::DispatchTool { tool_id: 42 }, &mut h),
            Ok(SyscallOutcome::ToolDispatched(DispatchTicket(42)))
        );
        assert_eq!(h.last_tool, 42);
    }

    #[test]
    fn dispatch_denies_when_handler_rejects() {
        let mut h = MockHandler {
            deny: true,
            ..Default::default()
        };
        for sc in [
            SyscallEnum::Yield,
            SyscallEnum::SleepUntilTick,
            SyscallEnum::AllocateFrames { frames: 1 },
            SyscallEnum::ReadSensoryPacket,
            SyscallEnum::DispatchTool { tool_id: 1 },
        ] {
            assert_eq!(dispatch(sc, &mut h), Err(SyscallError::PermissionDenied));
        }
    }
}
