//! Local read cache (flagship): a profile-scoped, view-model-level cache with
//! per-kind TTLs and stale-while-revalidate semantics.
//!
//! Why this shape: NetBox sends no usable HTTP cache headers (its `ETag` is a
//! write-concurrency token, not a read validator — no `304` path), and nbox's
//! value unit is the *assembled* view model (one detail view = many fanned-out
//! API calls composed into one struct). So the cache works at the view-model
//! layer, keyed by `(profile, kind, ref)`, not at the HTTP layer.
//!
//! Storage is pluggable behind [`CacheStore`]: an in-memory map ([`MemoryStore`])
//! is always available; an on-disk SQLite backend (`cache` feature, default-on)
//! adds cold-start, offline, and cross-invocation reuse. The orchestrator decides
//! freshness (fresh → stale-but-serveable → expired) from each entry's
//! `fetched_at`; the store only persists and evicts.

mod key;
mod orchestrator;
#[cfg(feature = "cache")]
mod sqlite;
mod store;

use std::sync::Arc;

use crate::config::CacheSettings;

pub use key::{CacheKey, profile_partition};
pub use orchestrator::{Cache, CacheConfig, Cached, Freshness, Source, Tier};
#[cfg(feature = "cache")]
pub use sqlite::SqliteStore;
pub use store::{CacheEntry, CacheStore, MemoryStore, UnixSecs, now_unix};

/// The default on-disk cache file: `$XDG_CACHE_HOME/nbox/cache.db` (and the
/// macOS/Windows equivalents). `None` if no cache directory can be determined.
#[cfg(feature = "cache")]
pub fn default_cache_path() -> Option<std::path::PathBuf> {
    dirs::cache_dir().map(|d| d.join("nbox").join("cache.db"))
}

/// Open the backing store for the given settings.
///
/// With the `cache` feature, caching enabled, and a resolvable path, this is the
/// on-disk SQLite store; if the file can't be opened it degrades to the in-memory
/// store (logged, never fatal). Without the feature — or with caching disabled —
/// it's always the in-memory store (and the orchestrator's `enabled=false` makes
/// it a no-op anyway, so nothing is written).
pub fn open_store(settings: &CacheSettings) -> Arc<dyn CacheStore> {
    #[cfg(feature = "cache")]
    if settings.enabled {
        let path = settings
            .path
            .clone()
            .map(std::path::PathBuf::from)
            .or_else(default_cache_path);
        match path {
            Some(path) => match SqliteStore::open(&path) {
                Ok(store) => return Arc::new(store),
                Err(e) => {
                    tracing::warn!("cache: on-disk store unavailable, using memory ({e:#})");
                }
            },
            None => tracing::warn!("cache: no cache directory found, using memory"),
        }
    }
    // Without the `cache` feature the on-disk backend doesn't exist; the in-memory
    // store is the only option (and `enabled` is honored by the orchestrator).
    #[cfg(not(feature = "cache"))]
    let _ = settings;
    Arc::new(MemoryStore::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> CacheEntry {
        CacheEntry {
            bytes: vec![1, 2, 3],
            fetched_at: 0,
            expires_at: 10_000,
        }
    }

    #[test]
    fn open_store_when_disabled_is_a_usable_memory_store() {
        let settings = CacheSettings {
            enabled: false,
            ttl_scale: 1.0,
            path: None,
        };
        let store = open_store(&settings);
        store.put("p", "k", &entry());
        assert!(store.get("p", "k", 10).is_some());
    }

    #[cfg(feature = "cache")]
    #[test]
    fn open_store_when_enabled_creates_the_sqlite_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.db");
        let settings = CacheSettings {
            enabled: true,
            ttl_scale: 1.0,
            path: Some(path.to_string_lossy().into_owned()),
        };
        let store = open_store(&settings);
        store.put("p", "k", &entry());
        assert!(store.get("p", "k", 10).is_some());
        drop(store);
        assert!(
            path.exists(),
            "on-disk cache file created at the override path"
        );
    }
}
