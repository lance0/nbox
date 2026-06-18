//! OS keyring access for the NetBox API token.
//!
//! The token is a secret: it is **never** written to `config.toml`, never logged,
//! and never `Debug`-printed. `nbox config token set` stores it here instead, and
//! [`crate::config::resolve_token`] reads it as the lowest-precedence source
//! (after env vars). The account key is namespaced by config path + profile name
//! so two configs (or two profiles) can't collide on one secret.
//!
//! Behind one cfg-free public API:
//! - real impl under `#[cfg(feature = "keyring")]`,
//! - a no-op shim under `#[cfg(not(feature = "keyring"))]`.
//!
//! Every call returns cleanly when the keystore is missing or mock — never a
//! panic. [`keyring_available`] tells callers whether a *persistent* OS keystore
//! is actually compiled in for this target, so the CLI can print the right
//! guidance ("set NBOX_TOKEN or a token_env instead") rather than silently
//! storing into a throwaway in-process mock.

/// The keyring service name nbox stores its credentials under. Only referenced by
/// the real backend impl; the no-op shim (feature off) never touches it.
#[cfg(feature = "keyring")]
const SERVICE: &str = "nbox";

/// Build the per-(config, profile) keyring account key. Namespacing by the
/// resolved config path keeps two configs — or `--config <tmp>` runs — from
/// clobbering one another's stored token; the profile name keeps profiles within
/// one config distinct.
///
/// Collision-safe: a plain `{path}::{profile}` join is ambiguous — a config path
/// ending in `::x` with profile `y` collides with path `…` + profile `x::y`. We
/// length-prefix the config path (`<len>:<path>::<profile>`) so the split point is
/// unambiguous regardless of which side contains `::`.
#[must_use]
pub fn account_key(config_path: &str, profile_name: &str) -> String {
    let len = config_path.len();
    format!("{len}:{config_path}::{profile_name}")
}

/// Whether a *persistent* OS keystore is compiled in for this target.
///
/// True only when a real backend is active: macOS/iOS with `apple-native`,
/// Windows with `windows-native`, or Linux/BSD with the Secret Service backend
/// (the off-by-default `keyring-secret-service` feature). Otherwise keyring v3
/// falls back to an in-process **mock** store — `set` "succeeds" but nothing
/// persists across processes — so we report it as unavailable and steer callers
/// to an env var. Also `false` when the `keyring` feature is off entirely.
#[must_use]
pub const fn keyring_available() -> bool {
    #[cfg(feature = "keyring")]
    {
        // Mirror keyring v3's own backend selection (see its `lib.rs`): a real,
        // persistent store is active only under these target/feature combos.
        cfg!(all(target_os = "macos", feature = "keyring")) // apple-native via our `keyring` feature
            || cfg!(all(target_os = "ios", feature = "keyring"))
            || cfg!(all(target_os = "windows", feature = "keyring"))
            || cfg!(all(
                any(target_os = "linux", target_os = "freebsd", target_os = "openbsd"),
                feature = "keyring-secret-service",
            ))
    }
    #[cfg(not(feature = "keyring"))]
    {
        false
    }
}

/// Read the token for `account` from the OS keyring, or `None` when absent /
/// unavailable. Never panics; a missing entry, an unusable keystore, or the
/// feature being off all yield `None` so token resolution simply falls through.
#[must_use]
pub fn keyring_get(account: &str) -> Option<String> {
    #[cfg(feature = "keyring")]
    {
        if !keyring_available() {
            return None;
        }
        let entry = match keyring::Entry::new(SERVICE, account) {
            Ok(e) => e,
            Err(e) => {
                tracing::debug!("keyring open failed: {e}");
                return None;
            }
        };
        match entry.get_password() {
            // An empty stored value is treated as "no token" — an empty string
            // would otherwise flow through `resolve_token` and produce a confusing
            // 401 instead of a clean "no token from any source".
            Ok(token) => (!token.is_empty()).then_some(token),
            // A missing entry is the normal "no token here" case (silent). A real
            // backend failure (locked keystore, D-Bus error) is logged at debug so
            // it's diagnosable, but still returns None so the UI falls through (L5).
            Err(keyring::Error::NoEntry) => None,
            Err(e) => {
                tracing::debug!("keyring read failed: {e}");
                None
            }
        }
    }
    #[cfg(not(feature = "keyring"))]
    {
        let _ = account;
        None
    }
}

