//! The cache orchestrator: TTL tiers, freshness, stale-while-revalidate, and
//! single-flight over a pluggable [`CacheStore`].
//!
//! [`Cache`] is the type the rest of nbox talks to. It owns a profile partition
//! and a TTL policy and turns a [`CacheStore`]'s dumb `bytes` into typed,
//! freshness-tagged view models:
//!
//! - **fresh** (age ≤ the tier's TTL): served from cache, no network.
//! - **stale-but-serveable** (TTL < age ≤ TTL + grace): the [`get_or_fetch`]
//!   path revalidates (single-flighted) and, if the origin is down, serves the
//!   stale copy ([stale-if-error]); the TUI instead serves it instantly via
//!   [`lookup`] and kicks its own background reload.
//! - **expired** (age > TTL + grace): a hard miss.
//!
//! [`get_or_fetch`]: Cache::get_or_fetch
//! [`lookup`]: Cache::lookup
//! [stale-if-error]: https://www.rfc-editor.org/rfc/rfc5861

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::Mutex as AsyncMutex;

use crate::cache::key::CacheKey;
use crate::cache::store::{CacheEntry, CacheStore, UnixSecs, now_unix};
use crate::netbox::search::ObjectKind;

/// A wall-clock source, injectable so the freshness/SWR logic is testable.
type Clock = Arc<dyn Fn() -> UnixSecs + Send + Sync>;

/// TTL tiers, coarsened from the per-kind volatility of NetBox data. Configured
/// durations live in [`CacheConfig`]; [`Tier::for_detail`] maps object kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Rarely-edited reference data (sites, tenants, racks, providers, …).
    Static,
    /// Standard object detail (devices, prefixes, IPs, VLANs, …).
    Detail,
    /// Search / list result sets — membership shifts faster than fields.
    Search,
}

impl Tier {
    /// The tier for a detail view of `kind`: static reference data gets a long
    /// TTL; status-bearing objects get the shorter detail TTL.
    pub fn for_detail(kind: ObjectKind) -> Self {
        match kind {
            ObjectKind::Site
            | ObjectKind::Tenant
            | ObjectKind::Rack
            | ObjectKind::Provider
            | ObjectKind::Asn
            | ObjectKind::Contact
            | ObjectKind::Aggregate => Tier::Static,
            ObjectKind::Device
            | ObjectKind::Prefix
            | ObjectKind::IpAddress
            | ObjectKind::Vlan
            | ObjectKind::Circuit
            | ObjectKind::IpRange
            | ObjectKind::Vm
            | ObjectKind::Cluster => Tier::Detail,
        }
    }
}

/// Cache policy: the master on/off switch, a global TTL multiplier, the per-tier
/// TTLs (seconds), and the grace window during which a stale entry may still be
/// served (stale-while-revalidate + offline fallback).
#[derive(Debug, Clone)]
pub struct CacheConfig {
    pub enabled: bool,
    /// Multiplies every TTL — the single knob power users reach for.
    pub scale: f64,
    pub static_ttl: u64,
    pub detail_ttl: u64,
    pub search_ttl: u64,
    /// How long past its TTL an entry may still be served (SWR / stale-if-error).
    pub stale_grace: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        // Conservative for an infra tool: short TTLs + visible age beat fast.
        Self {
            enabled: true,
            scale: 1.0,
            static_ttl: 900,
            detail_ttl: 300,
            search_ttl: 60,
            stale_grace: 600,
        }
    }
}

impl CacheConfig {
    /// Build the runtime policy from the user's `[cache]` settings: the on/off
    /// switch and the TTL scale come from config; the per-tier TTLs and grace
    /// window keep their (conservative) defaults. A non-positive or non-finite
    /// `ttl_scale` is rejected back to `1.0`.
    pub fn from_settings(s: &crate::config::CacheSettings) -> Self {
        let scale = if s.ttl_scale.is_finite() && s.ttl_scale > 0.0 {
            s.ttl_scale
        } else {
            1.0
        };
        Self {
            enabled: s.enabled,
            scale,
            ..Self::default()
        }
    }

