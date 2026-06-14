//! Kernel-side syscall routing for the hosted target.
//!
//! `corpus` defines the syscall *seam* — [`corpus::SyscallEnum`], the
//! [`corpus::SyscallHandler`] trait, and [`corpus::dispatch`] — but deliberately
//! contains no kernel logic, so that the trusted computing base stays `no_std`
//! and free of dependencies on the higher subsystem crates (scheduler / senses /
//! praxis / …).  This module is the hosted kernel's *implementation* of that
//! seam: [`KernelSyscallHandler`] holds borrows of the live subsystems and
//! routes each syscall to the genuine subsystem operation.
//!
//! Mapping (syscall → real subsystem method):
//!
//! | [`corpus::SyscallEnum`]      | Subsystem operation                                                   |
//! |------------------------------|-----------------------------------------------------------------------|
//! | `Yield`                      | [`scheduler::IterationAwareMlfq::check_and_boost`] over the agenda     |
//! | `SleepUntilTick`             | advance the [`interoception`] tick counter (next homeostatic tick)    |
//! | `AllocateFrames { frames }`  | [`corpus::FrameAllocator::allocate`]                                   |
//! | `ReadSensoryPacket`          | [`senses::SensoryBridge::next_prioritized_packet`]                    |
//! | `DispatchTool { tool_id }`   | [`praxis::ToolRegistry::dispatch`] (id resolved via the sorted list)  |
//!
//! # Capability seam
//!
//! [`KernelSyscallHandler`] carries a [`CapabilityPolicy`] predicate.  Every
//! syscall is screened through it first; a `false` verdict short-circuits with
//! [`corpus::SyscallError::PermissionDenied`].  Today the default policy admits
//! everything, but this is the documented hook where the object-capability token
//! system will be threaded: the scheduler will construct the handler with a
//! policy bound to the running agent's capability set, so an agent without the
//! `senses:read` capability (for example) is denied `ReadSensoryPacket` before
//! the bridge is ever touched.

// This module is an integration *seam*: it implements `corpus::SyscallHandler`
// and the PCB-tracking `AgentTable` so the somatic loop can route real syscalls,
// but the giant loop is not yet rewired to call them.  Until that call site
// lands the public API is exercised only by the unit tests below, so suppress
// the dead-code lints rather than littering the seam with `#[allow]` per item.
#![allow(dead_code)]

use corpus::syscall::{DispatchTicket, SensoryHandle};
use corpus::{FrameAllocator, SyscallEnum, SyscallError, SyscallHandler, SyscallOutcome};
use scheduler::{IterationAwareMlfq, TaskAgenda};
use senses::{SensoryBridge, SensoryPacket};

/// Capability screen consulted before every syscall is routed.
///
/// Returns `true` to admit the syscall, `false` to deny it with
/// [`SyscallError::PermissionDenied`].  This is the integration point for the
/// object-capability token system: a future implementation resolves the running
/// agent's capability set and only admits syscalls the agent holds a token for.
pub type CapabilityPolicy = fn(&SyscallEnum) -> bool;

/// Default capability policy: admit every syscall.
///
/// Replaced by a token-aware predicate once the object-capability system lands.
pub fn allow_all(_syscall: &SyscallEnum) -> bool {
    true
}

/// Routes [`corpus`] syscalls to the live hosted-kernel subsystems.
///
/// Holds borrows of the real subsystems for the duration of one dispatch batch;
/// the scheduler constructs a fresh handler (binding the capability policy to the
/// running agent) around the borrows it already owns, calls
/// [`corpus::dispatch`], then drops it.  Nothing in the giant somatic loop is
/// restructured — the handler is purely additive.
pub struct KernelSyscallHandler<'a> {
    /// MLFQ dispatcher whose quantum boundary `Yield` drives.
    scheduler: &'a mut IterationAwareMlfq,
    /// Live task agenda the scheduler boosts on a cooperative yield.
    agenda: &'a mut TaskAgenda,
    /// Physical frame allocator backing `AllocateFrames`.
    frames: &'a FrameAllocator,
    /// Sensory bridge drained by `ReadSensoryPacket`.
    senses: &'a SensoryBridge,
    /// Tool registry dispatched into by `DispatchTool`.
    tools: &'a praxis::ToolRegistry,
    /// Monotonic interoceptive tick counter advanced by `SleepUntilTick`.
    tick: u64,
    /// Capability screen consulted before every syscall.
    policy: CapabilityPolicy,
}

