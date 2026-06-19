//! Cache storage.
//!
//! A [`CacheStore`] is a profile-scoped `key → bytes` map with a hard expiry. The
//! orchestrator ([`super::Cache`]) serializes view models into the `bytes` and
//! reads `fetched_at` to report freshness; the store only holds entries and drops
//! ones past their `expires_at`. The sole backend is the in-process, size-capped
//! [`MemoryStore`] — nothing is written to disk.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch. Wall-clock, used only for cache-age math, so a
/// pre-1970 clock (the only `duration_since` failure) harmlessly reads as `0`.
pub type UnixSecs = u64;

/// The current wall-clock time as [`UnixSecs`].
pub fn now_unix() -> UnixSecs {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A stored cache entry: the serialized value plus when it was fetched and when
/// it must be dropped. Always `fetched_at <= expires_at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheEntry {
    /// The serialized view model (JSON bytes).
    pub bytes: Vec<u8>,
    /// When the value was fetched from NetBox.
    pub fetched_at: UnixSecs,
    /// Hard drop-dead: the store treats the entry as absent once `now >= expires_at`.
    pub expires_at: UnixSecs,
}

impl CacheEntry {
    /// Whether the entry is still live (not past its hard expiry) at `now`.
    pub fn live(&self, now: UnixSecs) -> bool {
        now < self.expires_at
    }
}

/// A profile-scoped cache backend. Cheap to share across async tasks
/// (`Send + Sync`); calls are synchronous and fast (a local map), so the
/// orchestrator invokes them inline rather than off the runtime.
pub trait CacheStore: Send + Sync {
    /// The live entry for `(profile, key)`, or `None` if absent/expired. An
    /// expired hit is dropped as a side effect (lazy expiry).
    fn get(&self, profile: &str, key: &str, now: UnixSecs) -> Option<CacheEntry>;
    /// Store (or replace) the entry for `(profile, key)`.
    fn put(&self, profile: &str, key: &str, entry: &CacheEntry);
    /// Drop one entry.
    fn invalidate(&self, profile: &str, key: &str);
    /// Drop every entry for `profile`, or the whole cache when `None`.
    fn clear(&self, profile: Option<&str>);
    /// Best-effort removal of all entries past their `expires_at`.
    fn sweep(&self, now: UnixSecs);
}

/// Hard cap on the number of cached entries — a backstop so a long-lived TUI or
/// MCP session can't grow the cache without bound. With the short de-dupe TTL the
/// live working set is normally far smaller; this only guarantees an upper bound.
const MAX_ENTRIES: usize = 1024;

