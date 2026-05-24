#![forbid(unsafe_code)]

//! Afferent sensory input vector: parses text + PCM streams into typed packets.

use std::sync::Mutex;

/// External policy bounds from `/dev/sensors/human`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HumanGuidance {
    /// Free-form policy directives provided by the operator.
    pub policy_hint: String,
}

/// Errors raised when sensory input cannot be consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SensoryBridgeError {
    /// Input stream did not contain valid guidance.
    InvalidInput,
}

/// Streamed sensory packet emitted by the bridge.
#[derive(Debug, Clone, PartialEq)]
pub enum SensoryPacket {
    /// A discrete text-buffer payload.
    Text(String),
    /// PCM audio frame (16-bit little-endian samples).
    Pcm(Vec<i16>),
}

/// Minimal sensory bridge for human intent signals.
#[derive(Debug)]
pub struct SensoryBridge {
    active_bounds: Mutex<HumanGuidance>,
    queue: Mutex<Vec<SensoryPacket>>,
}

impl SensoryBridge {
    /// Creates a new sensory bridge with initial human guidance.
    pub fn new(active_bounds: HumanGuidance) -> Self {
        Self {
            active_bounds: Mutex::new(active_bounds),
            queue: Mutex::new(Vec::new()),
        }
    }

    /// Reads current human policy bounds.
    pub async fn read_active_bounds(&self) -> Result<HumanGuidance, SensoryBridgeError> {
        Ok(self.active_bounds.lock().expect("poisoned").clone())
    }

    /// Updates active policy bounds.
    pub fn set_active_bounds(&self, guidance: HumanGuidance) {
        *self.active_bounds.lock().expect("poisoned") = guidance;
    }

    /// Packetizes a text buffer and enqueues it.
    pub fn packetize_text(&self, text: impl Into<String>) {
        self.queue
            .lock()
            .expect("poisoned")
            .push(SensoryPacket::Text(text.into()));
    }

    /// Packetizes a PCM frame and enqueues it.
    pub fn packetize_pcm(&self, samples: Vec<i16>) {
        self.queue
            .lock()
            .expect("poisoned")
            .push(SensoryPacket::Pcm(samples));
    }

    /// Pops the next sensory packet, if any.
    pub fn next_packet(&self) -> Option<SensoryPacket> {
        let mut q = self.queue.lock().expect("poisoned");
        if q.is_empty() {
            None
        } else {
            Some(q.remove(0))
        }
    }
}

impl Clone for SensoryBridge {
    fn clone(&self) -> Self {
        let bounds = self.active_bounds.lock().expect("poisoned").clone();
        let queue = self.queue.lock().expect("poisoned").clone();
        Self {
            active_bounds: Mutex::new(bounds),
            queue: Mutex::new(queue),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        use std::pin::Pin;
        use std::task::{Context, Poll, Waker};
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut future = Box::pin(future);
        loop {
            if let Poll::Ready(v) = Pin::as_mut(&mut future).poll(&mut cx) {
                return v;
            }
        }
    }

    #[test]
    fn read_active_bounds_returns_current_guidance() {
        let bridge = SensoryBridge::new(HumanGuidance {
            policy_hint: "low-cost".to_string(),
        });
        let g = block_on(bridge.read_active_bounds()).unwrap();
        assert_eq!(g.policy_hint, "low-cost");
    }

    #[test]
    fn packetize_and_pop_round_trip() {
        let bridge = SensoryBridge::new(HumanGuidance {
            policy_hint: "x".to_string(),
        });
        bridge.packetize_text("hello");
        bridge.packetize_pcm(vec![1, 2, 3]);
        assert!(matches!(bridge.next_packet(), Some(SensoryPacket::Text(_))));
        assert!(matches!(bridge.next_packet(), Some(SensoryPacket::Pcm(_))));
        assert!(bridge.next_packet().is_none());
    }
}