    fn base_ttl(&self, tier: Tier) -> u64 {
        match tier {
            Tier::Static => self.static_ttl,
            Tier::Detail => self.detail_ttl,
            Tier::Search => self.search_ttl,
        }
    }

    /// The effective TTL for `tier` after the global `scale`.
    fn max_age(&self, tier: Tier) -> u64 {
        let scaled = (self.base_ttl(tier) as f64 * self.scale).round();
        if scaled.is_finite() && scaled >= 0.0 {
            scaled as u64
        } else {
            self.base_ttl(tier)
        }
    }

    /// The hard lifetime of an entry: TTL plus the stale grace window.
    fn hard_window(&self, tier: Tier) -> u64 {
        self.max_age(tier).saturating_add(self.stale_grace)
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

/// Freshness metadata travelling with a cached value — what the TUI footer and
/// the MCP `cached_at` annotation are built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Freshness {
    pub source: Source,
    /// Seconds since the value was fetched from NetBox (`0` for `Origin`).
    pub age: u64,
    /// Past its TTL — served under the grace window (revalidate / offline).
    pub stale: bool,
}

impl Freshness {
    fn origin() -> Self {
        Self {
            source: Source::Origin,
            age: 0,
            stale: false,
        }
    }
}

/// A value plus its [`Freshness`].
#[derive(Debug, Clone)]
pub struct Cached<T> {
    pub value: T,
    pub freshness: Freshness,
}

/// The cache facade: a profile partition + TTL policy over a [`CacheStore`].
/// Cheap to clone (everything shared behind `Arc`), so tasks can carry their own
/// handle.
#[derive(Clone)]
pub struct Cache {
    store: Arc<dyn CacheStore>,
    partition: Arc<str>,
    config: Arc<CacheConfig>,
    /// Per-key locks giving single-flight: concurrent (re)fetches of the same key
    /// collapse to one origin request.
    inflight: Arc<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>>,
    clock: Clock,
}

impl Cache {
    /// Build a cache over `store` for one connection's `partition`.
    pub fn new(store: Arc<dyn CacheStore>, partition: String, config: CacheConfig) -> Self {
        let clock: Clock = Arc::new(now_unix);
        Self::with_clock(store, partition, config, clock)
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
            config: Arc::new(config),
            inflight: Arc::new(Mutex::new(HashMap::new())),
            clock,
        }
    }

    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    fn now(&self) -> UnixSecs {
        (self.clock)()
    }

    /// Look up a cached value without touching the network. `None` when caching
    /// is off, the entry is absent/hard-expired, or it fails to deserialize (a
    /// stale schema is treated as a miss). The freshness tells the caller whether
    /// to revalidate — the TUI serves this instantly and reloads if `stale`.
    pub fn lookup<T: DeserializeOwned>(&self, key: &CacheKey, tier: Tier) -> Option<Cached<T>> {
        if !self.config.enabled {
            return None;
        }
        let now = self.now();
        let entry = self.store.get(&self.partition, key.as_str(), now)?;
        let value: T = serde_json::from_slice(&entry.bytes).ok()?;
        let age = now.saturating_sub(entry.fetched_at);
        let stale = age > self.config.max_age(tier);
        Some(Cached {
            value,
            freshness: Freshness {
                source: Source::Cache,
                age,
                stale,
            },
        })
    }

