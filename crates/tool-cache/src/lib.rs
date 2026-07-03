#![forbid(unsafe_code)]

//! Tool response caching with TTL expiry and deduplication — Epic E26.
//!
//! # Overview
//!
//! `tool-cache` sits in front of the [`praxis::ToolRegistry`] dispatch path and
//! transparently returns cached responses for idempotent tool calls, reducing
//! redundant computation and LLM-side API cost.
//!
//! ## Key types
//!
//! - [`CacheConfig`] — TTL, capacity, per-tool overrides, bypass list.
//! - [`ToolCache`] — thread-safe store keyed on `(tool_id, payload_hash)`.
//! - [`CachedToolRegistry`] — wraps a `ToolRegistry` with transparent caching.
//! - [`CacheStats`] — hit/miss/eviction counters with a `hit_rate()` helper.
//! - [`CacheOutcome`] — per-dispatch metadata (`Hit { age_ms }`, `Miss`, `Bypassed`).
//!
//! ## Cache correctness invariants
//!
//! 1. Only successful (`Ok`) responses are cached.
//! 2. Tools listed in [`CacheConfig::bypass_tools`] are never cached (suitable
//!    for non-idempotent tools such as `clock`).
//! 3. Expired entries are evicted lazily on read and eagerly via [`ToolCache::evict_expired`].
//! 4. When [`CacheConfig::max_entries`] is reached, the oldest entry (by insertion
//!    timestamp) is displaced before a new one is inserted.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use praxis::{ToolEnvelope, ToolInvocationError, ToolRegistry};

// ── FNV-1a hash (no external dependencies) ───────────────────────────────────

fn fnv1a_hash(data: &[u8]) -> u64 {
    let mut h: u64 = 14_695_981_039_346_656_037;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(1_099_511_628_211);
    }
    h
}

// ── Current time helper ───────────────────────────────────────────────────────

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ── Cache key ─────────────────────────────────────────────────────────────────

/// Internal cache key: stable tool id + FNV-1a hash of the payload bytes.
type CacheKey = (String, u64);

fn make_key(tool_id: &str, payload: &[u8]) -> CacheKey {
    (tool_id.to_string(), fnv1a_hash(payload))
}

// ── Public types ──────────────────────────────────────────────────────────────

/// Configuration for the tool response cache.
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// Default TTL for cached entries in milliseconds (default: 60 000 = 1 min).
    pub default_ttl_ms: u64,
    /// Maximum number of entries held simultaneously (default: 1 024).
    pub max_entries: usize,
    /// Tool IDs whose results are never cached (e.g., `"clock"`, `"random"`).
    ///
    /// The `clock` tool is bypassed by default because its output changes on
    /// every call and caching it would return stale timestamps.
    pub bypass_tools: Vec<String>,
    /// Per-tool TTL overrides in milliseconds.  Takes precedence over
    /// [`Self::default_ttl_ms`] for matching tool IDs.
    pub tool_ttl_overrides: HashMap<String, u64>,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            default_ttl_ms: 60_000,
            max_entries: 1_024,
            bypass_tools: vec!["clock".to_string()],
            tool_ttl_overrides: HashMap::new(),
        }
    }
}

impl CacheConfig {
    /// Creates a configuration with no bypass list and a custom TTL.
    pub fn with_ttl(ttl_ms: u64) -> Self {
        Self {
            default_ttl_ms: ttl_ms,
            bypass_tools: Vec::new(),
            ..Default::default()
        }
    }
}

/// A single cached tool response.
#[derive(Debug, Clone)]
pub struct CacheEntry {
    /// The exact request payload that produced this response.
    ///
    /// Stored verbatim so a [`get`](ToolCacheInner::get) lookup can confirm an
    /// *exact* payload match rather than trusting the FNV-1a hash alone — an
    /// FNV-1a collision must never return a different payload's response.
    pub payload: Vec<u8>,
    /// The cached response bytes (owned copy).
    pub response: Vec<u8>,
    /// Unix timestamp (ms) when this entry expires.
    pub expires_at_ms: u64,
    /// Unix timestamp (ms) when this entry was inserted.
    pub inserted_at_ms: u64,
}

impl CacheEntry {
    /// Returns `true` when the entry has expired relative to `now_ms`.
    pub fn is_expired(&self, now_ms: u64) -> bool {
        now_ms >= self.expires_at_ms
    }

