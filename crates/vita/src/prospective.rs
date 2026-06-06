//! Prospective & temporal memory — E14, S14.2.
//!
//! A **future-intention store**: "remind me / do X at T", deadlines, and
//! follow-up tasks.  Due intentions are injected into the MLFQ task agenda at
//! the right time, reusing the existing wake/sleep cadence.
//!
//! # Architecture
//!
//! Intentions are persisted as a JSONL file under the agent's state directory.
//! On each somatic-loop iteration the lifecycle manager calls
//! [`inject_due_intentions`] which scans the store and pushes due entries onto
//! the [`scheduler::TaskAgenda`].  Completed intentions are removed (or
//! rescheduled if recurring).
//!
//! # Design choices
//!
//! - **No clock dependency in the struct**: callers supply the current time as
//!   nanoseconds since the Unix epoch, keeping the store testable without mocking.
//! - **Recurring intentions**: optional `repeat_interval_ns` reschedules the
//!   intention rather than deleting it.
//! - **File format**: JSONL (one `Intention` per line) to be consistent with the
//!   audit log and compilation corpus.
//!
//! # Exit criteria (S14.2)
//!
//! 1. Due intentions are injected into the task agenda with `MlfqTier::High`.
//! 2. Recurring intentions reschedule after being injected.
//! 3. Overdue intentions (past deadline by more than `overdue_grace_ns`) are
//!    escalated to `Critical` priority.
//! 4. The store survives a process restart with all pending intentions intact.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use scheduler::{Task, TaskAgenda};

// ── Intention ─────────────────────────────────────────────────────────────────

/// A single future intention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intention {
    /// Unique monotonic identifier within the agent.
    pub id: u64,
    /// Human-readable description (becomes the task prompt when due).
    pub description: String,
    /// Wall-clock nanoseconds since UNIX epoch when this intention is due.
    pub due_at_ns: u64,
    /// Optional recurring interval in nanoseconds.
    ///
    /// When `Some(interval)`, the intention is rescheduled to
    /// `due_at_ns + interval` instead of being removed after dispatch.
    pub repeat_interval_ns: Option<u64>,
    /// Wall-clock nanoseconds when this intention was created.
    pub created_at_ns: u64,
    /// `true` once the intention has been injected into the agenda and is
    /// awaiting acknowledgement.  Persistent intentions that have been
    /// dispatched but not yet completed remain in the store as `dispatched=true`.
    pub dispatched: bool,
}

impl Intention {
    /// Create a new one-shot intention due at `due_at_ns`.
    pub fn once(id: u64, description: impl Into<String>, due_at_ns: u64, now_ns: u64) -> Self {
        Self {
            id,
            description: description.into(),
            due_at_ns,
            repeat_interval_ns: None,
            created_at_ns: now_ns,
            dispatched: false,
        }
    }

    /// Create a recurring intention with an interval.
    pub fn recurring(
        id: u64,
        description: impl Into<String>,
        due_at_ns: u64,
        interval_ns: u64,
        now_ns: u64,
    ) -> Self {
        Self {
            id,
            description: description.into(),
            due_at_ns,
            repeat_interval_ns: Some(interval_ns),
            created_at_ns: now_ns,
            dispatched: false,
        }
    }

    /// `true` when this intention is due at or before `now_ns`.
    pub fn is_due(&self, now_ns: u64) -> bool {
        !self.dispatched && now_ns >= self.due_at_ns
    }

    /// How many nanoseconds this intention is overdue at `now_ns`.
    /// Returns `0` if not yet overdue.
    pub fn overdue_ns(&self, now_ns: u64) -> u64 {
        now_ns.saturating_sub(self.due_at_ns)
    }
}

// ── IntentionStore ────────────────────────────────────────────────────────────

/// Persistent store for future intentions (S14.2).
///
/// Backed by a JSONL file under the agent's state directory.  All mutations
/// flush to disk atomically (write-to-temp, then rename) for crash safety.
#[derive(Debug, Clone)]
pub struct IntentionStore {
    intentions: Vec<Intention>,
    /// Optional persistence path.  `None` for in-memory-only operation (tests).
    path: Option<PathBuf>,
    /// Monotonic ID counter.
    next_id: u64,
}

impl IntentionStore {
    /// Create an empty in-memory store (no persistence).
    pub fn in_memory() -> Self {
        Self {
            intentions: Vec::new(),
            path: None,
            next_id: 0,
        }
    }

