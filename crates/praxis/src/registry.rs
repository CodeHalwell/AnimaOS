//! Tool driver registry — `/dev/anima/praxis/tools/` namespace.
//!
//! The [`ToolRegistry`] maintains a filesystem-style directory of [`ToolDriver`]
//! implementations keyed by their stable `id()`.  Each entry is wrapped by a
//! dedicated [`CircuitBreaker`] so that persistent tool failures automatically
//! trip the breaker without affecting other tools in the registry.
//!
//! # Built-in tools
//!
//! The registry ships with two built-in tools that can be disabled via the
//! `exclude_builtins` constructor:
//!
//! | Tool id     | Description                              |
//! |-------------|------------------------------------------|
//! | `clock`     | Returns the current Unix timestamp (ms)  |
//! | `echo`      | Echoes the payload back unchanged        |

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{CircuitBreaker, ToolDriver, ToolEnvelope, ToolInvocationError};

// ── Built-in tool implementations ────────────────────────────────────────────

/// Returns the current Unix epoch in milliseconds as a little-endian u64.
#[derive(Debug)]
pub struct ClockTool;

impl ToolDriver for ClockTool {
    fn id(&self) -> &'static str {
        "clock"
    }

    fn schema(&self) -> &'static str {
        r#"{"type":"object","description":"Returns current Unix timestamp (ms) as little-endian u64 bytes.","properties":{}}"#
    }

    fn invoke(&self, _payload: &[u8]) -> Result<Vec<u8>, ToolInvocationError> {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_millis() as u64;
        Ok(ms.to_le_bytes().to_vec())
    }
}

/// Echoes the raw payload back to the caller unchanged.
#[derive(Debug)]
pub struct EchoTool;

impl ToolDriver for EchoTool {
    fn id(&self) -> &'static str {
        "echo"
    }

    fn schema(&self) -> &'static str {
        r#"{"type":"object","description":"Echoes the payload back unchanged.","properties":{"payload":{"type":"string"}}}"#
    }

    fn invoke(&self, payload: &[u8]) -> Result<Vec<u8>, ToolInvocationError> {
        Ok(payload.to_vec())
    }
}

/// A simple text I/O tool that converts payload bytes to UTF-8 and appends a
/// newline.  Errors on non-UTF-8 input.
#[derive(Debug)]
pub struct TextIoTool;

impl ToolDriver for TextIoTool {
    fn id(&self) -> &'static str {
        "text-io"
    }

    fn schema(&self) -> &'static str {
        r#"{"type":"object","description":"Converts bytes to UTF-8 string and appends newline.","properties":{"text":{"type":"string"}}}"#
    }

    fn invoke(&self, payload: &[u8]) -> Result<Vec<u8>, ToolInvocationError> {
        let text = std::str::from_utf8(payload).map_err(|_| ToolInvocationError::InvalidPayload)?;
        let mut out = text.to_string();
        out.push('\n');
        Ok(out.into_bytes())
    }
}

// ── Registry ─────────────────────────────────────────────────────────────────

/// Entry stored per registered tool.
struct RegistryEntry {
    driver: Arc<dyn ToolDriver>,
    breaker: CircuitBreaker,
}

/// Failure threshold: trip the circuit after this many consecutive errors.
const DEFAULT_OPEN_THRESHOLD: u32 = 5;

/// Thread-safe tool driver registry.
///
/// Registration, lookup, and dispatch are all thread-safe; the registry can be
/// cloned (`Clone` shares the same underlying table via `Arc`).
pub struct ToolRegistry {
    entries: Arc<Mutex<HashMap<String, RegistryEntry>>>,
}