    /// Age of this entry in milliseconds relative to `now_ms`.
    pub fn age_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.inserted_at_ms)
    }
}

/// Snapshot of cache operation counters.
#[derive(Debug, Clone, Default)]
pub struct CacheStats {
    /// Number of lookups that returned a valid cached response.
    pub hits: u64,
    /// Number of lookups that resulted in a live tool invocation.
    pub misses: u64,
    /// Number of entries removed due to TTL expiry.
    pub ttl_evictions: u64,
    /// Number of entries removed due to capacity pressure.
    pub capacity_evictions: u64,
    /// Current number of live entries in the store.
    pub current_entries: usize,
}

impl CacheStats {
    /// Hit rate as a fraction `[0.0, 1.0]`.  Returns `0.0` when no lookups have occurred.
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }

    /// Total number of lookups (hits + misses).
    pub fn total_lookups(&self) -> u64 {
        self.hits + self.misses
    }
}

/// Per-dispatch cache outcome returned by [`CachedToolRegistry::dispatch_with_outcome`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheOutcome {
    /// A valid cached response was returned.
    Hit {
        /// Age of the cached entry at lookup time in milliseconds.
        age_ms: u64,
    },
    /// No valid cached response existed; the tool was invoked.
    Miss,
    /// The tool is in the bypass list; it was always invoked directly.
    Bypassed,
}

// ── Inner cache state ─────────────────────────────────────────────────────────

struct ToolCacheInner {
    entries: HashMap<CacheKey, CacheEntry>,
    stats: CacheStats,
    config: CacheConfig,
}

impl ToolCacheInner {
    fn ttl_for(&self, tool_id: &str) -> u64 {
        self.config
            .tool_ttl_overrides
            .get(tool_id)
            .copied()
            .unwrap_or(self.config.default_ttl_ms)
    }

    fn is_bypassed(&self, tool_id: &str) -> bool {
        self.config.bypass_tools.iter().any(|b| b == tool_id)
    }

    /// Returns a cached entry if present and unexpired, evicting it if stale.
    fn get(&mut self, tool_id: &str, payload: &[u8]) -> Option<CacheEntry> {
        if self.is_bypassed(tool_id) {
            return None;
        }
        let key = make_key(tool_id, payload);
        let now = now_ms();

        match self.entries.get(&key) {
            Some(e) if e.is_expired(now) => {
                self.entries.remove(&key);
                self.stats.ttl_evictions += 1;
                self.stats.misses += 1;
                self.stats.current_entries = self.entries.len();
                None
            }
            // Hash match AND exact payload match: a genuine hit. Comparing the
            // stored payload guards against FNV-1a collisions returning a
            // different request's cached response.
            Some(e) if e.payload == payload => {
                self.stats.hits += 1;
                Some(e.clone())
            }
            // Hash collided with a different payload: treat as a miss. The caller
            // will re-invoke the tool and `insert` overwrites this bucket.
            Some(_) => {
                self.stats.misses += 1;
                None
            }
            None => {
                self.stats.misses += 1;
                None
            }
        }
    }

    /// Stores a response.  Enforces the capacity limit by evicting the oldest entry.
    fn insert(&mut self, tool_id: &str, payload: &[u8], response: Vec<u8>) {
        if self.is_bypassed(tool_id) {
            return;
        }
        let now = now_ms();
        let ttl = self.ttl_for(tool_id);
        let key = make_key(tool_id, payload);

        // Capacity: evict the oldest entry when at the limit and inserting a new key.
        if self.entries.len() >= self.config.max_entries && !self.entries.contains_key(&key) {
            if let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.inserted_at_ms)
                .map(|(k, _)| k.clone())
            {
                self.entries.remove(&oldest);
                self.stats.capacity_evictions += 1;
            }
        }

        self.entries.insert(
            key,
            CacheEntry {
                payload: payload.to_vec(),
                response,
                expires_at_ms: now + ttl,
                inserted_at_ms: now,
            },
        );
        self.stats.current_entries = self.entries.len();
    }

    fn evict_expired(&mut self) -> usize {
        let now = now_ms();
        let before = self.entries.len();
        self.entries.retain(|_, e| !e.is_expired(now));
        let evicted = before - self.entries.len();
        self.stats.ttl_evictions += evicted as u64;
        self.stats.current_entries = self.entries.len();
        evicted
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.stats.current_entries = 0;
    }
}