    /// Store a freshly-fetched value under `key`'s tier. A no-op when caching is
    /// off; a serialization failure is logged and skipped (never fatal).
    pub fn put<T: Serialize>(&self, key: &CacheKey, value: &T, tier: Tier) {
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
                expires_at: now.saturating_add(self.config.hard_window(tier)),
            },
        );
    }

    /// Serve `key` from cache when fresh, otherwise fetch from NetBox — the
    /// blocking SWR path for the CLI and MCP. Concurrent callers for the same key
    /// are single-flighted to one origin request. If the origin errors but a
    /// still-live (stale) entry exists, that entry is served (stale-if-error).
    pub async fn get_or_fetch<T, F, Fut>(
        &self,
        key: &CacheKey,
        tier: Tier,
        fetch: F,
    ) -> Result<Cached<T>>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        // Fast path: a fresh hit needs neither lock nor network.
        if let Some(c) = self.lookup::<T>(key, tier)
            && !c.freshness.stale
        {
            return Ok(c);
        }

        // Single-flight the (re)fetch on this key.
        let lock = self.inflight_lock(&self.full_key(key));
        let _guard = lock.lock().await;

        // Re-check under the lock — a peer may have refreshed it meanwhile. A
        // still-stale entry is retained as the stale-if-error fallback.
        let stale_fallback = match self.lookup::<T>(key, tier) {
            Some(c) if !c.freshness.stale => return Ok(c),
            other => other,
        };

        match fetch().await {
            Ok(value) => {
                self.put(key, &value, tier);
                Ok(Cached {
                    value,
                    freshness: Freshness::origin(),
                })
            }
            Err(e) => match stale_fallback {
                Some(c) => Ok(c), // origin down → serve the still-live stale copy
                None => Err(e),
            },
        }
    }

    /// Drop one cached entry (e.g. an explicit refresh of the focused object).
    pub fn invalidate(&self, key: &CacheKey) {
        self.store.invalidate(&self.partition, key.as_str());
    }

    /// Drop every entry for this profile partition.
    pub fn clear_profile(&self) {
        self.store.clear(Some(&self.partition));
    }

    /// Drop the entire cache, across all profiles.
    pub fn clear_all(&self) {
        self.store.clear(None);
    }

    /// Remove all hard-expired entries (best-effort housekeeping).
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
    fn max_age_scales_and_grace_extends_the_hard_window() {
        let cfg = CacheConfig {
            scale: 2.0,
            ..CacheConfig::default()
        };
        assert_eq!(cfg.max_age(Tier::Detail), 600, "300 * 2.0");
        assert_eq!(cfg.max_age(Tier::Search), 120);
        assert_eq!(cfg.hard_window(Tier::Detail), 600 + 600);
    }

    #[test]
    fn from_settings_maps_switch_and_scale_and_rejects_bad_scale() {
        use crate::config::CacheSettings;
        let on = CacheConfig::from_settings(&CacheSettings {
            enabled: true,
            ttl_scale: 3.0,
            path: None,
        });
        assert!(on.enabled);
        assert_eq!(on.max_age(Tier::Detail), 900, "300 * 3.0");

        // A zero / negative / non-finite scale falls back to 1.0 (never a 0 TTL).
        let bad = CacheConfig::from_settings(&CacheSettings {
            enabled: false,
            ttl_scale: 0.0,
            path: None,
        });
        assert!(!bad.enabled);
        assert!((bad.scale - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn for_detail_tiers_static_vs_volatile_kinds() {
        assert_eq!(Tier::for_detail(ObjectKind::Site), Tier::Static);
        assert_eq!(Tier::for_detail(ObjectKind::Tenant), Tier::Static);
        assert_eq!(Tier::for_detail(ObjectKind::Device), Tier::Detail);
        assert_eq!(Tier::for_detail(ObjectKind::IpAddress), Tier::Detail);
    }

    #[test]
    fn lookup_reports_fresh_then_stale_as_the_clock_advances() {
        let (cache, now) = test_cache();
        cache.put(&key(), &"v".to_string(), Tier::Detail);

        // Within the TTL: fresh.
        let c: Cached<String> = cache.lookup(&key(), Tier::Detail).unwrap();
        assert_eq!(c.value, "v");
        assert_eq!(c.freshness.source, Source::Cache);
        assert!(!c.freshness.stale);

        // Past the TTL but within grace: stale, with the age reported.
        now.store(1_000 + 301, Ordering::SeqCst);
        let c: Cached<String> = cache.lookup(&key(), Tier::Detail).unwrap();
        assert!(c.freshness.stale);
        assert_eq!(c.freshness.age, 301);

        // Past TTL + grace: hard miss.
        now.store(1_000 + 901, Ordering::SeqCst);
        assert!(cache.lookup::<String>(&key(), Tier::Detail).is_none());
    }

    #[tokio::test]
    async fn get_or_fetch_misses_then_hits() {
        let (cache, _now) = test_cache();
        let calls = AtomicUsize::new(0);
        let fetch = || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<_, anyhow::Error>("fresh".to_string())
        };

        let a: Cached<String> = cache
            .get_or_fetch(&key(), Tier::Detail, fetch)
            .await
            .unwrap();
        assert_eq!(a.value, "fresh");
        assert_eq!(a.freshness.source, Source::Origin);

        // Second call: a fresh hit, no fetch.
        let b: Cached<String> = cache
            .get_or_fetch(&key(), Tier::Detail, || async {
                panic!("must not fetch on a fresh hit")
            })
            .await
            .unwrap();
        assert_eq!(b.value, "fresh");
        assert_eq!(b.freshness.source, Source::Cache);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn get_or_fetch_revalidates_when_stale() {
        let (cache, now) = test_cache();
        cache.put(&key(), &"old".to_string(), Tier::Detail);
        now.store(1_000 + 400, Ordering::SeqCst); // past the 300s TTL

        let c: Cached<String> = cache
            .get_or_fetch(&key(), Tier::Detail, || async {
                Ok::<_, anyhow::Error>("new".to_string())
            })
            .await
            .unwrap();
        assert_eq!(c.value, "new", "stale entry is revalidated");
        assert_eq!(c.freshness.source, Source::Origin);
    }

    #[tokio::test]
    async fn get_or_fetch_serves_stale_on_origin_error() {
        let (cache, now) = test_cache();
        cache.put(&key(), &"cached".to_string(), Tier::Detail);
        now.store(1_000 + 400, Ordering::SeqCst); // stale, but within grace

        let c: Cached<String> = cache
            .get_or_fetch(&key(), Tier::Detail, || async {
                Err::<String, _>(anyhow::anyhow!("netbox down"))
            })
            .await
            .expect("stale-if-error serves the cached copy");
        assert_eq!(c.value, "cached");
        assert!(c.freshness.stale, "served stale under stale-if-error");
    }

    #[tokio::test]
    async fn get_or_fetch_propagates_error_with_no_entry() {
        let (cache, _now) = test_cache();
        let r: Result<Cached<String>> = cache
            .get_or_fetch(&key(), Tier::Detail, || async {
                Err::<String, _>(anyhow::anyhow!("netbox down"))
            })
            .await;
        assert!(r.is_err(), "a cold miss with a down origin is an error");
    }

    #[tokio::test]
    async fn disabled_cache_always_fetches_and_never_stores() {
        let now = Arc::new(AtomicU64::new(1_000));
        let n = now.clone();
        let clock: Clock = Arc::new(move || n.load(Ordering::SeqCst));
        let cache = Cache::with_clock(
            Arc::new(MemoryStore::new()),
            "test".to_string(),
            CacheConfig {
                enabled: false,
                ..CacheConfig::default()
            },
            clock,
        );
        let calls = AtomicUsize::new(0);
        for _ in 0..2 {
            let _c: Cached<String> = cache
                .get_or_fetch(&key(), Tier::Detail, || async {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, anyhow::Error>("v".to_string())
                })
                .await
                .unwrap();
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "no caching: every call fetches"
        );
        assert!(cache.lookup::<String>(&key(), Tier::Detail).is_none());
    }

    #[tokio::test]
    async fn concurrent_get_or_fetch_is_single_flighted() {
        let (cache, _now) = test_cache();
        let calls = Arc::new(AtomicUsize::new(0));
        let mk = |cache: Cache, calls: Arc<AtomicUsize>| async move {
            cache
                .get_or_fetch(&key(), Tier::Detail, || async {
                    calls.fetch_add(1, Ordering::SeqCst);
                    // Hold the in-flight lock long enough for the peer to queue.
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
