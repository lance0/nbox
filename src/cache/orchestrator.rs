//! The cache orchestrator: a small single-TTL, in-memory view-model cache with
//! freshness tracking and single-flight, over a [`CacheStore`].
//!
//! Deliberately minimal. Nothing persists to disk and nothing is ever served past
//! its TTL — there is no stale-while-revalidate and no offline fallback. The TTL
//! is a *de-dupe* window (default 30s): long enough to collapse a burst of
//! identical reads (TUI back-navigation, a chatty MCP agent inspecting one object
//! several times in a few seconds), short enough that "cached Ns ago" never raises
//! an eyebrow for infrastructure data. `r` / auto-refresh / profile switch always
//! bust and refetch.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::Mutex as AsyncMutex;

use crate::cache::key::CacheKey;
use crate::cache::store::{CacheEntry, CacheStore, MemoryStore, UnixSecs, now_unix};

/// A wall-clock source, injectable so the freshness logic is testable.
type Clock = Arc<dyn Fn() -> UnixSecs + Send + Sync>;

/// Bounds on the configurable TTL (seconds). A de-dupe window, not a freshness
/// window: tight enough that cached data never surprises, loose enough to cover a
/// burst of identical reads.
pub const MIN_TTL_SECS: u64 = 5;
pub const MAX_TTL_SECS: u64 = 300;
pub const DEFAULT_TTL_SECS: u64 = 30;

/// Cache policy: the on/off switch and the single TTL.
#[derive(Debug, Clone, Copy)]
pub struct CacheConfig {
    pub enabled: bool,
    pub ttl_secs: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            ttl_secs: DEFAULT_TTL_SECS,
        }
    }
}

impl CacheConfig {
    /// Build the runtime policy from the user's `[cache]` settings, clamping the
    /// TTL into `MIN_TTL_SECS..=MAX_TTL_SECS`.
    pub fn from_settings(s: &crate::config::CacheSettings) -> Self {
        Self {
            enabled: s.enabled,
            ttl_secs: s.ttl_secs.clamp(MIN_TTL_SECS, MAX_TTL_SECS),
        }
    }
}

/// Where a returned value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Freshly fetched from NetBox.
    Origin,
    /// Served from the cache.
    Cache,
}

/// Freshness metadata travelling with a value — what the TUI footer shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Freshness {
    pub source: Source,
    /// Seconds since the value was fetched from NetBox (`0` for `Origin`).
    pub age: u64,
}

impl Freshness {
    fn origin() -> Self {
        Self {
            source: Source::Origin,
            age: 0,
        }
    }
}

/// A value plus its [`Freshness`].
#[derive(Debug, Clone)]
pub struct Cached<T> {
    pub value: T,
    pub freshness: Freshness,
}

/// The cache facade: a profile partition + single TTL over an in-memory
/// [`CacheStore`]. Cheap to clone (everything shared behind `Arc`), so tasks can
/// carry their own handle.
#[derive(Clone)]
pub struct Cache {
    store: Arc<dyn CacheStore>,
    partition: Arc<str>,
    config: CacheConfig,
    /// Per-key locks giving single-flight: concurrent fetches of the same key
    /// collapse to one origin request.
    inflight: Arc<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>>,
    clock: Clock,
}

impl Cache {
    /// Build a cache over `store` for one connection's `partition`.
    pub fn new(store: Arc<dyn CacheStore>, partition: String, config: CacheConfig) -> Self {
        Self::with_clock(store, partition, config, Arc::new(now_unix))
    }

    /// An in-memory cache built from the user's `[cache]` settings.
    pub fn from_settings(partition: String, settings: &crate::config::CacheSettings) -> Self {
        Self::new(
            Arc::new(MemoryStore::new()),
            partition,
            CacheConfig::from_settings(settings),
        )
    }

    /// A no-op cache — the App's default until a real one is installed, and the
    /// shape every existing test gets (so the cache is invisible to them).
    pub fn disabled() -> Self {
        Self::new(
            Arc::new(MemoryStore::new()),
            "disabled".to_string(),
            CacheConfig {
                enabled: false,
                ttl_secs: DEFAULT_TTL_SECS,
            },
        )
    }

    fn with_clock(
        store: Arc<dyn CacheStore>,
        partition: String,
        config: CacheConfig,
        clock: Clock,
    ) -> Self {
        Self {
            store,
            partition: Arc::from(partition),
            config,
            inflight: Arc::new(Mutex::new(HashMap::new())),
            clock,
        }
    }

    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn ttl_secs(&self) -> u64 {
        self.config.ttl_secs
    }

