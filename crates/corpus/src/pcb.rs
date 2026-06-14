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

impl AgentState {
    /// Returns `true` if `self -> next` is a legal lifecycle edge.
    ///
    /// Legal edges:
    /// - `New -> Ready`
    /// - `Ready -> Running`
    /// - `Running -> Blocked`
    /// - `Running -> Ready` (preemption)
    /// - `Blocked -> Ready`
    /// - `Running | Ready | Blocked -> Terminated`
    ///
    /// All other edges (including any transition out of `Terminated`) are illegal.
    pub fn can_transition_to(self, next: AgentState) -> bool {
        use AgentState::*;
        matches!(
            (self, next),
            (New, Ready)
                | (Ready, Running)
                | (Running, Blocked)
                | (Running, Ready)
                | (Blocked, Ready)
                | (Running, Terminated)
                | (Ready, Terminated)
                | (Blocked, Terminated)
        )
    }
}

/// Rejected attempt to perform an illegal lifecycle transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionError {
    /// State the agent was in.
    pub from: AgentState,
    /// State the caller attempted to move to.
    pub to: AgentState,
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

    /// Validated transition to `next`; returns the previous state or rejects.
    fn transition(&mut self, next: AgentState) -> Result<AgentState, TransitionError> {
        if self.state.can_transition_to(next) {
            Ok(core::mem::replace(&mut self.state, next))
        } else {
            Err(TransitionError {
                from: self.state,
                to: next,
            })
        }
    }

    /// `New -> Ready`; returns the previous state or rejects.
    pub fn mark_ready(&mut self) -> Result<AgentState, TransitionError> {
        self.transition(AgentState::Ready)
    }

    /// `Ready -> Running`; returns the previous state or rejects.
    pub fn mark_running(&mut self) -> Result<AgentState, TransitionError> {
        self.transition(AgentState::Running)
    }

    /// `Running -> Blocked`; returns the previous state or rejects.
    pub fn mark_blocked(&mut self) -> Result<AgentState, TransitionError> {
        self.transition(AgentState::Blocked)
    }

    /// `Running -> Ready` (preemption); returns the previous state or rejects.
    pub fn mark_preempted(&mut self) -> Result<AgentState, TransitionError> {
        self.transition(AgentState::Ready)
    }

    /// `Blocked -> Ready`; returns the previous state or rejects.
    pub fn mark_unblocked(&mut self) -> Result<AgentState, TransitionError> {
        self.transition(AgentState::Ready)
    }

    /// `Running | Ready | Blocked -> Terminated`; returns the previous state or rejects.
    pub fn terminate(&mut self) -> Result<AgentState, TransitionError> {
        self.transition(AgentState::Terminated)
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
        assert_eq!(prev, Ok(AgentState::New));
        assert_eq!(pcb.state, AgentState::Ready);
    }

    fn pcb_in(state: AgentState) -> AgentPcb {
        let mut pcb = AgentPcb::new(AgentPid(99), 0, 0);
        pcb.state = state;
        pcb
    }

    // ---- legal transitions ----

    #[test]
    fn new_to_ready_is_legal() {
        let mut pcb = pcb_in(AgentState::New);
        assert_eq!(pcb.mark_ready(), Ok(AgentState::New));
        assert_eq!(pcb.state, AgentState::Ready);
    }

    #[test]
    fn ready_to_running_is_legal() {
        let mut pcb = pcb_in(AgentState::Ready);
        assert_eq!(pcb.mark_running(), Ok(AgentState::Ready));
        assert_eq!(pcb.state, AgentState::Running);
    }

    #[test]
    fn running_to_blocked_is_legal() {
        let mut pcb = pcb_in(AgentState::Running);
        assert_eq!(pcb.mark_blocked(), Ok(AgentState::Running));
        assert_eq!(pcb.state, AgentState::Blocked);
    }

    #[test]
    fn running_to_ready_preemption_is_legal() {
        let mut pcb = pcb_in(AgentState::Running);
        assert_eq!(pcb.mark_preempted(), Ok(AgentState::Running));
        assert_eq!(pcb.state, AgentState::Ready);
    }

    #[test]
    fn blocked_to_ready_is_legal() {
        let mut pcb = pcb_in(AgentState::Blocked);
        assert_eq!(pcb.mark_unblocked(), Ok(AgentState::Blocked));
        assert_eq!(pcb.state, AgentState::Ready);
    }

    #[test]
    fn terminate_is_legal_from_running_ready_blocked() {
        for from in [AgentState::Running, AgentState::Ready, AgentState::Blocked] {
            let mut pcb = pcb_in(from);
            assert_eq!(pcb.terminate(), Ok(from));
            assert_eq!(pcb.state, AgentState::Terminated);
        }
    }

    // ---- illegal transitions: must error, not panic, and not mutate ----

    #[test]
    fn new_cannot_run_directly() {
        let mut pcb = pcb_in(AgentState::New);
        assert_eq!(
            pcb.mark_running(),
            Err(TransitionError {
                from: AgentState::New,
                to: AgentState::Running
            })
        );
        assert_eq!(pcb.state, AgentState::New);
    }

    #[test]
    fn new_cannot_block() {
        let mut pcb = pcb_in(AgentState::New);
        assert!(pcb.mark_blocked().is_err());
        assert_eq!(pcb.state, AgentState::New);
    }

    #[test]
    fn new_cannot_terminate() {
        let mut pcb = pcb_in(AgentState::New);
        assert!(pcb.terminate().is_err());
        assert_eq!(pcb.state, AgentState::New);
    }

    #[test]
    fn ready_cannot_block() {
        let mut pcb = pcb_in(AgentState::Ready);
        assert!(pcb.mark_blocked().is_err());
        assert_eq!(pcb.state, AgentState::Ready);
    }

    #[test]
    fn ready_cannot_re_ready() {
        let mut pcb = pcb_in(AgentState::Ready);
        assert!(pcb.mark_ready().is_err());
        assert_eq!(pcb.state, AgentState::Ready);
    }

    #[test]
    fn running_cannot_run_again() {
        let mut pcb = pcb_in(AgentState::Running);
        assert!(pcb.mark_running().is_err());
        assert_eq!(pcb.state, AgentState::Running);
    }

    #[test]
    fn blocked_cannot_run_directly() {
        let mut pcb = pcb_in(AgentState::Blocked);
        assert!(pcb.mark_running().is_err());
        assert_eq!(pcb.state, AgentState::Blocked);
    }

    #[test]
    fn blocked_cannot_block_again() {
        let mut pcb = pcb_in(AgentState::Blocked);
        assert!(pcb.mark_blocked().is_err());
        assert_eq!(pcb.state, AgentState::Blocked);
    }

    #[test]
    fn terminated_is_terminal() {
        for to in [
            AgentState::New,
            AgentState::Ready,
            AgentState::Running,
            AgentState::Blocked,
            AgentState::Terminated,
        ] {
            let mut pcb = pcb_in(AgentState::Terminated);
            assert!(!AgentState::Terminated.can_transition_to(to));
            // No public method drives any of these from Terminated; verify via terminate/ready.
            assert!(pcb.mark_ready().is_err());
            assert!(pcb.terminate().is_err());
            assert_eq!(pcb.state, AgentState::Terminated);
        }
    }

    #[test]
    fn can_transition_to_matches_edge_table() {
        use AgentState::*;
        // A representative legal edge and a representative illegal edge.
        assert!(New.can_transition_to(Ready));
        assert!(Running.can_transition_to(Ready));
        assert!(Blocked.can_transition_to(Ready));
        assert!(!New.can_transition_to(Running));
        assert!(!Ready.can_transition_to(Blocked));
        assert!(!Terminated.can_transition_to(Ready));
    }
}
