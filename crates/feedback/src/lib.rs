#![forbid(unsafe_code)]

//! Response quality tracking and user feedback collection — Epic E24.
//!
//! # Scope
//!
//! AnimaOS invokes the cortex on behalf of users and records the outcome in the
//! audit trail.  E24 closes the feedback loop: users and operators can attach
//! explicit quality signals (thumbs-up/down, star ratings, corrections) to
//! completed invocations so the agent can learn from its mistakes and improve
//! over time.
//!
//! # Architecture
//!
//! ```text
//!  User / Operator
//!       │
//!       ▼  anima feedback record <invocation_id> <user_id> <up|down|stars:N>
//!  FeedbackStore ──► FeedbackRecord { rating, categories, correction? }
//!       │
//!       ▼  anima feedback analyze
//!  QualityReport { satisfaction_rate, avg_stars, category_counts, … }
//!       │
//!       ▼  build_training_hints()
//!  Vec<WeightedTrainingHint>  ──► E8 fine-tuning pipeline
//! ```
//!
//! # Modules
//!
//! | Module | Contents |
//! |---|---|
//! | [`record`] | [`record::FeedbackRecord`], [`record::FeedbackRating`], [`record::FeedbackCategory`] |
//! | [`store`] | [`store::FeedbackStore`], [`store::StoreError`] |
//! | [`report`] | [`report::QualityReport`], [`report::WeightedTrainingHint`], [`report::build_training_hints`] |

pub mod record;
pub mod report;
pub mod store;

// Re-export the most commonly used types.
pub use record::{FeedbackCategory, FeedbackRating, FeedbackRecord};
pub use report::{build_training_hints, QualityReport, WeightedTrainingHint};
pub use store::{FeedbackStore, StoreError};
