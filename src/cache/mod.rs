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
mod store;

pub use key::{CacheKey, profile_partition};
pub use orchestrator::{Cache, CacheConfig, Cached, Freshness, Source, Tier};
pub use store::{CacheEntry, CacheStore, MemoryStore, UnixSecs, now_unix};