    fn now(&self) -> UnixSecs {
        (self.clock)()
    }

    /// Look up a cached value without touching the network. `None` when caching is
    /// off, the entry is absent/expired, or it fails to deserialize (a stale schema
    /// is treated as a miss). A returned value is always within its TTL.
    pub fn lookup<T: DeserializeOwned>(&self, key: &CacheKey) -> Option<Cached<T>> {
        if !self.config.enabled {
            return None;
        }
        let now = self.now();
        let entry = self.store.get(&self.partition, key.as_str(), now)?;
        let value: T = serde_json::from_slice(&entry.bytes).ok()?;
        let age = now.saturating_sub(entry.fetched_at);
        Some(Cached {
            value,
            freshness: Freshness {
                source: Source::Cache,
                age,
            },
        })
    }

    /// Store a freshly-fetched value. A no-op when caching is off; a serialization
    /// failure is logged and skipped (never fatal).
    pub fn put<T: Serialize>(&self, key: &CacheKey, value: &T) {
        if !self.config.enabled {
            return;
        }
        let now = self.now();
        let bytes = match serde_json::to_vec(value) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("cache: skipping store of {} ({e})", key.as_str());
                return;
            }
        };
        self.store.put(
            &self.partition,
            key.as_str(),
            &CacheEntry {
                bytes,
                fetched_at: now,
                expires_at: now.saturating_add(self.config.ttl_secs),
            },
        );
    }

    /// Serve `key` from cache when within TTL, otherwise fetch from NetBox — the
    /// blocking path for the CLI and MCP. Concurrent callers for the same key are
    /// single-flighted to one origin request. A fetch error always propagates
    /// (nothing past TTL is ever served).
    pub async fn get_or_fetch<T, F, Fut>(&self, key: &CacheKey, fetch: F) -> Result<Cached<T>>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        // Fast path: a hit needs neither lock nor network.
        if let Some(c) = self.lookup::<T>(key) {
            return Ok(c);
        }
        // Single-flight the fetch on this key.
        let lock = self.inflight_lock(&self.full_key(key));
        let _guard = lock.lock().await;
        // Re-check under the lock — a peer may have filled it meanwhile.
        if let Some(c) = self.lookup::<T>(key) {
            return Ok(c);
        }
        let value = fetch().await?;
        self.put(key, &value);
        Ok(Cached {
            value,
            freshness: Freshness::origin(),
        })
    }

    /// Drop one cached entry (an explicit refresh of the focused object).
    pub fn invalidate(&self, key: &CacheKey) {
        self.store.invalidate(&self.partition, key.as_str());
    }

    /// Drop every entry for this profile partition.
    pub fn clear_profile(&self) {
        self.store.clear(Some(&self.partition));
    }

    /// Drop the entire cache.
    pub fn clear_all(&self) {
        self.store.clear(None);
    }

    /// Remove all expired entries (best-effort housekeeping).
    pub fn sweep(&self) {
        self.store.sweep(self.now());
    }

    fn full_key(&self, key: &CacheKey) -> String {
        // U+001F (unit separator) can't appear in a partition or key string.
        format!("{}\u{1f}{}", self.partition, key.as_str())
    }

    /// The per-key single-flight lock, creating it on first use. Idle locks (held
    /// only by the map) are pruned so the table tracks just contended keys.
    fn inflight_lock(&self, full: &str) -> Arc<AsyncMutex<()>> {
        let mut map = self.inflight.lock().unwrap();
        map.retain(|_, v| Arc::strong_count(v) > 1);
        map.entry(full.to_string()).or_default().clone()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use super::*;
    use crate::cache::store::MemoryStore;
    use crate::netbox::search::ObjectKind;

    /// A cache over a fresh `MemoryStore` with a manually-advanced clock.
    fn test_cache() -> (Cache, Arc<AtomicU64>) {
        let now = Arc::new(AtomicU64::new(1_000));
        let n = now.clone();
        let clock: Clock = Arc::new(move || n.load(Ordering::SeqCst));
        let cache = Cache::with_clock(
            Arc::new(MemoryStore::new()),
            "test".to_string(),
            CacheConfig::default(),
            clock,
        );
        (cache, now)
    }

    fn key() -> CacheKey {
        CacheKey::detail(ObjectKind::Device, 1)
    }

    #[test]
    fn from_settings_clamps_ttl_into_bounds() {
        use crate::config::CacheSettings;
        let too_low = CacheConfig::from_settings(&CacheSettings {
            enabled: true,
            ttl_secs: 1,
        });
        assert_eq!(too_low.ttl_secs, MIN_TTL_SECS);
        let too_high = CacheConfig::from_settings(&CacheSettings {
            enabled: true,
            ttl_secs: 100_000,
        });
        assert_eq!(too_high.ttl_secs, MAX_TTL_SECS);
        let ok = CacheConfig::from_settings(&CacheSettings {
            enabled: false,
            ttl_secs: 30,
        });
        assert!(!ok.enabled);
        assert_eq!(ok.ttl_secs, 30);
    }

    #[test]
    fn lookup_is_fresh_within_ttl_then_a_miss_after() {
        let (cache, now) = test_cache();
        cache.put(&key(), &"v".to_string());

        let c: Cached<String> = cache.lookup(&key()).unwrap();
        assert_eq!(c.value, "v");
        assert_eq!(c.freshness.source, Source::Cache);

        now.store(1_000 + 29, Ordering::SeqCst);
        assert!(
            cache.lookup::<String>(&key()).is_some(),
            "still within 30s TTL"
        );

        now.store(1_000 + 30, Ordering::SeqCst);
        assert!(
            cache.lookup::<String>(&key()).is_none(),
            "nothing is served at or past the TTL"
        );
    }

    #[tokio::test]
    async fn get_or_fetch_misses_then_hits() {
        let (cache, _now) = test_cache();
        let calls = AtomicUsize::new(0);
        let a: Cached<String> = cache
            .get_or_fetch(&key(), || async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, anyhow::Error>("fresh".to_string())
            })
            .await
            .unwrap();
        assert_eq!(a.value, "fresh");
        assert_eq!(a.freshness.source, Source::Origin);

        let b: Cached<String> = cache
            .get_or_fetch(&key(), || async { panic!("must not fetch on a hit") })
            .await
            .unwrap();
        assert_eq!(b.freshness.source, Source::Cache);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn get_or_fetch_refetches_after_ttl() {
        let (cache, now) = test_cache();
        cache.put(&key(), &"old".to_string());
        now.store(1_000 + 60, Ordering::SeqCst);
        let c: Cached<String> = cache
            .get_or_fetch(&key(), || async {
                Ok::<_, anyhow::Error>("new".to_string())
            })
            .await
            .unwrap();
        assert_eq!(c.value, "new");
        assert_eq!(c.freshness.source, Source::Origin);
    }

    #[tokio::test]
    async fn get_or_fetch_propagates_error_on_miss() {
        let (cache, _now) = test_cache();
        let r: Result<Cached<String>> = cache
            .get_or_fetch(&key(), || async {
                Err::<String, _>(anyhow::anyhow!("netbox down"))
            })
            .await;
        assert!(
            r.is_err(),
            "nothing cached + origin down => error (never serve stale)"
        );
    }

    #[tokio::test]
    async fn disabled_cache_always_fetches_and_never_stores() {
        let clock: Clock = Arc::new(|| 1_000);
        let cache = Cache::with_clock(
            Arc::new(MemoryStore::new()),
            "test".to_string(),
            CacheConfig {
                enabled: false,
                ttl_secs: 30,
            },
            clock,
        );
        let calls = AtomicUsize::new(0);
        for _ in 0..2 {
            let _c: Cached<String> = cache
                .get_or_fetch(&key(), || async {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, anyhow::Error>("v".to_string())
                })
                .await
                .unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(cache.lookup::<String>(&key()).is_none());
    }

    #[tokio::test]
    async fn concurrent_get_or_fetch_is_single_flighted() {
        let (cache, _now) = test_cache();
        let calls = Arc::new(AtomicUsize::new(0));
        let mk = |cache: Cache, calls: Arc<AtomicUsize>| async move {
            cache
                .get_or_fetch(&key(), || async {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
                    Ok::<_, anyhow::Error>("v".to_string())
                })
                .await
                .map(|c| c.value)
        };
        let (a, b) = tokio::join!(
            mk(cache.clone(), calls.clone()),
            mk(cache.clone(), calls.clone()),
        );
        assert_eq!(a.unwrap(), "v");
        assert_eq!(b.unwrap(), "v");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "two concurrent callers collapse to one origin fetch"
        );
    }
}