/// Store `token` for `account` in the OS keyring.
///
/// Returns [`anyhow::Error`] when no persistent keystore is available (the mock /
/// missing-backend / feature-off cases) so the CLI can print env-var guidance and
/// exit non-zero, rather than appearing to succeed against a throwaway store.
pub fn keyring_set(account: &str, token: &str) -> anyhow::Result<()> {
    // Reject an empty token outright: storing `""` would round-trip as a token
    // that `keyring_get` then drops to `None` (a silent no-op for the caller).
    // Callers that mean "no token" should `keyring_delete` instead.
    if token.is_empty() {
        anyhow::bail!("refusing to store an empty token");
    }
    #[cfg(feature = "keyring")]
    {
        if !keyring_available() {
            anyhow::bail!("keyring unavailable");
        }
        let entry = keyring::Entry::new(SERVICE, account)
            .map_err(|e| anyhow::anyhow!("keyring open failed: {e}"))?;
        entry
            .set_password(token)
            .map_err(|e| anyhow::anyhow!("keyring write failed: {e}"))?;
        Ok(())
    }
    #[cfg(not(feature = "keyring"))]
    {
        let _ = (account, token);
        anyhow::bail!("keyring unavailable")
    }
}

/// Delete the stored token for `account`. A missing entry is treated as success
/// (idempotent clear). Errors only on a genuine keystore failure or when no
/// persistent keystore is available.
pub fn keyring_delete(account: &str) -> anyhow::Result<()> {
    #[cfg(feature = "keyring")]
    {
        if !keyring_available() {
            anyhow::bail!("keyring unavailable");
        }
        let entry = keyring::Entry::new(SERVICE, account)
            .map_err(|e| anyhow::anyhow!("keyring open failed: {e}"))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(anyhow::anyhow!("keyring delete failed: {e}")),
        }
    }
    #[cfg(not(feature = "keyring"))]
    {
        let _ = account;
        anyhow::bail!("keyring unavailable")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_key_namespaces_by_config_and_profile() {
        // Length-prefixed so the path/profile split is unambiguous.
        assert_eq!(
            account_key("/home/u/.config/nbox/config.toml", "work"),
            "32:/home/u/.config/nbox/config.toml::work"
        );
        // Same profile name under two configs must not collide.
        assert_ne!(
            account_key("/a/config.toml", "default"),
            account_key("/b/config.toml", "default")
        );
    }

    #[test]
    fn account_key_is_collision_safe_across_the_separator() {
        // A `{path}::{profile}` join is ambiguous: path "/a::x" + profile "y" and
        // path "/a" + profile "x::y" would both render "/a::x::y". The length
        // prefix disambiguates them.
        assert_ne!(
            account_key("/a::x", "y"),
            account_key("/a", "x::y"),
            "the path/profile boundary must be unambiguous"
        );
        // A path that itself ends in `::` doesn't collide with a longer path.
        assert_ne!(account_key("/a::", "b"), account_key("/a", "::b"));
    }

    #[test]
    fn get_never_panics_and_is_none_when_unavailable() {
        // On the mock/feature-off path this is None; on a real keystore it's a
        // miss for a key we never wrote. Either way: no panic, no token.
        let key = account_key("/nbox/test/never-written", "no-such-profile");
        assert!(keyring_get(&key).is_none());
    }

    #[test]
    fn set_and_delete_error_cleanly_when_keyring_unavailable() {
        // When no persistent backend is compiled in (the common CI/musl/Linux
        // case and the feature-off shim), set/delete report unavailability rather
        // than panicking or silently using the throwaway mock.
        if !keyring_available() {
            let key = account_key("/nbox/test/unavailable", "p");
            assert!(keyring_set(&key, "secret").is_err());
            assert!(keyring_delete(&key).is_err());
        }
    }

    #[test]
    fn set_rejects_an_empty_token_regardless_of_backend() {
        // An empty token is rejected before any backend call, so this holds whether
        // or not a persistent keystore is compiled in.
        let key = account_key("/nbox/test/empty", "p");
        assert!(keyring_set(&key, "").is_err());
    }

    #[test]
    fn available_is_false_without_a_real_backend() {
        // This crate's default features select no Linux Secret Service backend, so
        // on Linux CI `keyring_available()` must be false (mock keystore). The
        // assertion is target-scoped so macOS/Windows dev boxes (real backends)
        // don't trip it.
        #[cfg(all(
            any(target_os = "linux", target_os = "freebsd", target_os = "openbsd"),
            not(feature = "keyring-secret-service")
        ))]
        assert!(!keyring_available());
    }
}
