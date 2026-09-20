//! Conversation memory for operator-originated tasks — E33 S33.1.
//!
//! # Why this exists
//!
//! The somatic loop turns a sensory packet into a task whose prompt *is* the
//! packet's text.  For a sensor reading that is exactly right.  For a human
//! talking to the agent it is not: the model never sees what was said a moment
//! ago, so "and the second one?" cannot work, and the agent's own identity
//! never frames the exchange.  The console looks like a chat window while the
//! thing behind it answers every line as though it were the first.
//!
//! [`ConversationMemory`] is the seam that fixes it without disturbing
//! anything else.  The loop calls it at the two points it already touches:
//!
//! | Point | Call | Effect |
//! |---|---|---|
//! | dispatch | [`ConversationMemory::compose`] | wrap the human's text in the context the model needs |
//! | completion | [`ConversationMemory::record_reply`] | persist the answer as the agent's turn |
//!
//! # What it deliberately does not do
//!
//! - **It does not change the audit trail.**  `TaskStarted` keeps recording the
//!   human's words, not the composed prompt, so the log and every console
//!   built on it stay readable instead of repeating the whole context on each
//!   turn.
//! - **It does not bypass policy.**  Composition happens *after*
//!   `senses::SensoryBridge` has applied the operator's policy bounds: the
//!   bounds govern what a human may say, not what the agent may recall.
//! - **It does not exist by default.**  With no memory installed the loop
//!   behaves exactly as it did before, which keeps the bare-metal target and
//!   every existing test on the original path.
//!
//! # Why a trait
//!
//! The implementation needs durable storage (`sessions::SessionStore`) and the
//! agent's identity document, both of which live above `vita` in the
//! dependency graph.  Keeping the contract here and the implementation in the
//! hosted kernel preserves that direction — the same reason the console tails
//! the audit log rather than reaching into the lifecycle.
//!
//! Because [`ConversationMemory::compose`] returns a plain prompt string, it
//! works with every existing [`scheduler::LlmBackend`] — mock, Ollama,
//! OpenAI-compatible, Anthropic — with no change to the scheduler or to any
//! provider.  Routing operator tasks through a chat-message API with tool use
//! is a later step; the seam does not need to move for it.

/// Supplies the conversational context around an operator-originated task.
///
/// Implementations are shared behind `Arc<Mutex<…>>` (see
/// [`crate::Subsystems::conversation`]), so `compose` and `record_reply` are
/// called under a lock held only for the duration of the call.
pub trait ConversationMemory: Send {
    /// Wrap freshly-dispatched guidance in whatever context the model needs —
    /// identity framing, recent turns, anything else the implementation keeps.
    ///
    /// `guidance` is the human's text exactly as it passed policy validation.
    /// The return value is the prompt actually sent to the backend; returning
    /// `guidance` unchanged is a valid no-op implementation.
    ///
    /// Called once per dispatch, so the context reflects everything known at
    /// the moment the task actually runs rather than when it was queued.
    fn compose(&mut self, task_id: u64, guidance: &str) -> String;

    /// Record the agent's reply to `task_id` as its turn in the conversation.
    ///
    /// Called after the backend completes.  A failure to persist must not
    /// propagate: the agent's lifecycle does not depend on its conversation
    /// history, so implementations absorb storage errors.
    fn record_reply(&mut self, task_id: u64, response: &str);

    /// Record something the agent said that was not a reply to a task — most
    /// importantly a question it is asking the operator (E33 S33.3).
    ///
    /// The default treats it as a reply, which is the right behaviour whenever
    /// the implementation stores both as the agent's own turns: the answer the
    /// human eventually sends then arrives with the question already in
    /// context, and no separate question-tracking state is needed.
    fn record_question(&mut self, task_id: u64, question: &str) {
        self.record_reply(task_id, question);
    }
}
