//! Per-agent lifecycle audit trail.
//!
//! The audit log is the end-of-pipeline observability surface called out in the
//! Phase 1 roadmap exit criteria: every task that traverses senses → vita →
//! scheduler → backend must leave a trace here.

/// A single observable lifecycle event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditEntry {
    /// A new task was pulled from the agenda and dispatched.
    TaskStarted {
        agent_id: String,
        task_id: u64,
        tier: u8,
        prompt: String,
    },
    /// The backend returned a complete streamed response.
    TaskCompleted {
        agent_id: String,
        task_id: u64,
        tokens_emitted: u32,
        response: String,
    },
    /// The backend returned an error or the stream was cancelled.
    TaskFailed {
        agent_id: String,
        task_id: u64,
        error: String,
    },
    /// Lifecycle transitioned into the sleep state.
    SleepEntered { agent_id: String },
    /// Lifecycle transitioned (back) into the waking state.
    WakeEntered { agent_id: String },
}

/// Append-only audit log.
#[derive(Debug, Default, Clone)]
pub struct AuditLog {
    entries: Vec<AuditEntry>,
}

impl AuditLog {
    /// Creates an empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends an entry.
    pub fn push(&mut self, entry: AuditEntry) {
        self.entries.push(entry);
    }

    /// Borrows the full entry sequence.
    pub fn entries(&self) -> &[AuditEntry] {
        &self.entries
    }

    /// Returns the number of recorded entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true when no entries have been recorded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