impl Clone for ToolRegistry {
    fn clone(&self) -> Self {
        Self {
            entries: Arc::clone(&self.entries),
        }
    }
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let entries = self.entries.lock().unwrap();
        f.debug_struct("ToolRegistry")
            .field("registered", &entries.len())
            .finish()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    /// Creates a new registry pre-populated with the three built-in tools
    /// (`clock`, `echo`, `text-io`).
    pub fn new() -> Self {
        let registry = Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
        };
        registry.register(ClockTool);
        registry.register(EchoTool);
        registry.register(TextIoTool);
        registry
    }

    /// Creates an empty registry with no pre-registered tools.
    pub fn empty() -> Self {
        Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    // ── Registration ─────────────────────────────────────────────────────────

    /// Registers a tool driver.
    ///
    /// If a tool with the same `id()` already exists it is replaced.
    pub fn register(&self, driver: impl ToolDriver + 'static) {
        let id = driver.id().to_string();
        let entry = RegistryEntry {
            driver: Arc::new(driver),
            breaker: CircuitBreaker::new(),
        };
        self.entries.lock().unwrap().insert(id, entry);
    }

    /// Registers a pre-built `Arc<dyn ToolDriver>`.
    pub fn register_arc(&self, driver: Arc<dyn ToolDriver>) {
        let id = driver.id().to_string();
        let entry = RegistryEntry {
            driver,
            breaker: CircuitBreaker::new(),
        };
        self.entries.lock().unwrap().insert(id, entry);
    }

    // ── Discovery ─────────────────────────────────────────────────────────────

    /// Returns a clone of the driver for `id`, or `None` if unregistered.
    pub fn lookup(&self, id: &str) -> Option<Arc<dyn ToolDriver>> {
        self.entries
            .lock()
            .unwrap()
            .get(id)
            .map(|e| Arc::clone(&e.driver))
    }

    /// Returns a sorted list of all registered tool identifiers.
    pub fn list(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.entries.lock().unwrap().keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Returns the number of registered tools.
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }

    /// Returns true when no tools are registered.
    pub fn is_empty(&self) -> bool {
        self.entries.lock().unwrap().is_empty()
    }

    // ── Dispatch ──────────────────────────────────────────────────────────────

    /// Dispatches a [`ToolEnvelope`] through the registry.
    ///
    /// Performs breaker health check, invokes the tool, and records the
    /// outcome in the breaker so persistent failures trip it.
    ///
    /// # Errors
    ///
    /// - [`ToolInvocationError::BreakerOpen`] — breaker is open (tool circuit tripped).
    /// - [`ToolInvocationError::InvalidPayload`] — tool rejected the payload.
    /// - [`ToolInvocationError::ExecutionFailed`] — tool failed internally.
    /// - A synthetic `ExecutionFailed("unknown tool: …")` when the tool is not registered.
    pub fn dispatch(&self, envelope: &ToolEnvelope) -> Result<Vec<u8>, ToolInvocationError> {
        let mut entries = self.entries.lock().unwrap();
        let entry = entries.get_mut(&envelope.tool_id).ok_or_else(|| {
            ToolInvocationError::ExecutionFailed(format!("unknown tool: {}", envelope.tool_id))
        })?;

        // Check circuit breaker health.
        entry.breaker.verify_pathway_health()?;

        // Invoke the tool.
        let result = entry.driver.invoke(&envelope.payload);
        match &result {
            Ok(_) => entry.breaker.record_success(),
            Err(ToolInvocationError::BreakerOpen) => {} // already open
            Err(_) => entry.breaker.record_failure(DEFAULT_OPEN_THRESHOLD),
        }
        result
    }

    /// Returns the current [`BreakerState`] for a tool, or `None` if unregistered.
    pub fn breaker_state(&self, id: &str) -> Option<crate::BreakerState> {
        self.entries
            .lock()
            .unwrap()
            .get(id)
            .map(|e| e.breaker.state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{Bus, ToolEnvelope};

    fn envelope(tool_id: &str, payload: &[u8]) -> ToolEnvelope {
        ToolEnvelope::new(Bus::Mcp, tool_id, payload.to_vec(), 0)
    }

    // ── Built-in tools ────────────────────────────────────────────────────────

    #[test]
    fn clock_tool_returns_eight_bytes() {
        let t = ClockTool;
        let out = t.invoke(b"").unwrap();
        assert_eq!(out.len(), 8, "clock must return a u64 LE timestamp");
        let ms = u64::from_le_bytes(out.try_into().unwrap());
        assert!(ms > 0, "timestamp must be non-zero");
    }

    #[test]
    fn echo_tool_round_trips_payload() {
        let t = EchoTool;
        let payload = b"hello from echo";
        let out = t.invoke(payload).unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn text_io_tool_appends_newline() {
        let t = TextIoTool;
        let out = t.invoke(b"hello").unwrap();
        assert_eq!(out, b"hello\n");
    }

    #[test]
    fn text_io_tool_rejects_non_utf8() {
        let t = TextIoTool;
        let err = t.invoke(&[0xFF, 0xFE]).unwrap_err();
        assert_eq!(err, ToolInvocationError::InvalidPayload);
    }

    // ── Registry basics ───────────────────────────────────────────────────────

    #[test]
    fn new_registry_has_three_builtins() {
        let r = ToolRegistry::new();
        assert_eq!(r.len(), 3);
        let ids = r.list();
        assert!(ids.contains(&"clock".to_string()));
        assert!(ids.contains(&"echo".to_string()));
        assert!(ids.contains(&"text-io".to_string()));
    }

    #[test]
    fn empty_registry_has_no_tools() {
        let r = ToolRegistry::empty();
        assert!(r.is_empty());
    }

    #[test]
    fn register_and_lookup_custom_tool() {
        let r = ToolRegistry::empty();
        r.register(EchoTool);
        assert!(r.lookup("echo").is_some());
        assert!(r.lookup("unknown").is_none());
    }

    #[test]
    fn list_returns_sorted_ids() {
        let r = ToolRegistry::empty();
        r.register(ClockTool);
        r.register(EchoTool);
        r.register(TextIoTool);
        let ids = r.list();
        assert_eq!(ids, vec!["clock", "echo", "text-io"]);
    }

    #[test]
    fn dispatch_echo_round_trips() {
        let r = ToolRegistry::new();
        let env = envelope("echo", b"test payload");
        let out = r.dispatch(&env).unwrap();
        assert_eq!(out, b"test payload");
    }

    #[test]
    fn dispatch_unknown_tool_returns_error() {
        let r = ToolRegistry::new();
        let env = envelope("no-such-tool", b"");
        let err = r.dispatch(&env).unwrap_err();
        assert!(
            matches!(err, ToolInvocationError::ExecutionFailed(_)),
            "expected ExecutionFailed, got {err:?}"
        );
    }

    // ── 1k registration stress ────────────────────────────────────────────────

    /// Verifies that the registry survives 1 000 concurrent registrations
    /// without data corruption and all entries remain accessible.
    #[test]
    fn registry_survives_one_thousand_registrations() {
        use std::thread;

        #[derive(Debug)]
        struct IndexedEcho(usize);

        impl ToolDriver for IndexedEcho {
            fn id(&self) -> &'static str {
                // Leak the string — only acceptable in tests.
                Box::leak(format!("echo-{}", self.0).into_boxed_str())
            }
            fn schema(&self) -> &'static str {
                ""
            }
            fn invoke(&self, payload: &[u8]) -> Result<Vec<u8>, ToolInvocationError> {
                Ok(payload.to_vec())
            }
        }

        let registry = ToolRegistry::empty();
        let mut handles = Vec::new();

        // Register 1 000 tools across 10 threads (100 each).
        for t in 0..10usize {
            let r = registry.clone();
            handles.push(thread::spawn(move || {
                for i in 0..100usize {
                    r.register(IndexedEcho(t * 100 + i));
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // All 1 000 tools must be present and individually callable.
        assert_eq!(registry.len(), 1_000);
        for t in 0..10usize {
            for i in 0..100usize {
                let id = format!("echo-{}", t * 100 + i);
                assert!(registry.lookup(&id).is_some(), "missing tool {id}");
            }
        }
    }

    // ── Circuit breaker integration ───────────────────────────────────────────

    #[test]
    fn circuit_breaker_trips_after_repeated_failures() {
        use crate::ToolInvocationError;

        #[derive(Debug)]
        struct AlwaysFails;
        impl ToolDriver for AlwaysFails {
            fn id(&self) -> &'static str {
                "always-fails"
            }
            fn schema(&self) -> &'static str {
                ""
            }
            fn invoke(&self, _: &[u8]) -> Result<Vec<u8>, ToolInvocationError> {
                Err(ToolInvocationError::ExecutionFailed("boom".into()))
            }
        }

        let r = ToolRegistry::empty();
        r.register(AlwaysFails);

        // Should fail for DEFAULT_OPEN_THRESHOLD - 1 times without tripping.
        for _ in 0..DEFAULT_OPEN_THRESHOLD - 1 {
            let _ = r.dispatch(&envelope("always-fails", b""));
        }
        assert_eq!(
            r.breaker_state("always-fails"),
            Some(crate::BreakerState::Closed)
        );
        // The next failure trips the breaker.
        let _ = r.dispatch(&envelope("always-fails", b""));
        assert_eq!(
            r.breaker_state("always-fails"),
            Some(crate::BreakerState::Open)
        );
    }

    // ── Routing integration ───────────────────────────────────────────────────

    #[test]
    fn length_robust_filter_selects_correct_tools_from_benchmark_set() {
        use crate::routing::{length_robust_filter, ToolCandidate};

        // Documented benchmark set: three tools with known relevance scores.
        let candidates = vec![
            ToolCandidate {
                id: "clock".to_string(),
                score: 0.95,
            },
            ToolCandidate {
                id: "echo".to_string(),
                score: 0.82,
            },
            ToolCandidate {
                id: "text-io".to_string(),
                score: 0.45,
            },
        ];
        // τ_rel = 0.85: keeps only tools within 85% of the top score (0.95).
        // Threshold = 0.85 * 0.95 = 0.8075 → keeps clock (0.95) and echo (0.82).
        let kept = length_robust_filter(&candidates, 0.85);
        let ids: Vec<&str> = kept.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["clock", "echo"]);
    }
}
