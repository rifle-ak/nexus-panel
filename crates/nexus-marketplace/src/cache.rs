//! Metadata cache for marketplace data

use crate::models::ModDetails;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Cache entry with expiration
struct CacheEntry {
    data: ModDetails,
    inserted_at: Instant,
}

/// Simple in-memory metadata cache
pub struct MetadataCache {
    entries: HashMap<String, CacheEntry>,
    ttl: Duration,
    max_entries: usize,
}

impl MetadataCache {
    /// Create a new cache with default settings (5 minute TTL, 1000 entries max)
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            ttl: Duration::from_secs(300), // 5 minutes
            max_entries: 1000,
        }
    }

    /// Create a cache with custom TTL
    pub fn with_ttl(ttl: Duration) -> Self {
        Self { ttl, ..Self::new() }
    }

    /// Get a cached entry if it exists and hasn't expired
    pub fn get(&self, key: &str) -> Option<&ModDetails> {
        self.entries.get(key).and_then(|entry| {
            if entry.inserted_at.elapsed() < self.ttl {
                Some(&entry.data)
            } else {
                None
            }
        })
    }

    /// Insert or update a cache entry
    pub fn set(&mut self, key: String, data: ModDetails) {
        // Evict expired entries if cache is full
        if self.entries.len() >= self.max_entries {
            self.evict_expired();
        }

        // If still full, remove oldest entry
        if self.entries.len() >= self.max_entries {
            self.evict_oldest();
        }

        self.entries.insert(
            key,
            CacheEntry {
                data,
                inserted_at: Instant::now(),
            },
        );
    }

    /// Remove expired entries
    pub fn evict_expired(&mut self) {
        self.entries.retain(|_, entry| entry.inserted_at.elapsed() < self.ttl);
    }

    /// Remove the oldest entry
    fn evict_oldest(&mut self) {
        if let Some(oldest_key) = self
            .entries
            .iter()
            .min_by_key(|(_, entry)| entry.inserted_at)
            .map(|(key, _)| key.clone())
        {
            self.entries.remove(&oldest_key);
        }
    }

    /// Clear all cached entries
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Get the number of cached entries
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if the cache is empty
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for MetadataCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ModInfo;
    use chrono::Utc;

    fn create_test_mod_details(id: &str) -> ModDetails {
        ModDetails {
            info: ModInfo {
                id: id.to_string(),
                name: format!("Test Mod {}", id),
                description: "Test description".to_string(),
                author: "Test Author".to_string(),
                provider: "test".to_string(),
                game: "rust".to_string(),
                category: None,
                downloads: 1000,
                rating: Some(4.5),
                latest_version: "1.0.0".to_string(),
                updated_at: Utc::now(),
                url: format!("https://test.com/{}", id),
                icon_url: None,
            },
            full_description: None,
            versions: vec![],
            dependencies: vec![],
            screenshots: vec![],
            license: None,
            source_url: None,
            issues_url: None,
            community_url: None,
        }
    }

    #[test]
    fn test_cache_set_get() {
        let mut cache = MetadataCache::new();
        let details = create_test_mod_details("test-1");

        cache.set("test-1".to_string(), details);

        assert!(cache.get("test-1").is_some());
        assert_eq!(cache.get("test-1").unwrap().info.id, "test-1");
    }

    #[test]
    fn test_cache_miss() {
        let cache = MetadataCache::new();
        assert!(cache.get("nonexistent").is_none());
    }

    #[test]
    fn test_cache_expiration() {
        let mut cache = MetadataCache::with_ttl(Duration::from_millis(10));
        let details = create_test_mod_details("test-1");

        cache.set("test-1".to_string(), details);

        // Should exist immediately
        assert!(cache.get("test-1").is_some());

        // Wait for expiration
        std::thread::sleep(Duration::from_millis(15));

        // Should be expired
        assert!(cache.get("test-1").is_none());
    }

    #[test]
    fn test_cache_clear() {
        let mut cache = MetadataCache::new();
        cache.set("test-1".to_string(), create_test_mod_details("test-1"));
        cache.set("test-2".to_string(), create_test_mod_details("test-2"));

        assert_eq!(cache.len(), 2);

        cache.clear();

        assert!(cache.is_empty());
    }
}