impl<'a> KernelSyscallHandler<'a> {
    /// Constructs a handler over the live subsystems with the permissive default
    /// capability policy ([`allow_all`]).
    pub fn new(
        scheduler: &'a mut IterationAwareMlfq,
        agenda: &'a mut TaskAgenda,
        frames: &'a FrameAllocator,
        senses: &'a SensoryBridge,
        tools: &'a praxis::ToolRegistry,
    ) -> Self {
        Self::with_policy(scheduler, agenda, frames, senses, tools, allow_all)
    }

    /// Constructs a handler with an explicit capability policy.
    ///
    /// The scheduler uses this overload to bind the running agent's capability
    /// set into `policy` for the duration of the dispatch.
    pub fn with_policy(
        scheduler: &'a mut IterationAwareMlfq,
        agenda: &'a mut TaskAgenda,
        frames: &'a FrameAllocator,
        senses: &'a SensoryBridge,
        tools: &'a praxis::ToolRegistry,
        policy: CapabilityPolicy,
    ) -> Self {
        Self {
            scheduler,
            agenda,
            frames,
            senses,
            tools,
            tick: 0,
            policy,
        }
    }

    /// Current interoceptive tick value (advanced by `SleepUntilTick`).
    pub fn tick(&self) -> u64 {
        self.tick
    }

    /// Convenience wrapper around [`corpus::dispatch`] for this handler.
    ///
    /// The somatic loop can call `handler.dispatch(syscall)` directly instead of
    /// reaching for the free function.
    pub fn dispatch(&mut self, syscall: SyscallEnum) -> Result<SyscallOutcome, SyscallError> {
        corpus::dispatch(syscall, self)
    }

    /// Screens `syscall` through the capability policy.
    fn admits(&self, syscall: &SyscallEnum) -> Result<(), SyscallError> {
        if (self.policy)(syscall) {
            Ok(())
        } else {
            Err(SyscallError::PermissionDenied)
        }
    }
}

