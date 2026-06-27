//! Per-user NetBox credential vault (Pattern 2, DESIGN §24).
//!
//! The read-only MCP server (Pattern 3) attributes callers in the nbox audit
//! log but still uses the single profile token for the last hop to NetBox —
//! accountability, not per-user RBAC. That is acceptable for reads. **Writes**
//! require real per-user NetBox identity: the NetBox object-change record must
//! name the human, not the service account.
//!
//! The vault maps an OIDC `sub` (the validated caller identity from
//! [`crate::mcp::oidc::Identity`]) to a NetBox API token. Tokens are **never**
//! stored in `config.toml` alongside the service token — they live in
//! per-user env vars named in `[serve.vault]`. This keeps the credential
//! boundary explicit: the operator provisions one env var per NetBox user,
//! and the vault resolves `sub → env_var → token` at request time.
//!
//! Fails closed: a write from a caller with no vault entry is rejected with a
//! clear error naming the `sub` and the missing `[serve.vault]` mapping. The
//! service token is **never** used for writes — it remains the read-only
//! fallback for Pattern 3 reads.
//!
//! The resolved token is never logged (see [`ResolvedToken`]'s `Debug`).

use std::collections::BTreeMap;

/// The per-user credential vault, keyed by OIDC `sub`.
///
/// Built from `[serve.vault]` in `config.toml`:
///
/// ```toml
/// [serve.vault]
/// "alice@example.com" = { token_env = "NETBOX_TOKEN_ALICE" }
/// "bob@example.com"    = { token_env = "NETBOX_TOKEN_BOB" }
/// ```
///
/// At request time, [`resolve`](Self::resolve) looks up the caller's `sub` and
/// reads the token from the named env var. A missing env var is a hard error
/// (the operator mis-provisioned), not a silent fallthrough to the service
/// token.
#[derive(Clone, Default)]
pub struct CredentialVault {
    /// `sub → env var name`. Looked up at request time.
    entries: BTreeMap<String, VaultEntry>,
    /// Whether MCP writes are enabled at all (the `[serve].allow_writes` gate).
    /// When `false`, all write tools are rejected regardless of vault state.
    allow_writes: bool,
}

/// One vault entry: the env var holding a per-user NetBox token.
#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
pub struct VaultEntry {
    /// The env var name holding this user's NetBox API token. **Required** —
    /// tokens are never stored in config; only the env var name is.
    #[serde(default)]
    pub token_env: String,
}

/// A resolved per-user token, ready to swap into a [`NetBoxClient`].
///
/// `Debug` is hand-written so the token value is never printed: it renders as
/// `<redacted>`.
#[derive(Clone)]
pub struct ResolvedToken(String);

impl std::fmt::Debug for ResolvedToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ResolvedToken(<redacted>)")
    }
}

impl ResolvedToken {
    /// The raw token value, for building the `Authorization` header.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Why a vault lookup failed. Maps to an MCP `invalid_params` error so the
/// caller (agent/operator) can fix it.
#[derive(Debug)]
pub enum VaultError {
    /// Writes are not enabled on this `nbox serve` instance. The operator
    /// must set `[serve].allow_writes = true` (or `--allow-writes`).
    WritesDisabled,
    /// The caller's `sub` has no entry in the vault. The operator must add a
    /// `[serve.vault."<sub>"]` mapping.
    NoEntry { sub: String },
    /// The vault entry names an env var that is not set. The operator must
    /// provision it.
    EnvMissing { sub: String, env_var: String },
    /// The env var is set but normalizes to nothing (whitespace/empty). Same
    /// fix as `EnvMissing`.
    EnvBlank { sub: String, env_var: String },
}

impl std::fmt::Display for VaultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VaultError::WritesDisabled => write!(
                f,
                "MCP writes are not enabled on this nbox serve instance; \
                 set [serve].allow_writes = true or pass --allow-writes"
            ),
            VaultError::NoEntry { sub } => write!(
                f,
                "no vault entry for caller sub \"{sub}\"; \
                 add [serve.vault.\"{sub}\"] with a token_env mapping"
            ),
            VaultError::EnvMissing { sub, env_var } => write!(
                f,
                "vault entry for caller sub \"{sub}\" names env var \"{env_var}\" \
                 that is not set"
            ),
            VaultError::EnvBlank { sub, env_var } => write!(
                f,
                "vault entry for caller sub \"{sub}\" names env var \"{env_var}\" \
                 that resolves to an empty token"
            ),
        }
    }
}

impl std::error::Error for VaultError {}

impl CredentialVault {
    /// Build a vault from a config `[serve.vault]` map and the `allow_writes`
    /// gate flag.
    pub fn new(entries: BTreeMap<String, VaultEntry>, allow_writes: bool) -> Self {
        Self {
            entries,
            allow_writes,
        }
    }

    /// Whether MCP writes are enabled at all (the top-level gate).
    pub fn allow_writes(&self) -> bool {
        self.allow_writes
    }

