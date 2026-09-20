//! Durable conversation memory for the serving agent — E33 S33.1.
//!
//! Implements [`vita::ConversationMemory`] over the E22 [`SessionStore`], which
//! already provides exactly what a conversation needs: an ordered, durable,
//! searchable, exportable sequence of turns owned by a user.  Before this, that
//! store existed but nothing on the `serve` path wrote to it, so the agent
//! answered every operator message as though it were the first thing ever said.
//!
//! # What composition produces
//!
//! A plain prompt string:
//!
//! ```text
//! <identity framing>
//!
//! Conversation so far:
//! operator: what are you working on?
//! anima: …
//!
//! operator: and the second one?
//! anima:
//! ```
//!
//! A string rather than a chat-message array on purpose: it works with *every*
//! [`scheduler::LlmBackend`] — the mock, Ollama, the OpenAI-compatible
//! umbrella, Anthropic — with no change to the scheduler or to any provider.
//! Routing operator tasks through a tool-calling chat API is a later step and
//! does not require this seam to move.
//!
//! # Bounds
//!
//! History is trimmed from the oldest end to [`MAX_CONTEXT_CHARS`] so a long
//! conversation cannot grow the prompt without limit; the newest turns always
//! survive.  Characters rather than tokens because the trait has no backend
//! handle, and four-bytes-per-token is the same approximation
//! [`scheduler::LlmBackend::estimate_token_count`] makes by default.
//!
//! Storage failures are absorbed, as the trait requires: the agent's lifecycle
//! does not depend on its history being writable, and an unwritable disk must
//! not stop it answering.

use std::sync::{Arc, Mutex};

use sessions::{
    make_session_id, ConversationRole, ConversationTurn, SessionQuery, SessionRecord, SessionStore,
};
use vita::ConversationMemory;

/// Longest composed context, in characters (≈ 1 500 tokens).
///
/// Sized to leave room in an 8 K window for the identity framing, the current
/// message and the reply, on the smallest local model a tier might bind.
pub const MAX_CONTEXT_CHARS: usize = 6_000;

/// How the operator and the agent are labelled in the composed transcript.
const OPERATOR_LABEL: &str = "operator";
const AGENT_LABEL: &str = "anima";

/// Conversation memory backed by a shared [`SessionStore`].
pub struct SessionConversation {
    store: Arc<Mutex<SessionStore>>,
    session_id: String,
    /// System framing prepended to every composed prompt.
    identity: String,
    max_context_chars: usize,
}

impl SessionConversation {
    /// Open (or create) the active session for `user_id` and return memory over it.
    ///
    /// Reuses the most recent active session for that user so a restart
    /// continues the same conversation rather than starting a fresh one; the
    /// operator's history survives the process.  Falls back to an in-memory
    /// store when the file cannot be opened, so a read-only or corrupt state
    /// directory degrades to a forgetful agent rather than a dead one.
    pub fn open(agent_id: &str, user_id: &str, identity: impl Into<String>) -> Self {
        Self::open_at(
            SessionStore::default_path(agent_id),
            agent_id,
            user_id,
            identity,
        )
    }

    /// As [`SessionConversation::open`], against an explicit store path.
    pub fn open_at(
        path: impl Into<std::path::PathBuf>,
        agent_id: &str,
        user_id: &str,
        identity: impl Into<String>,
    ) -> Self {
        let path = path.into();
        let mut store = SessionStore::open(&path).unwrap_or_else(|e| {
            eprintln!(
                "anima-hosted: cannot open session store at {} ({e}); \
                 conversation history will not persist this run",
                path.display()
            );
            SessionStore::in_memory()
        });

        // `list` returns newest first, so the head is the session to continue.
        let existing = store
            .list(&SessionQuery::for_user(user_id).active_only())
            .first()
            .map(|s| s.id.clone());

        let session_id = match existing {
            Some(id) => id,
            None => {
                let nonce = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0);
                let id = make_session_id(nonce);
                let record = SessionRecord::new(&id, user_id, agent_id);
                if let Err(e) = store.insert(record) {
                    eprintln!("anima-hosted: cannot start a conversation session ({e})");
                }
                let _ = store.flush();
                id
            }
        };

