#![forbid(unsafe_code)]

//! Synaptic memory layer implementing the CLS three-tier hierarchy.

pub mod archival;
pub mod decay;
pub mod l2_cache;
pub mod pressure;
pub mod pruning;
pub mod replay;

pub use archival::{
    archive_memory_node, embed_memory_node, retrieve_top_k_from_l3_for_l2, ArchivalEntry,
    ArchivalStore, ArchivalStoreError, ArchivedItem, DemotionOutcome, L3Archive, L3ArchiveError,
    Provenance, SourceTier,
};
pub use decay::{EmotionalContext, MemoryNode};
pub use l2_cache::ArcCache;
pub use pressure::MemoryPressureEvent;
pub use pruning::{prune_l2_cache, L1PruningStore, PruningReport};
pub use replay::{run_replay_validation, ReplayConfig, ReplayReport};

/// Default block size in tokens, matching PagedAttention page granularity.
pub const DEFAULT_BLOCK_SIZE: u32 = 16;

/// L1 live attention window modelled as block-structured token tracking.
///
/// Context is divided into fixed-size blocks.  Occupancy is reported at block
/// granularity and compared against a configurable high-water mark to produce
/// [`MemoryPressureEvent`]s that the scheduler and sleep state machine consume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualContextManager {
    /// Number of tokens currently in the L1 window.
    l1_token_count: u32,
    /// Hard upper bound on the context window (tokens).
    max_context: u32,
    /// Granularity of the block table: tokens per block.
    block_size: u32,
    /// Pressure fires when `occupied_blocks() >= high_water_blocks`.
    high_water_blocks: u32,
}

impl VirtualContextManager {
    /// Creates a new manager with a known L1 token count and unlimited context.
    pub fn new(l1_token_count: u32) -> Self {
        Self::with_capacity(l1_token_count, u32::MAX)
    }

    /// Creates a manager with an explicit context cap and the default block size.
    pub fn with_capacity(l1_token_count: u32, max_context: u32) -> Self {
        Self::with_blocks(l1_token_count, max_context, DEFAULT_BLOCK_SIZE)
    }

    /// Full constructor: explicit token count, context cap, and block size.
    ///
    /// The high-water mark is set to 75 % of total block capacity.
    pub fn with_blocks(l1_token_count: u32, max_context: u32, block_size: u32) -> Self {
        let block_size = block_size.max(1);
        let max_context = max_context.max(block_size); // at least one block
        let total_blocks = max_context / block_size;
        let high_water_blocks = (total_blocks.saturating_mul(3) / 4).max(1);
        Self {
            l1_token_count: l1_token_count.min(max_context),
            max_context,
            block_size,
            high_water_blocks,
        }
    }

    /// Overrides the high-water mark (in blocks).
    pub fn set_high_water_blocks(&mut self, blocks: u32) {
        self.high_water_blocks = blocks;
    }

    // ── Accessors ────────────────────────────────────────────────────────────

    /// Returns active L1 token count.
    pub fn get_l1_token_count(&self) -> u32 {
        self.l1_token_count
    }

    /// Updates active L1 token count, saturating at `max_context`.
    pub fn set_l1_token_count(&mut self, l1_token_count: u32) {
        self.l1_token_count = l1_token_count.min(self.max_context);
    }

    /// Adds `tokens` to the active count, saturating at `max_context`.
    pub fn add_tokens(&mut self, tokens: u32) {
        self.l1_token_count = self
            .l1_token_count
            .saturating_add(tokens)
            .min(self.max_context);
    }

    /// Returns the configured maximum context window (tokens).
    pub fn max_context(&self) -> u32 {
        self.max_context
    }

    /// Returns the block size used for occupancy tracking.
    pub fn block_size(&self) -> u32 {
        self.block_size
    }

    /// Total blocks available in the window.
    pub fn total_blocks(&self) -> u32 {
        self.max_context / self.block_size
    }

    /// Returns the high-water mark in blocks.
    pub fn high_water_blocks(&self) -> u32 {
        self.high_water_blocks
    }

    // ── Block-table ───────────────────────────────────────────────────────────

    /// Occupied blocks (ceiling division of token count by block size).
    ///
    /// Every partial block is counted as a full block, matching
    /// PagedAttention semantics: a block is allocated in full when the first
    /// token of that block is written.
    pub fn occupied_blocks(&self) -> u32 {
        if self.l1_token_count == 0 {
            return 0;
        }
        self.l1_token_count.div_ceil(self.block_size)
    }

    /// Free (unoccupied) blocks remaining in the window.
    pub fn free_blocks(&self) -> u32 {
        self.total_blocks().saturating_sub(self.occupied_blocks())
    }

