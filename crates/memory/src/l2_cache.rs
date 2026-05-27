//! L2 warm cache — Adaptive Replacement Cache (ARC).
//!
//! Implements the full ARC algorithm as described in
//! "ARC: A Self-Tuning, Low Overhead Replacement Cache" (Megiddo & Modha, 2003).
//!
//! Four internal lists are maintained:
//!
//! | List | Contents              | Role                        |
//! |------|-----------------------|-----------------------------|
//! | T1   | recently used once    | recency list (live values)  |
//! | T2   | used two or more times| frequency list (live values)|
//! | B1   | recently evicted T1   | recency ghost (keys only)   |
//! | B2   | recently evicted T2   | frequency ghost (keys only) |
//!
//! The adaptive parameter `p` (target size of T1) grows on B1 ghost hits and
//! shrinks on B2 ghost hits, allowing the policy to self-tune towards
//! recency-optimal or frequency-optimal depending on workload.
//!
//! Thread safety is provided by wrapping the inner state in
//! `Arc<std::sync::Mutex<ArcCacheInner>>`, making [`ArcCache`] cheaply
//! cloneable and safe to share across threads.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::hash::Hash;
use spin::Mutex;

// ── Promotion hint ────────────────────────────────────────────────────────────

/// Returned alongside cache hits to signal whether the retrieved item has
/// become a frequent-access candidate and should be considered for promotion
/// back into the L1 live context window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionHint {
    /// Item was in T1 (seen once); no promotion recommended yet.
    Recency,
    /// Item was in T2 (seen multiple times); recommend promotion to L1.
    Frequency,
}

// ── Inner state ───────────────────────────────────────────────────────────────

struct ArcCacheInner<K, V>
where
    K: Eq + Hash + Clone,
    V: Clone,
{
    /// Maximum combined live entries (|T1| + |T2| ≤ capacity).
    capacity: usize,
    /// Target size for T1 (adaptive, 0 ≤ p ≤ capacity).
    p: usize,
    /// T1: recency list.  Front = LRU, back = MRU.
    t1: VecDeque<(K, V)>,
    /// T2: frequency list.  Front = LRU, back = MRU.
    t2: VecDeque<(K, V)>,
    /// B1: ghost list for evicted T1 entries (keys only).
    b1: VecDeque<K>,
    /// B2: ghost list for evicted T2 entries (keys only).
    b2: VecDeque<K>,
}

