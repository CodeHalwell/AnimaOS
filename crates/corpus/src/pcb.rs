//! Agent Process Control Block (PCB).

/// Stable identifier for an agent process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AgentPid(pub u32);

/// Coarse-grained execution state of an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    /// Created but not yet scheduled.
    New,
    /// Eligible for scheduling.
    Ready,
    /// Currently executing on a scheduler quantum.
    Running,
    /// Waiting on I/O or a synchronization primitive.
    Blocked,
    /// Cleanly shut down.
    Terminated,
}

/// Per-agent control block tracked by the kernel scheduler.
#[derive(Debug, Clone)]
pub struct AgentPcb {
    /// Stable identifier.
    pub pid: AgentPid,
    /// Current execution state.
    pub state: AgentState,
    /// MLFQ priority tier (0 = highest).
    pub priority: u8,
    /// Active token budget granted by the credit backpressure system.
    pub token_credits: u32,
}

impl AgentPcb {
    /// Creates a new PCB in the `New` state.
    pub fn new(pid: AgentPid, priority: u8, token_credits: u32) -> Self {
        Self {
            pid,
            state: AgentState::New,
            priority,
            token_credits,
        }
    }

    /// Transitions to `Ready`, returning the previous state.
    pub fn mark_ready(&mut self) -> AgentState {
        core::mem::replace(&mut self.state, AgentState::Ready)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_pcb_is_in_new_state() {
        let pcb = AgentPcb::new(AgentPid(1), 0, 1024);
        assert_eq!(pcb.state, AgentState::New);
        assert_eq!(pcb.priority, 0);
        assert_eq!(pcb.token_credits, 1024);
    }

    #[test]
    fn mark_ready_returns_previous_state() {
        let mut pcb = AgentPcb::new(AgentPid(2), 1, 512);
        let prev = pcb.mark_ready();
        assert_eq!(prev, AgentState::New);
        assert_eq!(pcb.state, AgentState::Ready);
    }
}