    /// Resolve a caller's `sub` to a per-user NetBox token.
    ///
    /// Fails closed: `WritesDisabled` if the gate is off, `NoEntry` if the
    /// `sub` isn't in the vault, `EnvMissing`/`EnvBlank` if the env var is
    /// unprovisioned. Never falls back to the service token.
    pub fn resolve(&self, sub: &str) -> Result<ResolvedToken, VaultError> {
        if !self.allow_writes {
            return Err(VaultError::WritesDisabled);
        }
        let entry = self.entries.get(sub).ok_or_else(|| VaultError::NoEntry {
            sub: sub.to_string(),
        })?;
        let raw = std::env::var(&entry.token_env).map_err(|_| VaultError::EnvMissing {
            sub: sub.to_string(),
            env_var: entry.token_env.clone(),
        })?;
        let normalized =
            crate::config::normalize_token(&raw).ok_or_else(|| VaultError::EnvBlank {
                sub: sub.to_string(),
                env_var: entry.token_env.clone(),
            })?;
        Ok(ResolvedToken(normalized))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(env: &str) -> VaultEntry {
        VaultEntry {
            token_env: env.to_string(),
        }
    }

    fn vault_with(entries: &[(&str, &str)], allow: bool) -> CredentialVault {
        let map = entries
            .iter()
            .map(|(sub, env)| (sub.to_string(), entry(env)))
            .collect();
        CredentialVault::new(map, allow)
    }

    #[test]
    fn writes_disabled_rejects_even_with_entry() {
        // Even if a sub has an entry and the env var is set, writes are off →
        // the gate rejects before the vault lookup. The operator must
        // explicitly enable writes.
        unsafe {
            std::env::set_var("NBOX_VAULT_TEST_OK", "nbt_test_token");
        }
        let vault = vault_with(&[("alice", "NBOX_VAULT_TEST_OK")], false);
        let err = vault.resolve("alice").unwrap_err();
        assert!(matches!(err, VaultError::WritesDisabled));
        unsafe {
            std::env::remove_var("NBOX_VAULT_TEST_OK");
        }
    }

    #[test]
    fn resolves_sub_to_env_var_token() {
        unsafe {
            std::env::set_var("NBOX_VAULT_TEST_ALICE", "nbt_alice_secret");
        }
        let vault = vault_with(&[("alice", "NBOX_VAULT_TEST_ALICE")], true);
        let token = vault.resolve("alice").unwrap();
        assert_eq!(token.as_str(), "nbt_alice_secret");
        unsafe {
            std::env::remove_var("NBOX_VAULT_TEST_ALICE");
        }
    }

    #[test]
    fn no_entry_for_unknown_sub() {
        let vault = vault_with(&[("alice", "SOME_ENV")], true);
        let err = vault.resolve("bob").unwrap_err();
        assert!(matches!(err, VaultError::NoEntry { sub } if sub == "bob"));
    }

    #[test]
    fn env_missing_fails_closed() {
        // The env var is named in config but not set → hard error, not a
        // silent fallthrough.
        let vault = vault_with(&[("alice", "NBOX_VAULT_NEVER_SET")], true);
        let err = vault.resolve("alice").unwrap_err();
        assert!(matches!(
            err,
            VaultError::EnvMissing { sub, env_var }
            if sub == "alice" && env_var == "NBOX_VAULT_NEVER_SET"
        ));
    }

    #[test]
    fn blank_env_fails_closed() {
        unsafe {
            std::env::set_var("NBOX_VAULT_TEST_BLANK", "   ");
        }
        let vault = vault_with(&[("alice", "NBOX_VAULT_TEST_BLANK")], true);
        let err = vault.resolve("alice").unwrap_err();
        assert!(matches!(err, VaultError::EnvBlank { .. }));
        unsafe {
            std::env::remove_var("NBOX_VAULT_TEST_BLANK");
        }
    }

    #[test]
    fn strips_bearer_prefix_from_env_token() {
        // A pasted "Bearer nbt_..." in the env var normalizes to the bare key,
        // same as the CLI token resolution.
        unsafe {
            std::env::set_var("NBOX_VAULT_TEST_BEARER", "Bearer nbt_raw_token");
        }
        let vault = vault_with(&[("alice", "NBOX_VAULT_TEST_BEARER")], true);
        let token = vault.resolve("alice").unwrap();
        assert_eq!(token.as_str(), "nbt_raw_token");
        unsafe {
            std::env::remove_var("NBOX_VAULT_TEST_BEARER");
        }
    }

    #[test]
    fn resolved_token_debug_never_leaks_value() {
        unsafe {
            std::env::set_var("NBOX_VAULT_TEST_SECRET", "nbt_super_secret");
        }
        let vault = vault_with(&[("alice", "NBOX_VAULT_TEST_SECRET")], true);
        let token = vault.resolve("alice").unwrap();
        let debug = format!("{token:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("nbt_super_secret"));
        unsafe {
            std::env::remove_var("NBOX_VAULT_TEST_SECRET");
        }
    }

    #[test]
    fn empty_vault_rejects_all() {
        let vault = CredentialVault::new(BTreeMap::new(), true);
        let err = vault.resolve("anyone").unwrap_err();
        assert!(matches!(err, VaultError::NoEntry { sub } if sub == "anyone"));
    }
}
