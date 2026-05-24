//! L3 cerebral archival store - vector-similarity addressable storage stub.
//!
//! The production implementation is backed by embedded LanceDB; this scaffold
//! provides an in-memory equivalent so the rest of the runtime can wire
//! against a stable interface.

/// A single archived memory item.
#[derive(Debug, Clone, PartialEq)]
pub struct ArchivedItem {
    /// Stable item identifier.
    pub id: u64,
    /// Embedded vector representation.
    pub embedding: Vec<f32>,
    /// Opaque payload bytes.
    pub payload: Vec<u8>,
}

/// Errors raised when interacting with the archival store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchivalStoreError {
    /// Provided embedding had an unexpected dimensionality.
    DimensionMismatch,
    /// The store has reached its configured capacity.
    AtCapacity,
}

/// In-memory archival store backed by linear cosine-similarity scoring.
#[derive(Debug, Clone)]
pub struct ArchivalStore {
    items: Vec<ArchivedItem>,
    expected_dim: usize,
    capacity: usize,
}

impl ArchivalStore {
    /// Creates an empty store accepting embeddings of `expected_dim`.
    pub fn new(expected_dim: usize, capacity: usize) -> Self {
        Self {
            items: Vec::new(),
            expected_dim,
            capacity,
        }
    }

    /// Number of stored items.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True when no items are stored.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Stores an item, validating its embedding dimensionality.
    pub fn store(&mut self, item: ArchivedItem) -> Result<(), ArchivalStoreError> {
        if item.embedding.len() != self.expected_dim {
            return Err(ArchivalStoreError::DimensionMismatch);
        }
        if self.items.len() >= self.capacity {
            return Err(ArchivalStoreError::AtCapacity);
        }
        self.items.push(item);
        Ok(())
    }

    /// Returns the top-`k` items by cosine similarity to `query`.
    pub fn search(&self, query: &[f32], k: usize) -> Vec<&ArchivedItem> {
        if query.len() != self.expected_dim || k == 0 {
            return Vec::new();
        }
        let mut scored: Vec<(f32, &ArchivedItem)> = self
            .items
            .iter()
            .map(|item| (cosine_similarity(query, &item.embedding), item))
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().take(k).map(|(_, item)| item).collect()
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: u64, embedding: Vec<f32>) -> ArchivedItem {
        ArchivedItem {
            id,
            embedding,
            payload: vec![],
        }
    }

    #[test]
    fn store_rejects_wrong_dimension() {
        let mut store = ArchivalStore::new(3, 8);
        let err = store.store(item(1, vec![1.0, 2.0])).unwrap_err();
        assert_eq!(err, ArchivalStoreError::DimensionMismatch);
    }

    #[test]
    fn store_rejects_at_capacity() {
        let mut store = ArchivalStore::new(2, 1);
        store.store(item(1, vec![1.0, 0.0])).unwrap();
        let err = store.store(item(2, vec![0.0, 1.0])).unwrap_err();
        assert_eq!(err, ArchivalStoreError::AtCapacity);
    }

    #[test]
    fn search_returns_highest_cosine_first() {
        let mut store = ArchivalStore::new(2, 8);
        store.store(item(1, vec![1.0, 0.0])).unwrap();
        store.store(item(2, vec![0.0, 1.0])).unwrap();
        store.store(item(3, vec![0.9, 0.1])).unwrap();
        let results = store.search(&[1.0, 0.0], 2);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, 1);
        assert_eq!(results[1].id, 3);
    }
}