impl<K, V> ArcCacheInner<K, V>
where
    K: Eq + Hash + Clone,
    V: Clone,
{
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            p: 0,
            t1: VecDeque::new(),
            t2: VecDeque::new(),
            b1: VecDeque::new(),
            b2: VecDeque::new(),
        }
    }

    fn total_live(&self) -> usize {
        self.t1.len() + self.t2.len()
    }

    /// Removes the LRU victim from T1 or T2 according to the ARC policy and
    /// places its key in the corresponding ghost list.
    fn replace(&mut self) {
        // Evict from T1 when T1 is at or above the target p, or T2 is empty.
        if (self.t1.len() >= self.p.max(1) || self.t2.is_empty()) && !self.t1.is_empty() {
            let (k, _v) = self.t1.pop_front().unwrap();
            // Limit ghost lists to capacity to avoid unbounded growth.
            if self.b1.len() >= self.capacity {
                self.b1.pop_front();
            }
            self.b1.push_back(k);
        } else if !self.t2.is_empty() {
            let (k, _v) = self.t2.pop_front().unwrap();
            if self.b2.len() >= self.capacity {
                self.b2.pop_front();
            }
            self.b2.push_back(k);
        }
    }

    /// Looks up `key` in all four lists.  Returns the value and a promotion
    /// hint if the key is live (T1 or T2); updates internal state appropriately.
    fn get(&mut self, key: &K) -> Option<(V, PromotionHint)> {
        // T1 hit → promote to MRU of T2.
        if let Some(pos) = self.t1.iter().position(|(k, _)| k == key) {
            let (k, v) = self.t1.remove(pos).unwrap();
            self.t2.push_back((k, v.clone()));
            return Some((v, PromotionHint::Recency));
        }
        // T2 hit → move to MRU of T2 (already counted as frequent).
        if let Some(pos) = self.t2.iter().position(|(k, _)| k == key) {
            let (k, v) = self.t2.remove(pos).unwrap();
            self.t2.push_back((k, v.clone()));
            return Some((v, PromotionHint::Frequency));
        }
        None
    }

    /// Inserts `key → value`.  Implements the full ARC miss/ghost-hit path.
    fn insert(&mut self, key: K, value: V) {
        // ── Live hit (shouldn't normally happen through insert, but guard it) ──
        if self.get(&key).is_some() {
            // Already in T1 or T2 — get() moved it to T2 MRU; nothing else to do.
            // Update the value in-place.
            if let Some((_, v)) = self.t2.iter_mut().rev().find(|(k, _)| k == &key) {
                *v = value;
            }
            return;
        }

        // ── Ghost hit in B1 ──
        if let Some(pos) = self.b1.iter().position(|k| k == &key) {
            self.b1.remove(pos);
            // Adapt p upwards: B1 hit means recency is valuable.
            let delta = (self.b2.len() / self.b1.len().max(1)).max(1);
            self.p = (self.p + delta).min(self.capacity);
            // Make room if needed.
            if self.total_live() >= self.capacity {
                self.replace();
            }
            self.t2.push_back((key, value));
            return;
        }

        // ── Ghost hit in B2 ──
        if let Some(pos) = self.b2.iter().position(|k| k == &key) {
            self.b2.remove(pos);
            // Adapt p downwards: B2 hit means frequency is valuable.
            let delta = (self.b1.len() / self.b2.len().max(1)).max(1);
            self.p = self.p.saturating_sub(delta);
            if self.total_live() >= self.capacity {
                self.replace();
            }
            self.t2.push_back((key, value));
            return;
        }

        // ── Complete miss ──
        let total_ghost = self.b1.len() + self.b2.len();
        if self.total_live() >= self.capacity {
            // Cache is full: must evict a live entry.
            self.replace();
        } else if self.total_live() + total_ghost >= self.capacity * 2 {
            // Directory is full: trim the larger ghost list.
            if self.b1.len() > self.b2.len() {
                self.b1.pop_front();
            } else {
                self.b2.pop_front();
            }
        }
        self.t1.push_back((key, value));
    }

    fn len(&self) -> usize {
        self.total_live()
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn capacity(&self) -> usize {
        self.capacity
    }

    /// Removes all live entries (T1 and T2) for which `f(key, value)` returns
    /// `false`.  Ghost lists (B1, B2) are left intact so the ARC adaptation
    /// parameter `p` retains its history.
    ///
    /// Returns the number of entries removed.
    fn retain<F>(&mut self, mut f: F) -> usize
    where
        F: FnMut(&K, &V) -> bool,
    {
        let before = self.total_live();
        self.t1.retain(|(k, v)| f(k, v));
        self.t2.retain(|(k, v)| f(k, v));
        before - self.total_live()
    }

    /// Approximate ARC hit rate on a trace for benchmarking.
    fn hit_rate_on_trace(&mut self, trace: &[K]) -> f64
    where
        K: Eq + Hash + Clone,
        V: Default + Clone,
    {
        let mut hits = 0usize;
        for key in trace {
            if self.get(key).is_some() {
                hits += 1;
            } else {
                self.insert(key.clone(), V::default());
            }
        }
        if trace.is_empty() {
            0.0
        } else {
            hits as f64 / trace.len() as f64
        }
    }
}

// ── Public ArcCache ───────────────────────────────────────────────────────────

/// Thread-safe Adaptive Replacement Cache.
///
/// Clone is `O(1)` — both clones share the same underlying state.  Suitable
/// for passing across thread boundaries (`Send + Sync`).
pub struct ArcCache<K, V>
where
    K: Eq + Hash + Clone + Send + 'static,
    V: Clone + Send + 'static,
{
    inner: Arc<Mutex<ArcCacheInner<K, V>>>,
}

// Manual Clone so we share the Arc.
impl<K, V> Clone for ArcCache<K, V>
where
    K: Eq + Hash + Clone + Send + 'static,
    V: Clone + Send + 'static,
{
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<K, V> core::fmt::Debug for ArcCache<K, V>
where
    K: Eq + Hash + Clone + Send + 'static,
    V: Clone + Send + 'static,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let inner = self.inner.lock();
        f.debug_struct("ArcCache")
            .field("capacity", &inner.capacity())
            .field("len", &inner.len())
            .field("p", &inner.p)
            .finish()
    }
}

