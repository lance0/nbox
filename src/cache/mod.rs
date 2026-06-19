//! Local read cache: a small, profile-scoped, in-memory view-model cache.
//!
//! Why this shape: NetBox sends no usable HTTP cache headers (its `ETag` is a
//! write-concurrency token, not a read validator — no `304` path), and nbox's
//! value unit is the *assembled* view model (one detail view = many fanned-out
//! API calls composed into one struct). So the cache works at the view-model
//! layer, keyed by `(profile, kind, ref)`, not at the HTTP layer.
//!
//! Deliberately modest: **in-memory only** (nothing on disk, nothing survives a
//! restart), bounded in size, and **single-TTL** — nothing is ever served past
//! its TTL (no stale-while-revalidate, no offline fallback). The TTL is a short
//! *de-dupe* window (default 30s) that collapses bursts of identical reads — TUI
//! back-navigation, a chatty MCP agent — without letting infrastructure data look
//! stale. `r` / auto-refresh / profile switch always bust and refetch.

mod key;
mod orchestrator;
mod store;

pub use key::{CacheKey, profile_partition};
pub use orchestrator::{
    Cache, CacheConfig, Cached, DEFAULT_TTL_SECS, Freshness, MAX_TTL_SECS, MIN_TTL_SECS, Source,
};
pub use store::{CacheEntry, CacheStore, MemoryStore, UnixSecs, now_unix};
