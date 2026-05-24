#![deny(missing_docs)]

//! AnimaOS kernel-core: the privileged trusted computing base.
//!
//! This crate is intentionally small. `unsafe` is permitted but is confined to
//! audited modules with explicit safety invariants documented at each call site.

pub mod frame_allocator;
pub mod pcb;
pub mod syscall;

pub use frame_allocator::{FrameAllocation, FrameAllocator, FrameAllocatorError};
pub use pcb::{AgentPcb, AgentPid, AgentState};
pub use syscall::{SyscallEnum, SyscallError};
