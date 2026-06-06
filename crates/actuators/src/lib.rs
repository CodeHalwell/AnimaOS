#![forbid(unsafe_code)]

//! Efferent world-interaction layer — E7 Embodiment.
//!
//! # Structure
//!
//! - [`egress`]     — [`EgressGuard`]: URL/scheme validation, SSRF protection,
//!   host allow/deny lists.
//! - [`scorer`]     — [`ToolScorer`] trait + [`LexicalScorer`] (BM25-style) +
//!   [`FixtureScorer`] for hermetic tests.
//! - [`web_search`] — [`WebSearchTool`]: `ToolDriver` implementation over a
//!   [`SearchProvider`] abstraction (fixture and SearXNG impls).
//! - [`browser`]    — [`BrowserNavigateTool`]/[`BrowserReadTextTool`]/
//!   [`BrowserExtractTool`]: `ToolDriver`s over a [`BrowserDriver`] abstraction
//!   ([`MockBrowserDriver`] for CI; `PlaywrightDriver` behind `feature = "live"`).
//!
//! All modules are `std`-only. The crate is not added to any `no_std` target.
//!
//! # CI hermeticity
//!
//! Every live path (real HTTP, Playwright) is opt-in via env vars or feature
//! flags and is `#[ignore]`d in the default test suite. The default test suite
//! runs offline against deterministic fixtures.

pub mod browser;
pub mod egress;
pub mod scorer;
pub mod web_search;

#[cfg(feature = "live")]
pub use browser::PlaywrightDriver;
pub use browser::{
    BrowserDriver, BrowserExtractTool, BrowserNavigateTool, BrowserReadTextTool, MockBrowserDriver,
    MockPage, PageState,
};
pub use egress::{EgressDenialReason, EgressGuard, EgressVerdict};
pub use scorer::{FixtureScorer, LexicalScorer, ToolScorer};
pub use web_search::{
    FixtureProvider, SearchProvider, SearchResult, SearxngProvider, WebSearchTool,
};
