//! L2 warm cache - a minimal Adaptive Replacement Cache (ARC) sketch.
//!
//! This is a simplified ARC suitable for the early scaffold: it tracks two
//! LRU lists (recency `t1`, frequency `t2`) with a fixed combined capacity
//! and bumps items between them on access.

use std::collections::VecDeque;
use std::hash::Hash;

/// Adaptive replacement cache for unmapped KV blocks.
#[derive(Debug, Clone)]
pub struct ArcCache<K, V>
where
    K: Eq + Hash + Clone,
    V: Clone,
{
    capacity: usize,
    recency: VecDeque<(K, V)>,
    frequency: VecDeque<(K, V)>,
}

impl<K, V> ArcCache<K, V>
where
    K: Eq + Hash + Clone,
    V: Clone,
{
    /// Creates an empty cache with `capacity` slots split across both lists.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            recency: VecDeque::new(),
            frequency: VecDeque::new(),
        }
    }

    /// Returns the configured capacity.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns the total number of cached items.
    pub fn len(&self) -> usize {
        self.recency.len() + self.frequency.len()
    }

    /// Returns true when no items are cached.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Inserts a new key/value pair, evicting the LRU recency entry if full.
    pub fn insert(&mut self, key: K, value: V) {
        if let Some(pos) = self.recency.iter().position(|(k, _)| k == &key) {
            self.recency.remove(pos);
        }
        if let Some(pos) = self.frequency.iter().position(|(k, _)| k == &key) {
            self.frequency.remove(pos);
        }
        if self.len() >= self.capacity {
            // Prefer evicting from recency first; fall back to frequency.
            if self.recency.pop_front().is_none() {
                self.frequency.pop_front();
            }
        }
        self.recency.push_back((key, value));
    }

    /// Retrieves a value, promoting recency hits into the frequency list.
    pub fn get(&mut self, key: &K) -> Option<V> {
        if let Some(pos) = self.recency.iter().position(|(k, _)| k == key) {
            let (k, v) = self.recency.remove(pos).unwrap();
            self.frequency.push_back((k, v.clone()));
            return Some(v);
        }
        if let Some(pos) = self.frequency.iter().position(|(k, _)| k == key) {
            let (k, v) = self.frequency.remove(pos).unwrap();
            self.frequency.push_back((k, v.clone()));
            return Some(v);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_get_round_trips_value() {
        let mut cache: ArcCache<&'static str, u32> = ArcCache::new(4);
        cache.insert("a", 1);
        cache.insert("b", 2);
        assert_eq!(cache.get(&"a"), Some(1));
        assert_eq!(cache.get(&"b"), Some(2));
    }

    #[test]
    fn capacity_overflow_evicts_least_recent() {
        let mut cache: ArcCache<u32, u32> = ArcCache::new(2);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.insert(3, 30);
        assert_eq!(cache.get(&1), None);
        assert_eq!(cache.get(&2), Some(20));
        assert_eq!(cache.get(&3), Some(30));
    }

    #[test]
    fn hits_promote_to_frequency_list() {
        let mut cache: ArcCache<u32, u32> = ArcCache::new(2);
        cache.insert(1, 10);
        let _ = cache.get(&1); // promote to frequency
        cache.insert(2, 20);
        cache.insert(3, 30); // should evict from recency, not frequency
        assert_eq!(cache.get(&1), Some(10));
    }
}