        Self {
            store: Arc::new(Mutex::new(store)),
            session_id,
            identity: identity.into(),
            max_context_chars: MAX_CONTEXT_CHARS,
        }
    }

    /// The shared store, so the console can serve the same history it writes.
    pub fn store(&self) -> Arc<Mutex<SessionStore>> {
        Arc::clone(&self.store)
    }

    /// The session this memory appends to.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Override the context budget (mainly for tests).
    #[cfg(test)]
    pub fn with_context_chars(mut self, chars: usize) -> Self {
        self.max_context_chars = chars;
        self
    }

    /// Append one turn, absorbing any storage error.
    fn append(&self, role: ConversationRole, content: &str) {
        let Ok(mut store) = self.store.lock() else {
            return;
        };
        // The index is reassigned by `SessionRecord::append_turn`.
        let turn = ConversationTurn::new(0, role, content);
        // `SessionStore::append_turn` already flushes, and a flush rewrites
        // the whole store atomically — a second one here would double the
        // write amplification on what is now a per-turn hot path.
        if let Err(e) = store.append_turn(&self.session_id, turn) {
            eprintln!("anima-hosted: could not record a conversation turn ({e})");
        }
    }

    /// Render the tail of the conversation that fits the context budget.
    ///
    /// Walks backwards from the newest turn so the most recent exchange is
    /// always present, then emits in chronological order.
    fn transcript(&self) -> String {
        let Ok(store) = self.store.lock() else {
            return String::new();
        };
        let Some(session) = store.get(&self.session_id) else {
            return String::new();
        };

        let mut selected: Vec<&ConversationTurn> = Vec::new();
        let mut budget = self.max_context_chars;
        for turn in session.turns.iter().rev() {
            // +12 covers the label, separator and newline this turn will add.
            let cost = turn.content.len() + 12;
            if cost > budget && !selected.is_empty() {
                break;
            }
            budget = budget.saturating_sub(cost);
            selected.push(turn);
        }
        selected.reverse();

        let mut out = String::new();
        for turn in selected {
            let label = match turn.role {
                ConversationRole::User => OPERATOR_LABEL,
                ConversationRole::Assistant => AGENT_LABEL,
                ConversationRole::System => "system",
                ConversationRole::Tool => "tool",
            };
            out.push_str(label);
            out.push_str(": ");
            out.push_str(&turn.content);
            out.push('\n');
        }
        out
    }
}

impl ConversationMemory for SessionConversation {
    fn compose(&mut self, _task_id: u64, guidance: &str) -> String {
        // Record the human's turn first so it is part of the transcript, which
        // keeps history and prompt in agreement — what the operator sees in
        // `anima sessions show` is exactly what the model was given.
        self.append(ConversationRole::User, guidance);

        let transcript = self.transcript();
        let mut prompt = String::with_capacity(self.identity.len() + transcript.len() + 64);
        if !self.identity.is_empty() {
            prompt.push_str(&self.identity);
            prompt.push_str("\n\n");
        }
        prompt.push_str("Conversation so far:\n");
        prompt.push_str(&transcript);
        prompt.push('\n');
        prompt.push_str(AGENT_LABEL);
        prompt.push(':');
        prompt
    }

    fn record_reply(&mut self, _task_id: u64, response: &str) {
        self.append(ConversationRole::Assistant, response);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory(dir: &std::path::Path) -> SessionConversation {
        // An explicit path rather than the HOME-derived default: these tests
        // run in parallel threads of one process, and mutating HOME would race.
        SessionConversation::open_at(
            dir.join("sessions.json"),
            "test-agent",
            "user:test",
            "You are a test agent.",
        )
    }

    #[test]
    fn the_second_message_carries_the_first_exchange() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = memory(dir.path());

        let first = m.compose(1, "what are you working on?");
        assert_eq!(
            first.matches("operator:").count(),
            1,
            "the first prompt should hold exactly one operator turn: {first}"
        );
        m.record_reply(1, "reading the overnight logs");

        let second = m.compose(2, "and the second one?");
        assert!(
            second.contains("what are you working on?"),
            "prior question missing: {second}"
        );
        assert!(
            second.contains("reading the overnight logs"),
            "prior answer missing: {second}"
        );
        assert!(
            second.contains("and the second one?"),
            "current question missing: {second}"
        );
        assert!(second.contains("You are a test agent."), "framing missing");
        assert!(second.trim_end().ends_with("anima:"), "no reply cue");
    }

    #[test]
    fn history_is_trimmed_from_the_oldest_end_and_keeps_the_newest_turn() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = memory(dir.path()).with_context_chars(200);

        for i in 0..40 {
            m.compose(i, &format!("message number {i} {}", "x".repeat(40)));
            m.record_reply(i, &format!("reply number {i}"));
        }
        let prompt = m.compose(99, "the newest question");

        assert!(
            prompt.contains("the newest question"),
            "newest turn was trimmed away: {prompt}"
        );
        assert!(
            !prompt.contains("message number 0 "),
            "oldest turn survived trimming: {prompt}"
        );
        // Budget is advisory (the newest turn always survives), so allow the
        // framing and one oversized turn on top of it.
        assert!(
            prompt.len() < 200 + 400,
            "prompt unbounded at {}",
            prompt.len()
        );
    }

    #[test]
    fn turns_persist_across_a_reopen_so_a_restart_continues_the_conversation() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut m = memory(dir.path());
            m.compose(1, "remember this");
            m.record_reply(1, "remembered");
        }
        let mut reopened = memory(dir.path());
        let prompt = reopened.compose(2, "do you?");
        assert!(
            prompt.contains("remember this") && prompt.contains("remembered"),
            "history lost across reopen: {prompt}"
        );
    }

    #[test]
    fn what_the_model_sees_is_what_the_session_store_holds() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = memory(dir.path());
        m.compose(1, "one");
        m.record_reply(1, "two");

        let store = m.store();
        let guard = store.lock().unwrap();
        let session = guard.get(m.session_id()).expect("session exists");
        let contents: Vec<&str> = session.turns.iter().map(|t| t.content.as_str()).collect();
        assert_eq!(contents, vec!["one", "two"]);
    }
}