impl SyscallHandler for KernelSyscallHandler<'_> {
    fn yield_quantum(&mut self) -> Result<SyscallOutcome, SyscallError> {
        self.admits(&SyscallEnum::Yield)?;
        // A cooperative yield is the scheduler's quantum boundary: run the
        // starvation-prevention boost so lower tiers cannot be starved across
        // the yield, then return control to the agenda for re-selection.
        let _boosted = self.scheduler.check_and_boost(self.agenda);
        Ok(SyscallOutcome::Yielded)
    }

    fn sleep_until_tick(&mut self) -> Result<SyscallOutcome, SyscallError> {
        self.admits(&SyscallEnum::SleepUntilTick)?;
        // Park the caller until the next interoceptive tick.  The hosted kernel
        // models ticks as a monotonic counter advanced by the homeostatic
        // monitor; parking advances the handler's view to the tick the caller
        // will wake on.
        self.tick = self.tick.saturating_add(1);
        Ok(SyscallOutcome::Slept)
    }

    fn allocate_frames(&mut self, frames: usize) -> Result<SyscallOutcome, SyscallError> {
        self.admits(&SyscallEnum::AllocateFrames { frames })?;
        match self.frames.allocate(frames) {
            Ok(alloc) => Ok(SyscallOutcome::FramesAllocated(alloc.frames)),
            // Zero-sized requests are a caller bug; capacity exhaustion is a
            // genuine resource-unavailable condition.
            Err(corpus::FrameAllocatorError::ZeroSizedRequest) => Err(SyscallError::Invalid),
            Err(corpus::FrameAllocatorError::OutOfMemory) => Err(SyscallError::Unavailable),
        }
    }

    fn read_sensory_packet(&mut self) -> Result<SyscallOutcome, SyscallError> {
        self.admits(&SyscallEnum::ReadSensoryPacket)?;
        match self.senses.next_prioritized_packet() {
            // Encode a real packet into the opaque handle: the low bits carry the
            // packet's discriminant (so the caller can route to the right
            // decoder) and the high bits carry the bridge's remaining queue depth
            // (a stable, resolvable index back into the owning subsystem).
            Some(prioritized) => {
                let kind: u64 = match prioritized.packet {
                    SensoryPacket::Text(_) => 0,
                    SensoryPacket::Pcm(_) => 1,
                    SensoryPacket::Image { .. } => 2,
                };
                let remaining = self.senses.queue_len() as u64;
                Ok(SyscallOutcome::SensoryPacket(SensoryHandle(
                    (remaining << 8) | kind,
                )))
            }
            // Empty queue is not an error condition the kernel can satisfy now.
            None => Err(SyscallError::Unavailable),
        }
    }

    fn dispatch_tool(&mut self, tool_id: u32) -> Result<SyscallOutcome, SyscallError> {
        self.admits(&SyscallEnum::DispatchTool { tool_id })?;
        // `corpus` carries a numeric tool id to stay decoupled from praxis'
        // string-keyed registry; resolve it against the sorted id list so the
        // mapping is stable for a given registry population.
        let ids = self.tools.list();
        let name = ids
            .get(tool_id as usize)
            .ok_or(SyscallError::Invalid)?
            .clone();
        let envelope =
            praxis::ToolEnvelope::new(praxis::Bus::Mcp, name, Vec::new(), tool_id as u64);
        match self.tools.dispatch(&envelope) {
            Ok(_output) => Ok(SyscallOutcome::ToolDispatched(DispatchTicket(
                envelope.correlation_id,
            ))),
            // A tripped breaker or a tool that rejected the (empty) probe payload
            // means the pathway is currently unavailable rather than forbidden.
            Err(_) => Err(SyscallError::Unavailable),
        }
    }
}

// ---------------------------------------------------------------------------
// Agent table — self-contained PCB lifecycle bookkeeping.
// ---------------------------------------------------------------------------

use corpus::{AgentPcb, AgentPid, AgentState, TransitionError};
use std::collections::BTreeMap;

/// Owns the [`corpus::AgentPcb`]s for every admitted agent, keyed by
/// [`corpus::AgentPid`], and exposes the validated lifecycle transitions.
///
/// This is intentionally self-contained so it can be adopted incrementally
/// without restructuring the hosted somatic loop.  A future scheduler call
/// pattern is:
///
/// ```ignore
/// table.admit(pid, priority, credits);     // New -> Ready at enqueue time
/// table.run(pid)?;                          // Ready -> Running before dispatch
/// // ... agent executes a quantum ...
/// table.preempt(pid)?;                      // Running -> Ready on quantum end
/// table.block(pid)?;                        // Running -> Blocked on SleepUntilTick / I/O
/// table.unblock(pid)?;                      // Blocked -> Ready when the wait resolves
/// table.terminate(pid)?;                    // -> Terminated on shutdown
/// ```
///
/// Every method delegates to the PCB's validated transition, so illegal edges
/// surface as [`corpus::TransitionError`] without panicking.
#[derive(Debug, Default)]
pub struct AgentTable {
    agents: BTreeMap<AgentPid, AgentPcb>,
}

/// Failure modes for [`AgentTable`] operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTableError {
    /// No PCB is registered for the given pid.
    UnknownAgent(AgentPid),
    /// The requested lifecycle transition was illegal.
    Transition(TransitionError),
}

impl From<TransitionError> for AgentTableError {
    fn from(err: TransitionError) -> Self {
        AgentTableError::Transition(err)
    }
}

impl AgentTable {
    /// Creates an empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of agents currently tracked (including terminated ones not yet reaped).
    pub fn len(&self) -> usize {
        self.agents.len()
    }

