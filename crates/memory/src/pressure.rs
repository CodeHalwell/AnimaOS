//! Memory-pressure events emitted by the L1 context window.
//!
//! The [`VirtualContextManager`] computes occupancy at block granularity and
//! fires one of three pressure levels.  Downstream consumers (the scheduler,
//! the sleep state machine) subscribe to these events and reduce token budgets
//! or trigger a sleep transition accordingly.

use crate::VirtualContextManager;
use scheduler::BoundedTokenPipe;

/// Pressure level reported by the L1 context window.
///
/// Ordering is monotone: `Normal < HighWater < Critical`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MemoryPressureEvent {
    /// Occupied blocks are below the configured high-water mark.
    Normal,
    /// Occupied blocks have reached or exceeded the high-water mark.
    HighWater,
    /// The context window is completely full (no free blocks remain).
    Critical,
}

impl MemoryPressureEvent {
    /// Returns `true` when pressure requires immediate scheduler action.
    pub fn is_elevated(&self) -> bool {
        matches!(self, Self::HighWater | Self::Critical)
    }
}

/// Emits a pressure event into `pipe` by consuming credits proportional to
/// the current occupancy level.
///
/// - `Normal`: no credits consumed.
/// - `HighWater`: consumes `pipe.capacity() / 4` credits (soft backpressure).
/// - `Critical`: consumes all remaining credits (hard backpressure).
///
/// Credits that cannot be consumed (e.g., pipe already exhausted) are silently
/// ignored — this is advisory, not a hard stop.
pub fn emit_to_pipe(ctx: &VirtualContextManager, pipe: &mut BoundedTokenPipe) {
    match ctx.check_pressure() {
        MemoryPressureEvent::Normal => {}
        MemoryPressureEvent::HighWater => {
            let n = (pipe.capacity() / 4).max(1);
            let _ = pipe.push(n.min(pipe.available_credits()));
        }
        MemoryPressureEvent::Critical => {
            let _ = pipe.push(pipe.available_credits());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VirtualContextManager;
    use scheduler::BoundedTokenPipe;

    #[test]
    fn normal_pressure_emits_nothing_to_pipe() {
        // 4 tokens in a 1000-token window, 16-token blocks → 1 block used, far below 75% HWM
        let ctx = VirtualContextManager::with_blocks(4, 1000, 16);
        let mut pipe = BoundedTokenPipe::new(100);
        emit_to_pipe(&ctx, &mut pipe);
        assert_eq!(pipe.available_credits(), 100);
    }

    #[test]
    fn high_water_pressure_consumes_quarter_credits() {
        // 800 tokens in 1000-token window, block_size=16 → 50 blocks, total=62 blocks
        // high_water = 62 * 3/4 = 46 blocks → 50 >= 46 → HighWater
        let ctx = VirtualContextManager::with_blocks(800, 1000, 16);
        assert_eq!(ctx.check_pressure(), MemoryPressureEvent::HighWater);
        let mut pipe = BoundedTokenPipe::new(100);
        emit_to_pipe(&ctx, &mut pipe);
        assert_eq!(pipe.available_credits(), 75); // consumed 25 = 100/4
    }

    #[test]
    fn critical_pressure_consumes_all_credits() {
        // 1000 tokens = 1000-token window → fully occupied → Critical
        let ctx = VirtualContextManager::with_blocks(1000, 1000, 16);
        assert_eq!(ctx.check_pressure(), MemoryPressureEvent::Critical);
        let mut pipe = BoundedTokenPipe::new(100);
        emit_to_pipe(&ctx, &mut pipe);
        assert_eq!(pipe.available_credits(), 0);
    }

    #[test]
    fn pressure_ordering_is_monotone() {
        assert!(MemoryPressureEvent::Normal < MemoryPressureEvent::HighWater);
        assert!(MemoryPressureEvent::HighWater < MemoryPressureEvent::Critical);
    }

    #[test]
    fn is_elevated_reflects_action_requirement() {
        assert!(!MemoryPressureEvent::Normal.is_elevated());
        assert!(MemoryPressureEvent::HighWater.is_elevated());
        assert!(MemoryPressureEvent::Critical.is_elevated());
    }
}
