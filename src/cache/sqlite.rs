//! On-disk SQLite cache backend (the `cache` feature).
//!
//! A single-file [`CacheStore`] that survives restarts and is shared across
//! processes (a long-lived TUI and a one-shot CLI can use the same file). Uses
//! bundled SQLite (vendored C, statically linked — musl-safe), WAL journaling so
//! readers never block the writer, and `busy_timeout` to ride out cross-process
//! contention. Every operation is fail-soft: a cache error is logged and treated
//! as a miss / no-op so the cache can never take down a read.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::cache::store::{CacheEntry, CacheStore, UnixSecs};

/// SQLite integers are signed 64-bit; Unix-second timestamps fit comfortably, so
/// clamp rather than fail (a pre-1970 or post-year-292-billion cache is absurd).
fn to_sql(v: UnixSecs) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Inverse of [`to_sql`] for values read back out of the database.
fn from_sql(v: i64) -> UnixSecs {
    u64::try_from(v).unwrap_or(0)
}

/// An on-disk [`CacheStore`]. One mutex-guarded connection (a cache is a
/// single-writer workload; serializing in-process sidesteps `SQLITE_BUSY`, and
/// `busy_timeout` covers other processes).
pub struct SqliteStore {
    conn: Mutex<Connection>,
}

impl SqliteStore {
    /// Open (creating if absent) the cache database at `path`, applying the WAL
    /// pragmas and ensuring the schema. Errors if the directory or file can't be
    /// created — callers fall back to the in-memory store.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating cache dir {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening cache db {}", path.display()))?;
        // `auto_vacuum=INCREMENTAL` must be set before the table exists to take
        // effect on a fresh file; WAL + NORMAL + busy_timeout are the standard
        // local-cache tuning. STRICT enforces column types; WITHOUT ROWID suits a
        // pure composite-key table.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=5000;
             PRAGMA auto_vacuum=INCREMENTAL;
             CREATE TABLE IF NOT EXISTS cache (
                 profile    TEXT    NOT NULL,
                 key        TEXT    NOT NULL,
                 value      BLOB    NOT NULL,
                 fetched_at INTEGER NOT NULL,
                 expires_at INTEGER NOT NULL,
                 PRIMARY KEY (profile, key)
             ) STRICT, WITHOUT ROWID;
             CREATE INDEX IF NOT EXISTS idx_cache_expires ON cache(expires_at);",
        )
        .context("initializing cache schema")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

impl CacheStore for SqliteStore {
    fn get(&self, profile: &str, key: &str, now: UnixSecs) -> Option<CacheEntry> {
        let Ok(conn) = self.conn.lock() else {
            return None;
        };
        // Only return live rows. Expired rows are reclaimed by `sweep` / overwrite
        // rather than deleted on every read miss (which would write-amplify).
        let row = conn
            .query_row(
                "SELECT value, fetched_at, expires_at FROM cache \
                 WHERE profile=?1 AND key=?2 AND expires_at>?3",
                params![profile, key, to_sql(now)],
                |row| {
                    let fetched_at: i64 = row.get(1)?;
                    let expires_at: i64 = row.get(2)?;
                    Ok(CacheEntry {
                        bytes: row.get(0)?,
                        fetched_at: from_sql(fetched_at),
                        expires_at: from_sql(expires_at),
                    })
                },
            )
            .optional();
        match row {
            Ok(entry) => entry,
            Err(e) => {
                tracing::warn!("cache get failed: {e}");
                None
            }
        }
    }

    fn put(&self, profile: &str, key: &str, entry: &CacheEntry) {
        let Ok(conn) = self.conn.lock() else {
            return;
        };
        if let Err(e) = conn.execute(
            "INSERT INTO cache (profile, key, value, fetched_at, expires_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(profile, key) DO UPDATE SET \
                 value=excluded.value, \
                 fetched_at=excluded.fetched_at, \
                 expires_at=excluded.expires_at",
            params![
                profile,
                key,
                entry.bytes,
                to_sql(entry.fetched_at),
                to_sql(entry.expires_at)
            ],
        ) {
            tracing::warn!("cache put failed: {e}");
        }
    }