    /// True when no agents are tracked.
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }

    /// Returns the current state of `pid`, if tracked.
    pub fn state(&self, pid: AgentPid) -> Option<AgentState> {
        self.agents.get(&pid).map(|pcb| pcb.state)
    }

    /// Admits a new agent: inserts a PCB and drives `New -> Ready`.
    ///
    /// Returns the resulting state. If a PCB already existed for `pid` it is
    /// replaced (re-admission).
    pub fn admit(
        &mut self,
        pid: AgentPid,
        priority: u8,
        token_credits: u32,
    ) -> Result<AgentState, AgentTableError> {
        let mut pcb = AgentPcb::new(pid, priority, token_credits);
        pcb.mark_ready()?;
        let state = pcb.state;
        self.agents.insert(pid, pcb);
        Ok(state)
    }

    fn with_pcb<F>(&mut self, pid: AgentPid, f: F) -> Result<AgentState, AgentTableError>
    where
        F: FnOnce(&mut AgentPcb) -> Result<AgentState, TransitionError>,
    {
        let pcb = self
            .agents
            .get_mut(&pid)
            .ok_or(AgentTableError::UnknownAgent(pid))?;
        f(pcb)?;
        Ok(pcb.state)
    }

    /// `Ready -> Running`: dispatch the agent onto a quantum.
    pub fn run(&mut self, pid: AgentPid) -> Result<AgentState, AgentTableError> {
        self.with_pcb(pid, |pcb| pcb.mark_running())
    }

    /// `Running -> Blocked`: park the agent on I/O or a wait primitive.
    pub fn block(&mut self, pid: AgentPid) -> Result<AgentState, AgentTableError> {
        self.with_pcb(pid, |pcb| pcb.mark_blocked())
    }

    /// `Blocked -> Ready`: the wait resolved; re-enqueue the agent.
    pub fn unblock(&mut self, pid: AgentPid) -> Result<AgentState, AgentTableError> {
        self.with_pcb(pid, |pcb| pcb.mark_unblocked())
    }

    /// `Running -> Ready`: quantum expired; re-enqueue the agent.
    pub fn preempt(&mut self, pid: AgentPid) -> Result<AgentState, AgentTableError> {
        self.with_pcb(pid, |pcb| pcb.mark_preempted())
    }

    /// `Running | Ready | Blocked -> Terminated`: clean shutdown.
    pub fn terminate(&mut self, pid: AgentPid) -> Result<AgentState, AgentTableError> {
        self.with_pcb(pid, |pcb| pcb.terminate())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus::dispatch;
    use interoception::{HomeostaticMonitor, InteroceptiveSensorBundle};
    use senses::HumanGuidance;

    // ── Test fixtures ────────────────────────────────────────────────────────

    /// Builds the live subsystem set used to construct a handler in tests.
    fn fixtures() -> (
        IterationAwareMlfq,
        TaskAgenda,
        FrameAllocator,
        SensoryBridge,
        praxis::ToolRegistry,
    ) {
        (
            IterationAwareMlfq::default(),
            TaskAgenda::new(),
            FrameAllocator::new(64),
            SensoryBridge::new(HumanGuidance::new("test")),
            praxis::ToolRegistry::new(),
        )
    }

    // ── Syscall routing ──────────────────────────────────────────────────────

    #[test]
    fn yield_routes_to_scheduler_boost() {
        let (mut sched, mut agenda, frames, senses, tools) = fixtures();
        let mut h = KernelSyscallHandler::new(&mut sched, &mut agenda, &frames, &senses, &tools);
        assert_eq!(
            dispatch(SyscallEnum::Yield, &mut h),
            Ok(SyscallOutcome::Yielded)
        );
    }

    #[test]
    fn sleep_routes_to_interoception_tick() {
        // Touch the interoception API to keep the wiring honest even though the
        // tick counter lives on the handler.
        let bundle = InteroceptiveSensorBundle::with_defaults();
        let monitor = HomeostaticMonitor::new(1.0, 0.5, 8);
        let _ = bundle.sample(&monitor, 0, 1, 0);

        let (mut sched, mut agenda, frames, senses, tools) = fixtures();
        let mut h = KernelSyscallHandler::new(&mut sched, &mut agenda, &frames, &senses, &tools);
        assert_eq!(h.tick(), 0);
        assert_eq!(
            dispatch(SyscallEnum::SleepUntilTick, &mut h),
            Ok(SyscallOutcome::Slept)
        );
        assert_eq!(h.tick(), 1);
    }

    #[test]
    fn allocate_frames_routes_to_frame_allocator() {
        let (mut sched, mut agenda, frames, senses, tools) = fixtures();
        let mut h = KernelSyscallHandler::new(&mut sched, &mut agenda, &frames, &senses, &tools);
        assert_eq!(
            dispatch(SyscallEnum::AllocateFrames { frames: 4 }, &mut h),
            Ok(SyscallOutcome::FramesAllocated(4))
        );
    }

    #[test]
    fn allocate_frames_reports_unavailable_when_exhausted() {
        let (mut sched, mut agenda, frames, senses, tools) = fixtures();
        let mut h = KernelSyscallHandler::new(&mut sched, &mut agenda, &frames, &senses, &tools);
        // Pool is 64 frames; request more than capacity.
        assert_eq!(
            dispatch(SyscallEnum::AllocateFrames { frames: 65 }, &mut h),
            Err(SyscallError::Unavailable)
        );
    }

    #[test]
    fn allocate_zero_frames_is_invalid() {
        let (mut sched, mut agenda, frames, senses, tools) = fixtures();
        let mut h = KernelSyscallHandler::new(&mut sched, &mut agenda, &frames, &senses, &tools);
        assert_eq!(
            dispatch(SyscallEnum::AllocateFrames { frames: 0 }, &mut h),
            Err(SyscallError::Invalid)
        );
    }

    #[test]
    fn read_sensory_packet_routes_to_bridge() {
        let (mut sched, mut agenda, frames, senses, tools) = fixtures();
        senses.packetize_text("hello");
        let mut h = KernelSyscallHandler::new(&mut sched, &mut agenda, &frames, &senses, &tools);
        match dispatch(SyscallEnum::ReadSensoryPacket, &mut h) {
            Ok(SyscallOutcome::SensoryPacket(SensoryHandle(handle))) => {
                // Low byte is the Text discriminant (0); high bits the depth (0).
                assert_eq!(handle & 0xff, 0);
            }
            other => panic!("expected SensoryPacket, got {other:?}"),
        }
    }

    #[test]
    fn read_sensory_packet_unavailable_when_empty() {
        let (mut sched, mut agenda, frames, senses, tools) = fixtures();
        let mut h = KernelSyscallHandler::new(&mut sched, &mut agenda, &frames, &senses, &tools);
        assert_eq!(
            dispatch(SyscallEnum::ReadSensoryPacket, &mut h),
            Err(SyscallError::Unavailable)
        );
    }

    #[test]
    fn dispatch_tool_routes_to_registry() {
        let (mut sched, mut agenda, frames, senses, tools) = fixtures();
        // ToolRegistry::new() registers clock/echo/text-io; index 0 of the sorted
        // list resolves to a real, dispatchable tool.
        let mut h = KernelSyscallHandler::new(&mut sched, &mut agenda, &frames, &senses, &tools);
        match dispatch(SyscallEnum::DispatchTool { tool_id: 0 }, &mut h) {
            Ok(SyscallOutcome::ToolDispatched(_)) => {}
            other => panic!("expected ToolDispatched, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_tool_invalid_for_unknown_id() {
        let (mut sched, mut agenda, frames, senses, tools) = fixtures();
        let mut h = KernelSyscallHandler::new(&mut sched, &mut agenda, &frames, &senses, &tools);
        assert_eq!(
            dispatch(SyscallEnum::DispatchTool { tool_id: 9999 }, &mut h),
            Err(SyscallError::Invalid)
        );
    }

    // ── Capability seam ──────────────────────────────────────────────────────

    fn deny_all(_syscall: &SyscallEnum) -> bool {
        false
    }

    #[test]
    fn capability_policy_denies_every_syscall() {
        let (mut sched, mut agenda, frames, senses, tools) = fixtures();
        senses.packetize_text("hello");
        let mut h = KernelSyscallHandler::with_policy(
            &mut sched,
            &mut agenda,
            &frames,
            &senses,
            &tools,
            deny_all,
        );
        for sc in [
            SyscallEnum::Yield,
            SyscallEnum::SleepUntilTick,
            SyscallEnum::AllocateFrames { frames: 1 },
            SyscallEnum::ReadSensoryPacket,
            SyscallEnum::DispatchTool { tool_id: 0 },
        ] {
            assert_eq!(dispatch(sc, &mut h), Err(SyscallError::PermissionDenied));
        }
    }

    fn deny_senses(syscall: &SyscallEnum) -> bool {
        !matches!(syscall, SyscallEnum::ReadSensoryPacket)
    }

    #[test]
    fn capability_policy_can_deny_selectively() {
        let (mut sched, mut agenda, frames, senses, tools) = fixtures();
        senses.packetize_text("hello");
        let mut h = KernelSyscallHandler::with_policy(
            &mut sched,
            &mut agenda,
            &frames,
            &senses,
            &tools,
            deny_senses,
        );
        // Sensory read is denied by policy...
        assert_eq!(
            dispatch(SyscallEnum::ReadSensoryPacket, &mut h),
            Err(SyscallError::PermissionDenied)
        );
        // ...but yield is still admitted.
        assert_eq!(
            dispatch(SyscallEnum::Yield, &mut h),
            Ok(SyscallOutcome::Yielded)
        );
    }

    // ── AgentTable / PCB lifecycle ───────────────────────────────────────────

    #[test]
    fn admit_drives_new_to_ready() {
        let mut table = AgentTable::new();
        assert_eq!(table.admit(AgentPid(1), 0, 1024), Ok(AgentState::Ready));
        assert_eq!(table.state(AgentPid(1)), Some(AgentState::Ready));
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn full_legal_lifecycle() {
        let mut table = AgentTable::new();
        let pid = AgentPid(7);
        table.admit(pid, 0, 0).unwrap();
        assert_eq!(table.run(pid), Ok(AgentState::Running));
        assert_eq!(table.block(pid), Ok(AgentState::Blocked));
        assert_eq!(table.unblock(pid), Ok(AgentState::Ready));
        assert_eq!(table.run(pid), Ok(AgentState::Running));
        assert_eq!(table.preempt(pid), Ok(AgentState::Ready));
        assert_eq!(table.run(pid), Ok(AgentState::Running));
        assert_eq!(table.terminate(pid), Ok(AgentState::Terminated));
    }

    #[test]
    fn illegal_transition_errors_without_mutating() {
        let mut table = AgentTable::new();
        let pid = AgentPid(3);
        table.admit(pid, 0, 0).unwrap(); // -> Ready
                                         // Ready cannot block directly.
        match table.block(pid) {
            Err(AgentTableError::Transition(TransitionError { from, to })) => {
                assert_eq!(from, AgentState::Ready);
                assert_eq!(to, AgentState::Blocked);
            }
            other => panic!("expected Transition error, got {other:?}"),
        }
        // State is unchanged.
        assert_eq!(table.state(pid), Some(AgentState::Ready));
    }

    #[test]
    fn terminated_is_terminal() {
        let mut table = AgentTable::new();
        let pid = AgentPid(5);
        table.admit(pid, 0, 0).unwrap();
        table.run(pid).unwrap();
        table.terminate(pid).unwrap();
        assert!(table.run(pid).is_err());
        assert!(table.terminate(pid).is_err());
        assert_eq!(table.state(pid), Some(AgentState::Terminated));
    }

    #[test]
    fn unknown_agent_is_reported() {
        let mut table = AgentTable::new();
        assert_eq!(
            table.run(AgentPid(42)),
            Err(AgentTableError::UnknownAgent(AgentPid(42)))
        );
    }
}
