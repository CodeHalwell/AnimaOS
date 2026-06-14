#![no_std]
#![deny(missing_docs)]

//! AnimaOS corpus: the privileged trusted computing base — the body.
//!
//! This crate is intentionally small. `unsafe` is permitted but is confined to
//! audited modules with explicit safety invariants documented at each call site.
//!
//! # `no_std` contract
//!
//! `corpus` is fully `no_std`-clean. It depends only on `core` (and `core::alloc`
//! for the [`heap_allocator`] module) so that it can be linked into the bare-metal
//! microVM kernel as well as the hosted Linux target.  Tests are compiled with
//! `std` present (the test harness requires it); the library itself does not.

pub mod frame_allocator;
pub mod heap_allocator;
pub mod pcb;
pub mod syscall;

pub use frame_allocator::{FrameAllocation, FrameAllocator, FrameAllocatorError};
pub use heap_allocator::BumpAllocator;
pub use pcb::{AgentPcb, AgentPid, AgentState, TransitionError};
pub use syscall::{dispatch, SyscallEnum, SyscallError, SyscallHandler, SyscallOutcome};
