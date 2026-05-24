#![forbid(unsafe_code)]

/// Minimal L1 working context manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualContextManager {
    l1_token_count: u32,
}

impl VirtualContextManager {
    /// Creates a new manager with a known L1 token count.
    pub fn new(l1_token_count: u32) -> Self {
        Self { l1_token_count }
    }

    /// Returns active L1 token count.
    pub fn get_l1_token_count(&self) -> u32 {
        self.l1_token_count
    }

    /// Updates active L1 token count.
    pub fn set_l1_token_count(&mut self, l1_token_count: u32) {
        self.l1_token_count = l1_token_count;
    }
}