/// An in-process [`CacheStore`] backed by a mutex-guarded, size-capped map.
/// Process-lifetime only — nothing survives a restart.
#[derive(Debug, Default)]
pub struct MemoryStore {
    entries: Mutex<HashMap<(String, String), CacheEntry>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of entries held (live or not). Diagnostic / test helper.
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl CacheStore for MemoryStore {
    fn get(&self, profile: &str, key: &str, now: UnixSecs) -> Option<CacheEntry> {
        let mut map = self.entries.lock().unwrap();
        let k = (profile.to_string(), key.to_string());
        if let Some(e) = map.get(&k) {
            if e.live(now) {
                return Some(e.clone());
            }
        } else {
            return None;
        }
        // Present but expired: drop it (lazy expiry) and report a miss.
        map.remove(&k);
        None
    }

    fn put(&self, profile: &str, key: &str, entry: &CacheEntry) {
        let mut map = self.entries.lock().unwrap();
        let k = (profile.to_string(), key.to_string());
        // Stay within the cap: when inserting a new key into a full map, evict the
        // oldest entry (smallest `fetched_at`). The scan is cheap at this size.
        if !map.contains_key(&k)
            && map.len() >= MAX_ENTRIES
            && let Some(oldest) = map
                .iter()
                .min_by_key(|(_, e)| e.fetched_at)
                .map(|(k, _)| k.clone())
        {
            map.remove(&oldest);
        }
        map.insert(k, entry.clone());
    }

    fn invalidate(&self, profile: &str, key: &str) {
        self.entries
            .lock()
            .unwrap()
            .remove(&(profile.to_string(), key.to_string()));
    }

    fn clear(&self, profile: Option<&str>) {
        let mut map = self.entries.lock().unwrap();
        match profile {
            Some(p) => map.retain(|(prof, _), _| prof != p),
            None => map.clear(),
        }
    }

    fn sweep(&self, now: UnixSecs) {
        self.entries.lock().unwrap().retain(|_, e| e.live(now));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(body: &str, fetched_at: UnixSecs, expires_at: UnixSecs) -> CacheEntry {
        CacheEntry {
            bytes: body.as_bytes().to_vec(),
            fetched_at,
            expires_at,
        }
    }

    #[test]
    fn put_then_get_roundtrips_within_expiry() {
        let s = MemoryStore::new();
        s.put("p", "k", &entry("hi", 100, 200));
        let got = s.get("p", "k", 150).expect("live entry");
        assert_eq!(got.bytes, b"hi");
        assert_eq!(got.fetched_at, 100);
    }

    #[test]
    fn expired_entry_is_a_miss_and_is_dropped() {
        let s = MemoryStore::new();
        s.put("p", "k", &entry("hi", 100, 200));
        assert!(
            s.get("p", "k", 200).is_none(),
            "now == expires_at is expired"
        );
        assert!(s.get("p", "k", 250).is_none());
        // The expired entry was evicted on the first miss.
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn profile_scopes_keys() {
        let s = MemoryStore::new();
        s.put("a", "k", &entry("from-a", 0, 100));
        s.put("b", "k", &entry("from-b", 0, 100));
        assert_eq!(s.get("a", "k", 10).unwrap().bytes, b"from-a");
        assert_eq!(s.get("b", "k", 10).unwrap().bytes, b"from-b");
    }

    #[test]
    fn invalidate_removes_one_entry_only() {
        let s = MemoryStore::new();
        s.put("p", "k1", &entry("1", 0, 100));
        s.put("p", "k2", &entry("2", 0, 100));
        s.invalidate("p", "k1");
        assert!(s.get("p", "k1", 10).is_none());
        assert!(s.get("p", "k2", 10).is_some());
    }

    #[test]
    fn clear_by_profile_keeps_other_profiles() {
        let s = MemoryStore::new();
        s.put("a", "k", &entry("a", 0, 100));
        s.put("b", "k", &entry("b", 0, 100));
        s.clear(Some("a"));
        assert!(s.get("a", "k", 10).is_none());
        assert!(s.get("b", "k", 10).is_some(), "other profile untouched");
    }

    #[test]
    fn clear_all_empties_the_store() {
        let s = MemoryStore::new();
        s.put("a", "k", &entry("a", 0, 100));
        s.put("b", "k", &entry("b", 0, 100));
        s.clear(None);
        assert!(s.is_empty());
    }

    #[test]
    fn put_evicts_oldest_when_over_capacity() {
        let s = MemoryStore::new();
        let over = u64::try_from(MAX_ENTRIES).unwrap() + 10;
        for i in 0..over {
            // Increasing fetched_at, so k0 is the oldest; long expiry so nothing
            // expires during the test — only the cap should drop entries.
            s.put("p", &format!("k{i}"), &entry("v", i, i + 1_000_000));
        }
        assert!(s.len() <= MAX_ENTRIES, "size stays bounded by the cap");
        assert!(
            s.get("p", "k0", 1).is_none(),
            "the oldest entry was evicted"
        );
        let last = format!("k{}", over - 1);
        assert!(s.get("p", &last, 1).is_some(), "the newest entry is kept");
    }

    #[test]
    fn sweep_drops_only_expired_entries() {
        let s = MemoryStore::new();
        s.put("p", "live", &entry("live", 0, 100));
        s.put("p", "dead", &entry("dead", 0, 50));
        s.sweep(60);
        assert!(s.get("p", "live", 60).is_some());
        assert!(s.get("p", "dead", 60).is_none());
        assert_eq!(s.len(), 1);
    }
}