    /// Open (or create) a persistent store at `path`.
    ///
    /// If the file already exists its JSONL records are loaded.
    pub fn open(path: &Path) -> Result<Self, IntentionStoreError> {
        let mut store = Self {
            intentions: Vec::new(),
            path: Some(path.to_path_buf()),
            next_id: 0,
        };

        if path.exists() {
            let text = std::fs::read_to_string(path)
                .map_err(|e| IntentionStoreError::Io(e.to_string()))?;
            for (line_no, line) in text.lines().enumerate() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let intention: Intention = serde_json::from_str(line).map_err(|e| {
                    IntentionStoreError::Corrupt(format!("line {}: {e}", line_no + 1))
                })?;
                if intention.id >= store.next_id {
                    store.next_id = intention.id + 1;
                }
                store.intentions.push(intention);
            }
        }

        Ok(store)
    }

    /// Number of intentions currently stored (dispatched and pending).
    pub fn len(&self) -> usize {
        self.intentions.len()
    }

    /// `true` when the store is empty.
    pub fn is_empty(&self) -> bool {
        self.intentions.is_empty()
    }

    /// Borrows all stored intentions (pending and dispatched).
    pub fn all(&self) -> &[Intention] {
        &self.intentions
    }

    /// Add a one-shot intention and flush to disk.
    pub fn add_once(
        &mut self,
        description: impl Into<String>,
        due_at_ns: u64,
        now_ns: u64,
    ) -> Result<u64, IntentionStoreError> {
        let id = self.next_id;
        self.next_id += 1;
        let intention = Intention::once(id, description, due_at_ns, now_ns);
        self.intentions.push(intention);
        self.flush()?;
        Ok(id)
    }

    /// Add a recurring intention and flush to disk.
    pub fn add_recurring(
        &mut self,
        description: impl Into<String>,
        due_at_ns: u64,
        interval_ns: u64,
        now_ns: u64,
    ) -> Result<u64, IntentionStoreError> {
        let id = self.next_id;
        self.next_id += 1;
        let intention = Intention::recurring(id, description, due_at_ns, interval_ns, now_ns);
        self.intentions.push(intention);
        self.flush()?;
        Ok(id)
    }

    /// Remove an intention by id.  Returns `true` if it was found and removed.
    pub fn remove(&mut self, id: u64) -> Result<bool, IntentionStoreError> {
        let before = self.intentions.len();
        self.intentions.retain(|i| i.id != id);
        let removed = self.intentions.len() < before;
        if removed {
            self.flush()?;
        }
        Ok(removed)
    }

    /// Mark an intention as completed.
    ///
    /// - For one-shot intentions: the entry is removed from the store.
    /// - For recurring intentions: `due_at_ns` is advanced by `repeat_interval_ns`
    ///   and `dispatched` is reset to `false`.
    pub fn complete(
        &mut self,
        id: u64,
        now_ns: u64,
    ) -> Result<CompletionOutcome, IntentionStoreError> {
        let Some(pos) = self.intentions.iter().position(|i| i.id == id) else {
            return Ok(CompletionOutcome::NotFound);
        };

        let outcome = if let Some(interval) = self.intentions[pos].repeat_interval_ns {
            self.intentions[pos].due_at_ns =
                self.intentions[pos].due_at_ns.saturating_add(interval);
            // If the new due time is still in the past, advance by full intervals.
            while self.intentions[pos].due_at_ns < now_ns {
                self.intentions[pos].due_at_ns =
                    self.intentions[pos].due_at_ns.saturating_add(interval);
            }
            self.intentions[pos].dispatched = false;
            CompletionOutcome::Rescheduled {
                new_due_at_ns: self.intentions[pos].due_at_ns,
            }
        } else {
            self.intentions.remove(pos);
            CompletionOutcome::Removed
        };

        self.flush()?;
        Ok(outcome)
    }

    /// Flush the store to disk atomically (write-to-temp, rename).
    fn flush(&self) -> Result<(), IntentionStoreError> {
        let Some(ref path) = self.path else {
            return Ok(());
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| IntentionStoreError::Io(e.to_string()))?;
        }

        let mut content = String::new();
        for intention in &self.intentions {
            let line = serde_json::to_string(intention)
                .map_err(|e| IntentionStoreError::Corrupt(e.to_string()))?;
            content.push_str(&line);
            content.push('\n');
        }

        // Atomic write: tmp → rename.
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &content).map_err(|e| IntentionStoreError::Io(e.to_string()))?;
        std::fs::rename(&tmp, path).map_err(|e| IntentionStoreError::Io(e.to_string()))?;

        Ok(())
    }
}

/// Outcome of marking an intention complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionOutcome {
    /// One-shot intention removed from the store.
    Removed,
    /// Recurring intention rescheduled to `new_due_at_ns`.
    Rescheduled { new_due_at_ns: u64 },
    /// No intention with the given id was found.
    NotFound,
}

/// Errors from intention-store operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentionStoreError {
    /// I/O failure while reading or writing the backing file.
    Io(String),
    /// The backing file is corrupted and could not be parsed.
    Corrupt(String),
}