    // ── Pressure ─────────────────────────────────────────────────────────────

    /// Evaluates the current memory-pressure level.
    ///
    /// - [`MemoryPressureEvent::Critical`] — window is completely full.
    /// - [`MemoryPressureEvent::HighWater`] — at or above the configured mark.
    /// - [`MemoryPressureEvent::Normal`] — below the high-water mark.
    pub fn check_pressure(&self) -> MemoryPressureEvent {
        let blocks = self.occupied_blocks();
        if blocks >= self.total_blocks() {
            MemoryPressureEvent::Critical
        } else if blocks >= self.high_water_blocks {
            MemoryPressureEvent::HighWater
        } else {
            MemoryPressureEvent::Normal
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn occupied_blocks_is_ceiling_of_token_count_over_block_size() {
        let ctx = VirtualContextManager::with_blocks(17, 1000, 16);
        // 17 tokens / 16 per block = ceiling(17/16) = 2 blocks
        assert_eq!(ctx.occupied_blocks(), 2);
    }

    #[test]
    fn occupied_blocks_exact_boundary_is_not_overcounted() {
        let ctx = VirtualContextManager::with_blocks(32, 1000, 16);
        // 32 tokens / 16 per block = exactly 2 blocks
        assert_eq!(ctx.occupied_blocks(), 2);
    }

    #[test]
    fn occupied_blocks_zero_tokens_returns_zero() {
        let ctx = VirtualContextManager::with_blocks(0, 1000, 16);
        assert_eq!(ctx.occupied_blocks(), 0);
    }

    #[test]
    fn free_blocks_matches_total_minus_occupied() {
        let ctx = VirtualContextManager::with_blocks(32, 160, 16);
        // 160/16 = 10 total, 32/16 = 2 occupied → 8 free
        assert_eq!(ctx.total_blocks(), 10);
        assert_eq!(ctx.occupied_blocks(), 2);
        assert_eq!(ctx.free_blocks(), 8);
    }

    #[test]
    fn check_pressure_normal_below_high_water() {
        // 160-token window, block_size=16 → 10 blocks total, HWM = 7 (75%)
        let ctx = VirtualContextManager::with_blocks(0, 160, 16);
        assert_eq!(ctx.high_water_blocks(), 7);
        // 6 blocks occupied (96 tokens)
        let mut ctx2 = ctx;
        ctx2.set_l1_token_count(96);
        assert_eq!(ctx2.occupied_blocks(), 6);
        assert_eq!(ctx2.check_pressure(), MemoryPressureEvent::Normal);
    }

    #[test]
    fn check_pressure_fires_high_water_at_mark() {
        // 160 tokens, block_size=16 → 10 blocks, HWM = 7
        let mut ctx = VirtualContextManager::with_blocks(0, 160, 16);
        // 7 blocks = 112 tokens → exactly at HWM
        ctx.set_l1_token_count(112);
        assert_eq!(ctx.occupied_blocks(), 7);
        assert_eq!(ctx.check_pressure(), MemoryPressureEvent::HighWater);
    }

    #[test]
    fn check_pressure_critical_when_full() {
        let mut ctx = VirtualContextManager::with_blocks(0, 160, 16);
        ctx.set_l1_token_count(160);
        assert_eq!(ctx.check_pressure(), MemoryPressureEvent::Critical);
    }

    #[test]
    fn l1_occupancy_within_one_block_of_ground_truth() {
        // Adding 25 tokens to a 16-token-block manager should report 2 blocks
        // (not 3), confirming ceiling accuracy is within ±1 block.
        let ctx = VirtualContextManager::with_blocks(25, 1000, 16);
        let reported = ctx.occupied_blocks();
        let exact = (25f64 / 16f64).ceil() as u32;
        assert_eq!(reported, exact, "occupancy should match ceiling exactly");
        // Also check that |reported - exact| <= 1
        assert!((reported as i64 - exact as i64).unsigned_abs() <= 1);
    }

    #[test]
    fn set_high_water_blocks_overrides_default() {
        let mut ctx = VirtualContextManager::with_blocks(0, 160, 16);
        ctx.set_high_water_blocks(3);
        ctx.set_l1_token_count(48); // 3 blocks
        assert_eq!(ctx.check_pressure(), MemoryPressureEvent::HighWater);
    }

    #[test]
    fn add_tokens_saturates_at_max_context() {
        let mut ctx = VirtualContextManager::with_capacity(990, 1000);
        ctx.add_tokens(100);
        assert_eq!(ctx.get_l1_token_count(), 1000);
    }
}