// ── ToolCache (public, thread-safe) ──────────────────────────────────────────

/// Thread-safe tool response cache with TTL expiry and capacity eviction.
///
/// Multiple [`ToolCache`] clones share the same underlying store via `Arc`.
#[derive(Clone)]
pub struct ToolCache {
    inner: Arc<Mutex<ToolCacheInner>>,
}

impl ToolCache {
    /// Creates a new cache with the given configuration.
    pub fn new(config: CacheConfig) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ToolCacheInner {
                entries: HashMap::new(),
                stats: CacheStats::default(),
                config,
            })),
        }
    }

    /// Creates a new cache with default configuration (1-min TTL, 1 024 entries,
    /// `clock` tool bypassed).
    pub fn with_defaults() -> Self {
        Self::new(CacheConfig::default())
    }

    /// Returns a cached entry for `(tool_id, payload)` if one exists and has not
    /// expired, recording the lookup in the internal statistics.
    ///
    /// Returns `None` for bypassed tools, expired entries (which are removed), and
    /// genuine misses.
    pub fn get(&self, tool_id: &str, payload: &[u8]) -> Option<CacheEntry> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(tool_id, payload)
    }

    /// Stores a response for `(tool_id, payload)`.
    ///
    /// Silently no-ops for bypassed tools.
    pub fn insert(&self, tool_id: &str, payload: &[u8], response: Vec<u8>) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(tool_id, payload, response);
    }

    /// Removes all expired entries and returns how many were evicted.
    pub fn evict_expired(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .evict_expired()
    }

    /// Removes every entry from the cache.
    pub fn clear(&self) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// Returns a point-in-time snapshot of cache statistics.
    pub fn stats(&self) -> CacheStats {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut s = inner.stats.clone();
        s.current_entries = inner.entries.len();
        s
    }

    /// Returns the current number of entries.
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .len()
    }

    /// Returns `true` when the cache contains no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ── CachedToolRegistry ────────────────────────────────────────────────────────

/// A [`ToolRegistry`] wrapper that transparently caches successful responses.
///
/// - Idempotent tools (not in [`CacheConfig::bypass_tools`]) are served from
///   cache when a matching entry exists.
/// - Non-idempotent tools (`bypass_tools`) are always forwarded to the registry.
/// - Failed invocations are **never** cached.
/// - The underlying registry's circuit breakers remain authoritative.
///
/// Use [`Self::dispatch_with_outcome`] when the caller needs to know whether the
/// response came from cache (e.g., to emit an [`AuditEntry`] upstream).
#[derive(Clone)]
pub struct CachedToolRegistry {
    registry: ToolRegistry,
    cache: ToolCache,
}

impl CachedToolRegistry {
    /// Wraps `registry` with a new [`ToolCache`] using `config`.
    pub fn new(registry: ToolRegistry, config: CacheConfig) -> Self {
        Self {
            registry,
            cache: ToolCache::new(config),
        }
    }

    /// Wraps `registry` with default cache configuration.
    pub fn with_defaults(registry: ToolRegistry) -> Self {
        Self::new(registry, CacheConfig::default())
    }

    /// Returns a reference to the underlying cache.
    pub fn cache(&self) -> &ToolCache {
        &self.cache
    }

    /// Returns a reference to the underlying registry.
    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    /// Dispatches an envelope through the cache-backed registry.
    ///
    /// Equivalent to [`Self::dispatch_with_outcome`] but discards the outcome.
    pub fn dispatch(&self, envelope: &ToolEnvelope) -> Result<Vec<u8>, ToolInvocationError> {
        self.dispatch_with_outcome(envelope).0
    }