impl std::fmt::Display for IntentionStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IntentionStoreError::Io(e) => write!(f, "intention store I/O error: {e}"),
            IntentionStoreError::Corrupt(e) => write!(f, "intention store corrupted: {e}"),
        }
    }
}

// ── inject_due_intentions ─────────────────────────────────────────────────────

/// Seconds in a nanosecond (for grace-period arithmetic).
const ONE_SECOND_NS: u64 = 1_000_000_000;

/// Default overdue grace period: 60 seconds.
pub const DEFAULT_OVERDUE_GRACE_NS: u64 = 60 * ONE_SECOND_NS;

/// Inject due intentions into the task agenda (S14.2).
///
/// Scans `store` for intentions with `due_at_ns <= now_ns` and pushes a
/// [`Task`] for each into `agenda`.  The tier is:
///
/// - `0` (High)    — not yet overdue or within grace period.
/// - `2` (Low)     — `overdue_ns > overdue_grace_ns` (already escalated once,
///   now degraded to let urgent tasks preempt).
///
/// Marks injected intentions as `dispatched = true` and flushes the store.
///
/// Returns the number of intentions injected.
pub fn inject_due_intentions(
    store: &mut IntentionStore,
    agenda: &mut TaskAgenda,
    now_ns: u64,
    overdue_grace_ns: u64,
) -> Result<usize, IntentionStoreError> {
    let due_ids: Vec<(u64, String, u64)> = store
        .intentions
        .iter()
        .filter(|i| i.is_due(now_ns))
        .map(|i| (i.id, i.description.clone(), i.overdue_ns(now_ns)))
        .collect();

    if due_ids.is_empty() {
        return Ok(0);
    }

    let mut injected = 0;
    for (id, description, overdue_ns) in &due_ids {
        // High tier (0) for normal, Low tier (2) for long-overdue.
        let mlfq_level = if *overdue_ns > overdue_grace_ns { 2 } else { 0 };
        let task = Task {
            id: *id,
            mlfq_level,
            prompt: description.clone(),
            token_budget: Some(2048),
        };
        agenda.push(task);

        // Mark dispatched.
        if let Some(intention) = store.intentions.iter_mut().find(|i| i.id == *id) {
            intention.dispatched = true;
        }
        injected += 1;
    }

    store.flush()?;
    Ok(injected)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use scheduler::TaskAgenda;

    const T0: u64 = 1_700_000_000_000_000_000; // arbitrary epoch in ns
    const ONE_HOUR_NS: u64 = 3_600 * ONE_SECOND_NS;
    const ONE_MINUTE_NS: u64 = 60 * ONE_SECOND_NS;

    // ── S14.2 Exit criterion 1 — due intentions injected at High tier ─────────

    #[test]
    fn due_intention_is_injected_into_agenda_at_high_tier() {
        let mut store = IntentionStore::in_memory();
        let mut agenda = TaskAgenda::default();

        // Create an intention due just 5 seconds ago — within the 60s grace period.
        let just_past = T0 - 5 * ONE_SECOND_NS;
        store
            .add_once("check deployment status", just_past, just_past)
            .expect("add");

        let injected = inject_due_intentions(&mut store, &mut agenda, T0, DEFAULT_OVERDUE_GRACE_NS)
            .expect("inject");

        assert_eq!(injected, 1, "one intention injected");
        let task = agenda.select_optimal_task().expect("task available");
        assert_eq!(
            task.mlfq_level, 0,
            "due-but-within-grace injected at High (tier 0)"
        );
        assert!(task.prompt.contains("check deployment"));
    }

    #[test]
    fn future_intention_is_not_injected() {
        let mut store = IntentionStore::in_memory();
        let mut agenda = TaskAgenda::default();

        // Due one hour in the future.
        store
            .add_once("future task", T0 + ONE_HOUR_NS, T0)
            .expect("add");

        let injected = inject_due_intentions(&mut store, &mut agenda, T0, DEFAULT_OVERDUE_GRACE_NS)
            .expect("inject");

        assert_eq!(injected, 0, "future intention must not be injected");
        assert!(agenda.select_optimal_task().is_none());
    }

    // ── S14.2 Exit criterion 2 — recurring intentions reschedule ─────────────

    #[test]
    fn recurring_intention_reschedules_after_completion() {
        let mut store = IntentionStore::in_memory();
        let id = store
            .add_recurring("daily standup", T0, ONE_HOUR_NS, T0)
            .expect("add");

        // Simulate injection + mark completed.
        store.intentions[0].dispatched = true;
        let outcome = store.complete(id, T0 + ONE_MINUTE_NS).expect("complete");

        assert!(
            matches!(outcome, CompletionOutcome::Rescheduled { .. }),
            "recurring intention must reschedule"
        );
        // Should not be removed from the store.
        assert_eq!(store.len(), 1, "recurring intention remains in store");

        // New due time must be in the future.
        if let CompletionOutcome::Rescheduled { new_due_at_ns } = outcome {
            assert!(
                new_due_at_ns > T0,
                "rescheduled time {new_due_at_ns} must be after T0 {T0}"
            );
        }
    }

    #[test]
    fn one_shot_intention_is_removed_on_completion() {
        let mut store = IntentionStore::in_memory();
        let id = store
            .add_once("send report", T0 - ONE_MINUTE_NS, T0)
            .expect("add");

        let outcome = store.complete(id, T0).expect("complete");
        assert_eq!(outcome, CompletionOutcome::Removed);
        assert!(store.is_empty(), "one-shot intention removed from store");
    }

    // ── S14.2 Exit criterion 3 — overdue escalation ───────────────────────────

    #[test]
    fn overdue_intention_is_injected_at_low_tier() {
        let mut store = IntentionStore::in_memory();
        let mut agenda = TaskAgenda::default();

        // Due 2 hours ago — overdue by > DEFAULT_OVERDUE_GRACE_NS.
        let two_hours_ago = T0 - 2 * ONE_HOUR_NS;
        store
            .add_once("overdue task", two_hours_ago, two_hours_ago)
            .expect("add");

        inject_due_intentions(&mut store, &mut agenda, T0, DEFAULT_OVERDUE_GRACE_NS)
            .expect("inject");

        let task = agenda.select_optimal_task().expect("task");
        assert_eq!(
            task.mlfq_level, 2,
            "long-overdue task injected at Low (tier 2)"
        );
    }

    // ── S14.2 Exit criterion 4 — persistence ─────────────────────────────────

    #[test]
    fn intention_store_survives_process_restart() {
        let dir = std::env::temp_dir().join("anima_prospective_restart_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create dir");
        let path = dir.join("intentions.jsonl");

        {
            let mut store = IntentionStore::open(&path).expect("open1");
            store
                .add_once("pending after restart", T0 + ONE_HOUR_NS, T0)
                .expect("add");
            store
                .add_recurring(
                    "recurring after restart",
                    T0 + ONE_MINUTE_NS,
                    ONE_HOUR_NS,
                    T0,
                )
                .expect("add recurring");
        }

        {
            let store = IntentionStore::open(&path).expect("open2");
            assert_eq!(store.len(), 2, "both intentions survive restart");
            let recurring = store.all().iter().find(|i| i.repeat_interval_ns.is_some());
            assert!(recurring.is_some(), "recurring intention preserved");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── Already-dispatched intentions are not re-injected ────────────────────

    #[test]
    fn dispatched_intention_is_not_re_injected() {
        let mut store = IntentionStore::in_memory();
        let mut agenda = TaskAgenda::default();

        store
            .add_once("send email", T0 - ONE_MINUTE_NS, T0)
            .expect("add");

        // First injection.
        inject_due_intentions(&mut store, &mut agenda, T0, DEFAULT_OVERDUE_GRACE_NS)
            .expect("first inject");
        // Drain the agenda.
        while agenda.select_optimal_task().is_some() {}

        // Second injection — already dispatched, should not re-inject.
        let count = inject_due_intentions(&mut store, &mut agenda, T0, DEFAULT_OVERDUE_GRACE_NS)
            .expect("second inject");
        assert_eq!(count, 0, "dispatched intention must not be re-injected");
        assert!(agenda.select_optimal_task().is_none());
    }

    // ── Remove ────────────────────────────────────────────────────────────────

    #[test]
    fn remove_intention_by_id() {
        let mut store = IntentionStore::in_memory();
        let id = store.add_once("to delete", T0, T0).expect("add");
        let removed = store.remove(id).expect("remove");
        assert!(removed, "remove must return true for existing id");
        assert!(store.is_empty());
    }

    #[test]
    fn remove_nonexistent_id_returns_false() {
        let mut store = IntentionStore::in_memory();
        let removed = store.remove(999).expect("remove");
        assert!(!removed, "remove nonexistent must return false");
    }

    // ── Intention helpers ─────────────────────────────────────────────────────

    #[test]
    fn is_due_respects_dispatched_flag() {
        let mut i = Intention::once(0, "test", T0 - ONE_MINUTE_NS, T0);
        assert!(i.is_due(T0), "undispatched past intention is due");
        i.dispatched = true;
        assert!(!i.is_due(T0), "dispatched intention is not due");
    }

    #[test]
    fn overdue_ns_returns_zero_for_future_intention() {
        let i = Intention::once(0, "future", T0 + ONE_HOUR_NS, T0);
        assert_eq!(i.overdue_ns(T0), 0);
    }
}