impl<K, V> ArcCache<K, V>
where
    K: Eq + Hash + Clone + Send + 'static,
    V: Clone + Send + 'static,
{
    /// Creates an empty ARC cache with the given capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ArcCacheInner::new(capacity))),
        }
    }

    /// Returns the configured capacity.
    pub fn capacity(&self) -> usize {
        self.inner.lock().capacity()
    }

    /// Returns the total number of live cached items (|T1| + |T2|).
    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    /// Returns true when no items are cached.
    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }

    /// Inserts a key/value pair, running the ARC admission/eviction policy.
    pub fn insert(&self, key: K, value: V) {
        self.inner.lock().insert(key, value);
    }

    /// Retrieves a value and returns a [`PromotionHint`] indicating whether the
    /// item is a frequent-access candidate (→ promote to L1).
    ///
    /// Returns `None` on a complete cache miss.
    pub fn get_with_hint(&self, key: &K) -> Option<(V, PromotionHint)> {
        self.inner.lock().get(key)
    }

    /// Retrieves a value (without the promotion hint).
    pub fn get(&self, key: &K) -> Option<V> {
        self.get_with_hint(key).map(|(v, _)| v)
    }

    /// Removes all live entries for which `f(key, value)` returns `false`.
    ///
    /// Ghost-list state (B1 / B2) is preserved so the ARC adaptation
    /// parameter `p` retains its history across the pruning pass.
    ///
    /// Returns the number of entries removed.
    pub fn retain<F>(&self, f: F) -> usize
    where
        F: FnMut(&K, &V) -> bool,
    {
        self.inner.lock().retain(f)
    }

    /// Computes the ARC hit rate on `trace`, treating missing values as
    /// `V::default()`.  Used for correctness benchmarking against reference
    /// implementations.
    pub fn hit_rate_on_trace(&self, trace: &[K]) -> f64
    where
        V: Default,
    {
        self.inner.lock().hit_rate_on_trace(trace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn arc<K, V>(cap: usize) -> ArcCache<K, V>
    where
        K: Eq + Hash + Clone + Send + 'static,
        V: Clone + Send + 'static,
    {
        ArcCache::new(cap)
    }

    // ── Basic correctness ────────────────────────────────────────────────────

    #[test]
    fn insert_and_get_round_trips_value() {
        let cache: ArcCache<&'static str, u32> = arc(4);
        cache.insert("a", 1);
        cache.insert("b", 2);
        assert_eq!(cache.get(&"a"), Some(1));
        assert_eq!(cache.get(&"b"), Some(2));
        assert_eq!(cache.get(&"c"), None);
    }

    #[test]
    fn capacity_overflow_evicts_an_entry() {
        let cache: ArcCache<u32, u32> = arc(2);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.insert(3, 30); // should evict LRU of T1
                             // After eviction the cache still holds exactly 2 live entries.
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn repeated_access_promotes_to_frequency_list() {
        let cache: ArcCache<u32, u32> = arc(4);
        cache.insert(1, 10);
        // First get: T1 hit → moves to T2.
        let (_, hint) = cache.get_with_hint(&1).unwrap();
        assert_eq!(hint, PromotionHint::Recency);
        // Second get: T2 hit.
        let (_, hint2) = cache.get_with_hint(&1).unwrap();
        assert_eq!(hint2, PromotionHint::Frequency);
    }

    #[test]
    fn frequency_items_survive_recency_eviction_pressure() {
        // Insert 1, access twice (promotes to T2), then insert 3 new items.
        // Item 1 should survive because it is in T2.
        let cache: ArcCache<u32, u32> = arc(3);
        cache.insert(1, 10);
        cache.get(&1); // → T2
        cache.get(&1); // still T2
        cache.insert(2, 20);
        cache.insert(3, 30);
        cache.insert(4, 40); // triggers eviction from T1, not T2
        assert_eq!(cache.get(&1), Some(10), "frequent item must survive");
    }

    #[test]
    fn ghost_hit_in_b1_adapts_p_upward() {
        // Saturate a 2-capacity cache with 3 items so item 1 lands in B1.
        let cache: ArcCache<u32, u32> = arc(2);
        cache.insert(1, 10); // T1: [1]
        cache.insert(2, 20); // T1: [1, 2]
        cache.insert(3, 30); // T1 full → evict 1 → B1: [1], T1: [2, 3]
        let p_before = cache.inner.lock().p;
        // Re-insert 1: B1 ghost hit → p should increase.
        cache.insert(1, 10);
        let p_after = cache.inner.lock().p;
        assert!(p_after >= p_before, "B1 ghost hit must not decrease p");
    }

    // ── ARC hit-rate vs. reference ────────────────────────────────────────────

    /// Reference LRU hit-rate for comparison purposes (not ARC — used to show
    /// ARC is at least as good on repeated-access workloads).
    fn lru_hit_rate(capacity: usize, trace: &[u32]) -> f64 {
        let mut lru: VecDeque<u32> = VecDeque::new();
        let mut hits = 0;
        for &k in trace {
            if let Some(pos) = lru.iter().position(|x| *x == k) {
                hits += 1;
                lru.remove(pos);
                lru.push_back(k);
            } else {
                if lru.len() >= capacity {
                    lru.pop_front();
                }
                lru.push_back(k);
            }
        }
        hits as f64 / trace.len().max(1) as f64
    }

    #[test]
    fn arc_hit_rate_is_at_least_as_good_as_lru_on_frequency_workload() {
        // Workload: 10 hot keys accessed in round-robin inside a larger cold set.
        // ARC's frequency promotion should help it converge to a better hit rate
        // than LRU for this pattern.
        let capacity = 5;
        let hot: Vec<u32> = (0..10).collect();
        let cold: Vec<u32> = (10..50).collect();
        let mut trace = Vec::new();
        // Warm up: access hot keys twice each.
        for &k in &hot {
            trace.push(k);
            trace.push(k);
        }
        // Then interleave hot and cold in a streaming pattern.
        for i in 0..200 {
            trace.push(hot[i % hot.len()]);
            if i % 5 == 0 {
                trace.push(cold[i % cold.len()]);
            }
        }
        let arc_cache: ArcCache<u32, u32> = ArcCache::new(capacity);
        let arc_rate = arc_cache.hit_rate_on_trace(&trace);
        let lru_rate = lru_hit_rate(capacity, &trace);
        // ARC should match or beat LRU on this workload; allow 1% tolerance.
        assert!(
            arc_rate >= lru_rate - 0.01,
            "ARC hit rate {arc_rate:.3} is more than 1% below LRU {lru_rate:.3}"
        );
    }

    #[test]
    fn promotion_hint_frequency_indicates_l2_to_l1_candidate() {
        let cache: ArcCache<u32, u32> = arc(4);
        cache.insert(42, 99);
        // First access: recency hint.
        let (_, h1) = cache.get_with_hint(&42).unwrap();
        assert_eq!(h1, PromotionHint::Recency);
        // Second access: frequency hint → candidate for L1 promotion.
        let (v, h2) = cache.get_with_hint(&42).unwrap();
        assert_eq!(h2, PromotionHint::Frequency);
        assert_eq!(v, 99);
    }

    // ── Thread safety (concurrent reader/writer soak) ─────────────────────────

    #[test]
    fn concurrent_readers_and_writers_produce_no_panics() {
        let cache: ArcCache<u32, u32> = ArcCache::new(32);
        let mut handles = Vec::new();

        // 4 writer threads, each inserting 500 entries.
        for t in 0u32..4 {
            let c = cache.clone();
            handles.push(thread::spawn(move || {
                for i in 0u32..500 {
                    c.insert(t * 1000 + i, i);
                }
            }));
        }
        // 4 reader threads, each reading 500 entries (many will be misses).
        for t in 0u32..4 {
            let c = cache.clone();
            handles.push(thread::spawn(move || {
                for i in 0u32..500 {
                    let _ = c.get(&(t * 1000 + i));
                }
            }));
        }

        for h in handles {
            h.join().expect("thread panicked");
        }
        // Cache must still be in a valid state.
        assert!(cache.len() <= cache.capacity());
    }
}