    /// Dispatches an envelope and returns both the result and the [`CacheOutcome`].
    ///
    /// Callers may use the outcome to emit audit events:
    ///
    /// ```rust,ignore
    /// match outcome {
    ///     CacheOutcome::Hit { age_ms } => audit.push(AuditEntry::ToolCacheHit { tool_id, age_ms }),
    ///     CacheOutcome::Miss => audit.push(AuditEntry::ToolCacheMiss { tool_id }),
    ///     CacheOutcome::Bypassed => {}
    /// }
    /// ```
    pub fn dispatch_with_outcome(
        &self,
        envelope: &ToolEnvelope,
    ) -> (Result<Vec<u8>, ToolInvocationError>, CacheOutcome) {
        let tool_id = &envelope.tool_id;
        let payload = &envelope.payload;

        // Check bypass first (avoids a lock on the inner cache).
        let bypassed = {
            let inner = self.cache.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.is_bypassed(tool_id)
        };
        if bypassed {
            return (self.registry.dispatch(envelope), CacheOutcome::Bypassed);
        }

        // Try the cache.
        if let Some(entry) = self.cache.get(tool_id, payload) {
            let age = entry.age_ms(now_ms());
            return (Ok(entry.response), CacheOutcome::Hit { age_ms: age });
        }

        // Cache miss: invoke the underlying registry.
        let result = self.registry.dispatch(envelope);
        if let Ok(ref response) = result {
            self.cache.insert(tool_id, payload, response.clone());
        }
        (result, CacheOutcome::Miss)
    }

    /// Evicts expired entries and returns the count removed.
    pub fn evict_expired(&self) -> usize {
        self.cache.evict_expired()
    }

    /// Clears all cached entries.
    pub fn clear_cache(&self) {
        self.cache.clear();
    }