    fn invalidate(&self, profile: &str, key: &str) {
        let Ok(conn) = self.conn.lock() else {
            return;
        };
        if let Err(e) = conn.execute(
            "DELETE FROM cache WHERE profile=?1 AND key=?2",
            params![profile, key],
        ) {
            tracing::warn!("cache invalidate failed: {e}");
        }
    }

    fn clear(&self, profile: Option<&str>) {
        let Ok(conn) = self.conn.lock() else {
            return;
        };
        let result = match profile {
            Some(p) => conn.execute("DELETE FROM cache WHERE profile=?1", params![p]),
            None => conn.execute("DELETE FROM cache", []),
        };
        if let Err(e) = result {
            tracing::warn!("cache clear failed: {e}");
        }
    }

    fn sweep(&self, now: UnixSecs) {
        let Ok(conn) = self.conn.lock() else {
            return;
        };
        if let Err(e) = conn.execute(
            "DELETE FROM cache WHERE expires_at<=?1",
            params![to_sql(now)],
        ) {
            tracing::warn!("cache sweep failed: {e}");
            return;
        }
        // Reclaim freed pages cheaply (no-op unless auto_vacuum=INCREMENTAL).
        let _ = conn.execute_batch("PRAGMA incremental_vacuum;");
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

    fn store() -> (tempfile::TempDir, SqliteStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(&dir.path().join("cache.db")).unwrap();
        (dir, store)
    }

    #[test]
    fn put_then_get_roundtrips() {
        let (_dir, s) = store();
        s.put("p", "k", &entry("hi", 100, 200));
        let got = s.get("p", "k", 150).expect("live");
        assert_eq!(got.bytes, b"hi");
        assert_eq!(got.fetched_at, 100);
        assert_eq!(got.expires_at, 200);
    }

    #[test]
    fn get_filters_expired_rows() {
        let (_dir, s) = store();
        s.put("p", "k", &entry("hi", 100, 200));
        assert!(
            s.get("p", "k", 200).is_none(),
            "now == expires_at is expired"
        );
        assert!(s.get("p", "k", 999).is_none());
    }

    #[test]
    fn put_overwrites_on_conflict() {
        let (_dir, s) = store();
        s.put("p", "k", &entry("old", 100, 200));
        s.put("p", "k", &entry("new", 300, 400));
        let got = s.get("p", "k", 350).unwrap();
        assert_eq!(got.bytes, b"new");
        assert_eq!(got.fetched_at, 300);
    }

    #[test]
    fn survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.db");
        {
            let s = SqliteStore::open(&path).unwrap();
            s.put("p", "k", &entry("persisted", 100, 10_000));
        }
        // A new process / connection reads the on-disk entry — the flagship
        // cold-start / offline property.
        let reopened = SqliteStore::open(&path).unwrap();
        let got = reopened.get("p", "k", 200).expect("entry survived reopen");
        assert_eq!(got.bytes, b"persisted");
    }

    #[test]
    fn clear_and_invalidate_scope_correctly() {
        let (_dir, s) = store();
        s.put("a", "k1", &entry("a1", 0, 100));
        s.put("a", "k2", &entry("a2", 0, 100));
        s.put("b", "k1", &entry("b1", 0, 100));

        s.invalidate("a", "k1");
        assert!(s.get("a", "k1", 10).is_none());
        assert!(s.get("a", "k2", 10).is_some());

        s.clear(Some("a"));
        assert!(s.get("a", "k2", 10).is_none());
        assert!(s.get("b", "k1", 10).is_some(), "other profile untouched");

        s.clear(None);
        assert!(s.get("b", "k1", 10).is_none());
    }

    #[test]
    fn sweep_removes_expired() {
        let (_dir, s) = store();
        s.put("p", "live", &entry("live", 0, 100));
        s.put("p", "dead", &entry("dead", 0, 50));
        s.sweep(60);
        assert!(s.get("p", "live", 60).is_some());
        // Even a clock before expiry can't resurrect a swept row.
        assert!(s.get("p", "dead", 10).is_none());
    }
}
