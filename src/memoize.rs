//! Memoization utility for caching function results.
//!
//! Provides a simple in-memory cache with TTL support.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

/// A cached entry with expiration
struct CacheEntry<V> {
    value: V,
    expires_at: Option<Instant>,
}

impl<V> CacheEntry<V> {
    fn new(value: V, ttl: Option<Duration>) -> Self {
        Self {
            value,
            expires_at: ttl.map(|d| Instant::now() + d),
        }
    }

    fn is_expired(&self) -> bool {
        self.expires_at
            .map(|t| Instant::now() > t)
            .unwrap_or(false)
    }
}

/// Thread-safe memoization cache with optional TTL
pub struct MemoCache<K, V> {
    cache: RwLock<HashMap<K, CacheEntry<V>>>,
    default_ttl: Option<Duration>,
}

impl<K, V> MemoCache<K, V>
where
    K: Eq + std::hash::Hash + Clone,
    V: Clone,
{
    /// Create a new memo cache without TTL
    pub fn new() -> Self {
        Self {
            cache: RwLock::new(HashMap::new()),
            default_ttl: None,
        }
    }

    /// Create a new memo cache with a default TTL
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            cache: RwLock::new(HashMap::new()),
            default_ttl: Some(ttl),
        }
    }

    /// Get a cached value, computing it if not present or expired.
    pub fn get_or_compute<F>(&self, key: K, compute: F) -> V
    where
        F: FnOnce() -> V,
    {
        // Try read first
        {
            let cache = self.cache.read().unwrap();
            if let Some(entry) = cache.get(&key) {
                if !entry.is_expired() {
                    return entry.value.clone();
                }
            }
        }

        // Compute and insert
        let value = compute();
        let entry = CacheEntry::new(value.clone(), self.default_ttl);
        self.cache.write().unwrap().insert(key, entry);
        value
    }

    /// Insert a value directly
    pub fn insert(&self, key: K, value: V) {
        let entry = CacheEntry::new(value, self.default_ttl);
        self.cache.write().unwrap().insert(key, entry);
    }

    /// Insert a value with a specific TTL
    pub fn insert_with_ttl(&self, key: K, value: V, ttl: Duration) {
        let entry = CacheEntry::new(value, Some(ttl));
        self.cache.write().unwrap().insert(key, entry);
    }

    /// Get a cached value if present and not expired
    pub fn get(&self, key: &K) -> Option<V> {
        let cache = self.cache.read().unwrap();
        cache.get(key).and_then(|entry| {
            if entry.is_expired() {
                None
            } else {
                Some(entry.value.clone())
            }
        })
    }

    /// Remove a cached value
    pub fn remove(&self, key: &K) {
        self.cache.write().unwrap().remove(key);
    }

    /// Clear all cached values
    pub fn clear(&self) {
        self.cache.write().unwrap().clear();
    }

    /// Remove all expired entries
    pub fn cleanup_expired(&self) {
        let mut cache = self.cache.write().unwrap();
        cache.retain(|_, entry| !entry.is_expired());
    }

    /// Get the number of non-expired entries
    pub fn len(&self) -> usize {
        let cache = self.cache.read().unwrap();
        cache.iter().filter(|(_, e)| !e.is_expired()).count()
    }

    /// Check if the cache is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<K, V> Default for MemoCache<K, V>
where
    K: Eq + std::hash::Hash + Clone,
    V: Clone,
{
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_cache() {
        let cache: MemoCache<String, i32> = MemoCache::new();
        let val = cache.get_or_compute("key".to_string(), || 42);
        assert_eq!(val, 42);

        // Second call should return cached value
        let val2 = cache.get_or_compute("key".to_string(), || 100);
        assert_eq!(val2, 42);
    }

    #[test]
    fn test_insert_and_get() {
        let cache: MemoCache<String, i32> = MemoCache::new();
        cache.insert("key".to_string(), 42);
        assert_eq!(cache.get(&"key".to_string()), Some(42));
    }

    #[test]
    fn test_remove() {
        let cache: MemoCache<String, i32> = MemoCache::new();
        cache.insert("key".to_string(), 42);
        cache.remove(&"key".to_string());
        assert_eq!(cache.get(&"key".to_string()), None);
    }

    #[test]
    fn test_clear() {
        let cache: MemoCache<String, i32> = MemoCache::new();
        cache.insert("a".to_string(), 1);
        cache.insert("b".to_string(), 2);
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn test_ttl_expiration() {
        let cache: MemoCache<String, i32> = MemoCache::with_ttl(Duration::from_millis(10));
        cache.insert("key".to_string(), 42);
        assert_eq!(cache.get(&"key".to_string()), Some(42));

        // Wait for expiration
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(cache.get(&"key".to_string()), None);
    }

    #[test]
    fn test_len() {
        let cache: MemoCache<String, i32> = MemoCache::new();
        assert!(cache.is_empty());
        cache.insert("a".to_string(), 1);
        cache.insert("b".to_string(), 2);
        assert_eq!(cache.len(), 2);
    }
}