    /// Returns current cache statistics.
    pub fn stats(&self) -> CacheStats {
        self.cache.stats()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use praxis::{Bus, ToolEnvelope, ToolRegistry};

    fn envelope(tool_id: &str, payload: &[u8]) -> ToolEnvelope {
        ToolEnvelope::new(Bus::Mcp, tool_id, payload.to_vec(), 0)
    }

    // ── CacheConfig ───────────────────────────────────────────────────────────

    #[test]
    fn default_config_has_expected_defaults() {
        let cfg = CacheConfig::default();
        assert_eq!(cfg.default_ttl_ms, 60_000);
        assert_eq!(cfg.max_entries, 1_024);
        assert!(cfg.bypass_tools.contains(&"clock".to_string()));
    }

    #[test]
    fn with_ttl_constructs_config_with_empty_bypass_list() {
        let cfg = CacheConfig::with_ttl(500);
        assert_eq!(cfg.default_ttl_ms, 500);
        assert!(cfg.bypass_tools.is_empty());
    }

    // ── CacheEntry ────────────────────────────────────────────────────────────

    #[test]
    fn cache_entry_is_expired_when_now_at_or_past_expiry() {
        let entry = CacheEntry {
            payload: vec![],
            response: vec![1, 2, 3],
            expires_at_ms: 1_000,
            inserted_at_ms: 0,
        };
        assert!(!entry.is_expired(999));
        assert!(entry.is_expired(1_000));
        assert!(entry.is_expired(1_001));
    }

    #[test]
    fn cache_entry_age_saturates_at_zero_for_future_insertion() {
        let entry = CacheEntry {
            payload: vec![],
            response: vec![],
            expires_at_ms: 2_000,
            inserted_at_ms: 500,
        };
        assert_eq!(entry.age_ms(300), 0); // inserted in the "future" relative to now
        assert_eq!(entry.age_ms(500), 0);
        assert_eq!(entry.age_ms(750), 250);
    }

    // ── CacheStats ────────────────────────────────────────────────────────────

    #[test]
    fn hit_rate_is_zero_when_no_lookups() {
        let stats = CacheStats::default();
        assert_eq!(stats.hit_rate(), 0.0);
        assert_eq!(stats.total_lookups(), 0);
    }

    #[test]
    fn hit_rate_reflects_ratio_of_hits_to_total() {
        let stats = CacheStats {
            hits: 3,
            misses: 1,
            ..Default::default()
        };
        assert!((stats.hit_rate() - 0.75).abs() < f64::EPSILON);
        assert_eq!(stats.total_lookups(), 4);
    }

    // ── ToolCache basic operations ────────────────────────────────────────────

    #[test]
    fn cache_miss_on_empty_store() {
        let cache = ToolCache::with_defaults();
        assert!(cache.get("echo", b"hello").is_none());
    }

    #[test]
    fn insert_then_get_returns_entry() {
        let cache = ToolCache::with_defaults();
        cache.insert("echo", b"hello", b"world".to_vec());
        let entry = cache.get("echo", b"hello").expect("should be present");
        assert_eq!(entry.response, b"world");
    }

    #[test]
    fn different_payloads_are_distinct_cache_keys() {
        let cache = ToolCache::with_defaults();
        cache.insert("echo", b"a", b"response-a".to_vec());
        cache.insert("echo", b"b", b"response-b".to_vec());
        assert_eq!(cache.get("echo", b"a").unwrap().response, b"response-a");
        assert_eq!(cache.get("echo", b"b").unwrap().response, b"response-b");
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn hash_collision_does_not_return_wrong_payload_response() {
        // Simulate two distinct payloads that land in the same FNV-1a bucket by
        // inserting an entry whose stored payload differs from the query payload
        // but shares its cache key. A naïve hash-only cache would return this as
        // a hit; the payload comparison must treat it as a miss instead.
        let cache = ToolCache::with_defaults();

        let stored_payload = b"payload-A".to_vec();
        let colliding_payload = b"payload-B".to_vec();

        {
            let mut inner = cache.inner.lock().unwrap();
            let now = now_ms();
            // Key is derived from `colliding_payload` (forcing the "collision"),
            // but the entry records `stored_payload` as the real request.
            inner.entries.insert(
                make_key("echo", &colliding_payload),
                CacheEntry {
                    payload: stored_payload.clone(),
                    response: b"response-for-A".to_vec(),
                    expires_at_ms: now + 60_000,
                    inserted_at_ms: now,
                },
            );
        }

        // Looking up the colliding payload must NOT return A's response.
        assert!(
            cache.get("echo", &colliding_payload).is_none(),
            "a hash collision must be treated as a miss, not a false hit"
        );
        let s = cache.stats();
        assert_eq!(s.hits, 0, "collision must not count as a hit");
        assert_eq!(s.misses, 1);
    }

    #[test]
    fn collision_miss_is_overwritten_by_correct_payload() {
        // After a colliding miss, inserting the correct payload+response under the
        // same key replaces the stale entry, and a subsequent lookup hits.
        let cache = ToolCache::with_defaults();
        let colliding_payload = b"payload-B".to_vec();

        {
            let mut inner = cache.inner.lock().unwrap();
            let now = now_ms();
            inner.entries.insert(
                make_key("echo", &colliding_payload),
                CacheEntry {
                    payload: b"payload-A".to_vec(),
                    response: b"response-for-A".to_vec(),
                    expires_at_ms: now + 60_000,
                    inserted_at_ms: now,
                },
            );
        }

        // Miss (collision), then store B's real response.
        assert!(cache.get("echo", &colliding_payload).is_none());
        cache.insert("echo", &colliding_payload, b"response-for-B".to_vec());

        let entry = cache
            .get("echo", &colliding_payload)
            .expect("B should now hit");
        assert_eq!(entry.response, b"response-for-B");
        assert_eq!(entry.payload, colliding_payload);
    }

    #[test]
    fn expired_entry_is_evicted_on_read() {
        let cfg = CacheConfig::with_ttl(0); // TTL = 0 ms → expires immediately
        let cache = ToolCache::new(cfg);
        cache.insert("echo", b"payload", b"result".to_vec());
        // Even with 0 ms TTL the entry expires at `now + 0` so any subsequent
        // call at or after that timestamp treats it as expired.
        // We sleep 1 ms to ensure the clock has advanced.
        std::thread::sleep(std::time::Duration::from_millis(1));
        assert!(cache.get("echo", b"payload").is_none());
    }

    #[test]
    fn stats_track_hits_and_misses() {
        let cache = ToolCache::with_defaults();
        cache.insert("echo", b"hi", b"there".to_vec());

        cache.get("echo", b"hi"); // hit
        cache.get("echo", b"hi"); // hit
        cache.get("echo", b"missing"); // miss

        let s = cache.stats();
        assert_eq!(s.hits, 2);
        assert_eq!(s.misses, 1);
        assert!((s.hit_rate() - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn clear_empties_the_cache() {
        let cache = ToolCache::with_defaults();
        cache.insert("echo", b"a", b"1".to_vec());
        cache.insert("echo", b"b", b"2".to_vec());
        assert_eq!(cache.len(), 2);
        cache.clear();
        assert_eq!(cache.len(), 0);
        assert!(cache.is_empty());
    }

    #[test]
    fn evict_expired_removes_only_expired_entries() {
        let cfg = CacheConfig::with_ttl(5_000); // long TTL for "live" entry
        let cache = ToolCache::new(cfg);
        cache.insert("echo", b"live", b"still-valid".to_vec());

        // Inject a manually-expired entry by directly inserting with expired timestamp.
        // We do this by using TTL=0 on a separate instance pointing at the same Arc.
        {
            let mut inner = cache.inner.lock().unwrap();
            let now = now_ms();
            inner.entries.insert(
                make_key("echo", b"stale"),
                CacheEntry {
                    payload: b"stale".to_vec(),
                    response: b"old".to_vec(),
                    expires_at_ms: now.saturating_sub(1), // already expired
                    inserted_at_ms: now.saturating_sub(100),
                },
            );
        }
        assert_eq!(cache.len(), 2);

        let evicted = cache.evict_expired();
        assert_eq!(evicted, 1);
        assert_eq!(cache.len(), 1);
        assert!(cache.get("echo", b"live").is_some());
    }

    // ── Bypass list ───────────────────────────────────────────────────────────

    #[test]
    fn bypassed_tool_is_never_cached() {
        let cache = ToolCache::with_defaults(); // clock is bypassed by default
        cache.insert("clock", b"", b"timestamp".to_vec());
        assert!(cache.get("clock", b"").is_none());
        assert!(cache.is_empty());
    }

    #[test]
    fn custom_bypass_list_blocks_specified_tools() {
        let cfg = CacheConfig {
            bypass_tools: vec!["random".to_string()],
            ..CacheConfig::default()
        };
        let cache = ToolCache::new(cfg);
        cache.insert("random", b"seed", b"42".to_vec());
        assert!(cache.get("random", b"seed").is_none());
        // Non-bypassed tool still works.
        cache.insert("echo", b"hi", b"world".to_vec());
        assert!(cache.get("echo", b"hi").is_some());
    }

    // ── Capacity eviction ─────────────────────────────────────────────────────

    #[test]
    fn capacity_limit_evicts_oldest_entry() {
        let cfg = CacheConfig {
            max_entries: 2,
            bypass_tools: Vec::new(),
            ..CacheConfig::default()
        };
        let cache = ToolCache::new(cfg);

        cache.insert("echo", b"first", b"1".to_vec());
        std::thread::sleep(std::time::Duration::from_millis(1));
        cache.insert("echo", b"second", b"2".to_vec());
        assert_eq!(cache.len(), 2);

        // Inserting a third entry should evict the oldest ("first").
        std::thread::sleep(std::time::Duration::from_millis(1));
        cache.insert("echo", b"third", b"3".to_vec());
        assert_eq!(cache.len(), 2);

        // "first" should be gone; "second" and "third" remain.
        assert!(cache.get("echo", b"first").is_none());
        assert!(cache.get("echo", b"second").is_some());
        assert!(cache.get("echo", b"third").is_some());

        let s = cache.stats();
        assert_eq!(s.capacity_evictions, 1);
    }

    // ── Per-tool TTL overrides ────────────────────────────────────────────────

    #[test]
    fn per_tool_ttl_override_is_respected() {
        let mut overrides = HashMap::new();
        overrides.insert("echo".to_string(), 0); // echo entries expire immediately
        let cfg = CacheConfig {
            default_ttl_ms: 60_000,
            tool_ttl_overrides: overrides,
            bypass_tools: Vec::new(),
            ..Default::default()
        };
        let cache = ToolCache::new(cfg);
        cache.insert("echo", b"data", b"resp".to_vec());
        std::thread::sleep(std::time::Duration::from_millis(1));
        // echo entry expired; text-io (default TTL) would still be valid.
        assert!(cache.get("echo", b"data").is_none());
    }

    // ── CachedToolRegistry ────────────────────────────────────────────────────

    #[test]
    fn cache_miss_invokes_underlying_tool() {
        let registry = ToolRegistry::new();
        let cached = CachedToolRegistry::with_defaults(registry);
        let env = envelope("echo", b"hello");
        let result = cached.dispatch(&env).expect("should succeed");
        assert_eq!(result, b"hello");
        assert_eq!(cached.stats().misses, 1);
        assert_eq!(cached.stats().hits, 0);
    }

    #[test]
    fn second_dispatch_of_same_call_is_a_cache_hit() {
        let registry = ToolRegistry::new();
        let cached = CachedToolRegistry::with_defaults(registry);
        let env = envelope("echo", b"repeat-me");

        cached.dispatch(&env).unwrap();
        cached.dispatch(&env).unwrap();

        let s = cached.stats();
        assert_eq!(s.hits, 1);
        assert_eq!(s.misses, 1);
    }

    #[test]
    fn cached_response_matches_original() {
        let registry = ToolRegistry::new();
        let cached = CachedToolRegistry::with_defaults(registry);
        let env = envelope("echo", b"cache-me");

        let first = cached.dispatch(&env).unwrap();
        let second = cached.dispatch(&env).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn bypassed_tool_always_returns_live_result() {
        let registry = ToolRegistry::new();
        let cached = CachedToolRegistry::with_defaults(registry);

        // clock is bypassed by default: dispatch twice, both should be misses.
        let env = envelope("clock", b"");
        cached.dispatch(&env).unwrap();
        cached.dispatch(&env).unwrap();

        // No hits expected; cache should still be empty.
        assert_eq!(cached.stats().hits, 0);
        assert!(cached.cache().is_empty());
    }

    #[test]
    fn dispatch_with_outcome_returns_hit_on_second_call() {
        let registry = ToolRegistry::new();
        let cached = CachedToolRegistry::with_defaults(registry);
        let env = envelope("echo", b"outcome-test");

        let (_, outcome1) = cached.dispatch_with_outcome(&env);
        assert_eq!(outcome1, CacheOutcome::Miss);

        let (_, outcome2) = cached.dispatch_with_outcome(&env);
        assert!(matches!(outcome2, CacheOutcome::Hit { .. }));
    }

    #[test]
    fn dispatch_with_outcome_reports_bypassed_for_clock() {
        let registry = ToolRegistry::new();
        let cached = CachedToolRegistry::with_defaults(registry);
        let env = envelope("clock", b"");
        let (_, outcome) = cached.dispatch_with_outcome(&env);
        assert_eq!(outcome, CacheOutcome::Bypassed);
    }

    #[test]
    fn failed_dispatch_is_not_cached() {
        let registry = ToolRegistry::new();
        let cached = CachedToolRegistry::with_defaults(registry);
        let env = envelope("nonexistent-tool", b"data");

        let result = cached.dispatch(&env);
        assert!(result.is_err());

        // The failed call should not populate the cache.
        assert!(cached.cache().is_empty());
        let s = cached.stats();
        assert_eq!(s.hits, 0);
        // Miss was recorded by the attempted cache look-up.
        assert_eq!(s.misses, 1);
    }

    #[test]
    fn clear_cache_removes_all_entries() {
        let registry = ToolRegistry::new();
        let cached = CachedToolRegistry::with_defaults(registry);
        cached.dispatch(&envelope("echo", b"a")).unwrap();
        cached.dispatch(&envelope("echo", b"b")).unwrap();
        assert_eq!(cached.cache().len(), 2);
        cached.clear_cache();
        assert!(cached.cache().is_empty());
    }

    #[test]
    fn concurrent_reads_do_not_panic() {
        use std::sync::Arc;
        use std::thread;

        let registry = ToolRegistry::new();
        let cached = Arc::new(CachedToolRegistry::with_defaults(registry));

        // Pre-populate.
        cached.dispatch(&envelope("echo", b"shared")).unwrap();

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let c = Arc::clone(&cached);
                thread::spawn(move || {
                    for _ in 0..50 {
                        let _ = c.dispatch(&envelope("echo", b"shared"));
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().expect("thread should not panic");
        }

        // All subsequent dispatches are hits (the first per-thread was a miss only on
        // the very first invocation; subsequent ones hit).
        let s = cached.stats();
        assert!(s.hits > 0, "expected cache hits from concurrent readers");
    }

    #[test]
    fn fnv1a_hash_is_deterministic() {
        assert_eq!(fnv1a_hash(b"hello"), fnv1a_hash(b"hello"));
        assert_ne!(fnv1a_hash(b"hello"), fnv1a_hash(b"world"));
        assert_ne!(fnv1a_hash(b"abc"), fnv1a_hash(b"cba"));
    }

    #[test]
    fn cache_clone_shares_underlying_state() {
        let cache = ToolCache::with_defaults();
        let clone = cache.clone();

        cache.insert("echo", b"shared", b"value".to_vec());

        // The clone should see the same entry.
        assert!(clone.get("echo", b"shared").is_some());
        assert_eq!(clone.len(), 1);
    }
}
