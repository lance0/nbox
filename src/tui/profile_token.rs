//! Transactional keyring changes for profile saves.
//!
//! Profile metadata and profile tokens live in different stores (`config.toml`
//! and the OS keyring). A save that changes both has to prepare the keyring
//! mutation before writing the file, then roll it back if the file write fails.
//! That avoids the two bad half-states: a profile that says it saved but has no
//! token, or a token orphaned under a profile that never landed in the config.

use std::path::Path;

use anyhow::Result;

use crate::tui::config_modal::TokenAction;

/// Abstracts the keyring for focused transaction tests.
pub(crate) trait SecretOps {
    fn get(&self, account: &str) -> Result<Option<String>>;
    fn set(&self, account: &str, token: &str) -> Result<()>;
    fn delete(&self, account: &str) -> Result<()>;
}

pub(crate) struct RealSecretOps;

impl SecretOps for RealSecretOps {
    fn get(&self, account: &str) -> Result<Option<String>> {
        crate::secret::keyring_get_checked(account)
    }

    fn set(&self, account: &str, token: &str) -> Result<()> {
        crate::secret::keyring_set(account, token)
    }

    fn delete(&self, account: &str) -> Result<()> {
        crate::secret::keyring_delete(account)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenNotice {
    Cleared,
    MigrationSkipped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SecretSnapshot {
    key: String,
    value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PreparedKind {
    None,
    Notice(TokenNotice),
    Set {
        new_key: String,
        old_key: Option<String>,
        previous_new: Option<String>,
    },
    Clear {
        snapshots: Vec<SecretSnapshot>,
    },
    RenameKeep {
        new_key: String,
        old_key: String,
        previous_new: Option<String>,
    },
}

/// A keyring mutation that has already been applied far enough for the config
/// write to proceed. Call `commit` after the TOML write succeeds, or `rollback`
/// after it fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedTokenChange {
    kind: PreparedKind,
}

impl PreparedTokenChange {
    pub(crate) fn prepare(
        path: &Path,
        original: Option<&str>,
        name: &str,
        action: &TokenAction,
    ) -> Result<Self> {
        Self::prepare_with_ops(
            &RealSecretOps,
            &path.display().to_string(),
            original,
            name,
            action,
        )
    }

    fn prepare_with_ops<O: SecretOps>(
        ops: &O,
        path: &str,
        original: Option<&str>,
        name: &str,
        action: &TokenAction,
    ) -> Result<Self> {
        let new_key = crate::secret::account_key(path, name);
        let old_key = original
            .filter(|orig| *orig != name)
            .map(|orig| crate::secret::account_key(path, orig));

        match action {
            TokenAction::Set(token) => Self::prepare_set(ops, new_key, old_key, token),
            TokenAction::Clear => Self::prepare_clear(ops, new_key, old_key),
            TokenAction::Keep => Ok(Self::prepare_keep(ops, new_key, old_key)),
        }
    }

    fn prepare_set<O: SecretOps>(
        ops: &O,
        new_key: String,
        old_key: Option<String>,
        token: &str,
    ) -> Result<Self> {
        let previous_new = ops
            .get(&new_key)
            .map_err(|e| anyhow::anyhow!("could not read existing stored token ({e:#})"))?;
        if let Err(e) = ops.set(&new_key, token) {
            restore_snapshot(
                ops,
                &SecretSnapshot {
                    key: new_key.clone(),
                    value: previous_new.clone(),
                },
            );
            anyhow::bail!(
                "pasted token was NOT stored ({e:#}); switch token_store to config or use token_env/NBOX_TOKEN"
            );
        }
        Ok(Self {
            kind: PreparedKind::Set {
                new_key,
                old_key,
                previous_new,
            },
        })
    }

    fn prepare_clear<O: SecretOps>(
        ops: &O,
        new_key: String,
        old_key: Option<String>,
    ) -> Result<Self> {
        let mut snapshots = Vec::with_capacity(if old_key.is_some() { 2 } else { 1 });
        let mut keys = vec![new_key];
        if let Some(old) = old_key
            && !keys.iter().any(|key| key == &old)
        {
            keys.push(old);
        }

        for key in keys {
            let snapshot = SecretSnapshot {
                value: ops.get(&key).map_err(|e| {
                    anyhow::anyhow!("could not read stored token before clear ({e:#})")
                })?,
                key,
            };
            if let Err(e) = ops.delete(&snapshot.key) {
                rollback_snapshots(ops, &snapshots);
                anyhow::bail!(
                    "stored token was NOT cleared ({e:#}); try again or clear it outside nbox"
                );
            }
            snapshots.push(snapshot);
        }

        Ok(Self {
            kind: PreparedKind::Clear { snapshots },
        })
    }

    fn prepare_keep<O: SecretOps>(ops: &O, new_key: String, old_key: Option<String>) -> Self {
        let Some(old_key) = old_key else {
            return Self {
                kind: PreparedKind::None,
            };
        };
        let existing = match ops.get(&old_key) {
            Ok(Some(existing)) => existing,
            Ok(None) => {
                return Self {
                    kind: PreparedKind::None,
                };
            }
            Err(e) => {
                tracing::debug!("could not read stored token before rename: {e:#}");
                return Self {
                    kind: PreparedKind::Notice(TokenNotice::MigrationSkipped),
                };
            }
        };
        let previous_new = match ops.get(&new_key) {
            Ok(value) => value,
            Err(e) => {
                tracing::debug!("could not read destination stored token before rename: {e:#}");
                return Self {
                    kind: PreparedKind::Notice(TokenNotice::MigrationSkipped),
                };
            }
        };
        if let Err(e) = ops.set(&new_key, &existing) {
            restore_snapshot(
                ops,
                &SecretSnapshot {
                    key: new_key.clone(),
                    value: previous_new.clone(),
                },
            );
            tracing::debug!("stored token was not migrated to the new profile name: {e:#}");
            return Self {
                kind: PreparedKind::Notice(TokenNotice::MigrationSkipped),
            };
        }
        Self {
            kind: PreparedKind::RenameKeep {
                new_key,
                old_key,
                previous_new,
            },
        }
    }

    pub(crate) fn stored_token(&self) -> bool {
        matches!(self.kind, PreparedKind::Set { .. })
    }

    pub(crate) fn commit(self) -> Option<TokenNotice> {
        self.commit_with_ops(&RealSecretOps)
    }

    fn commit_with_ops<O: SecretOps>(self, ops: &O) -> Option<TokenNotice> {
        match self.kind {
            PreparedKind::None => None,
            PreparedKind::Notice(notice) => Some(notice),
            PreparedKind::Set { old_key, .. } => {
                if let Some(old_key) = old_key
                    && let Err(e) = ops.delete(&old_key)
                {
                    tracing::debug!("failed to delete old keyring entry after profile save: {e:#}");
                }
                None
            }
            PreparedKind::RenameKeep { old_key, .. } => {
                if let Err(e) = ops.delete(&old_key) {
                    tracing::debug!("failed to delete old keyring entry after profile save: {e:#}");
                }
                None
            }
            PreparedKind::Clear { .. } => Some(TokenNotice::Cleared),
        }
    }

    pub(crate) fn rollback(&self) {
        self.rollback_with_ops(&RealSecretOps);
    }

    fn rollback_with_ops<O: SecretOps>(&self, ops: &O) {
        match &self.kind {
            PreparedKind::None | PreparedKind::Notice(_) => {}
            PreparedKind::Set {
                new_key,
                previous_new,
                ..
            }
            | PreparedKind::RenameKeep {
                new_key,
                previous_new,
                ..
            } => {
                restore_snapshot(
                    ops,
                    &SecretSnapshot {
                        key: new_key.clone(),
                        value: previous_new.clone(),
                    },
                );
            }
            PreparedKind::Clear { snapshots } => rollback_snapshots(ops, snapshots),
        }
    }
}

fn rollback_snapshots<O: SecretOps>(ops: &O, snapshots: &[SecretSnapshot]) {
    for snapshot in snapshots.iter().rev() {
        restore_snapshot(ops, snapshot);
    }
}

fn restore_snapshot<O: SecretOps>(ops: &O, snapshot: &SecretSnapshot) {
    let result = match &snapshot.value {
        Some(token) => ops.set(&snapshot.key, token),
        None => ops.delete(&snapshot.key),
    };
    if let Err(e) = result {
        tracing::debug!("failed to restore keyring entry during profile-save rollback: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::{HashMap, HashSet};

    use super::*;

    #[derive(Default)]
    struct FakeSecretOps {
        entries: RefCell<HashMap<String, String>>,
        fail_get: RefCell<HashSet<String>>,
        fail_set: RefCell<HashSet<String>>,
        fail_delete: RefCell<HashSet<String>>,
    }

    impl FakeSecretOps {
        fn key(path: &str, name: &str) -> String {
            crate::secret::account_key(path, name)
        }

        fn insert(&self, path: &str, name: &str, token: &str) {
            self.entries
                .borrow_mut()
                .insert(Self::key(path, name), token.to_string());
        }

        fn get_entry(&self, path: &str, name: &str) -> Option<String> {
            self.entries.borrow().get(&Self::key(path, name)).cloned()
        }
    }

    impl SecretOps for FakeSecretOps {
        fn get(&self, account: &str) -> Result<Option<String>> {
            if self.fail_get.borrow().contains(account) {
                anyhow::bail!("injected get failure");
            }
            Ok(self.entries.borrow().get(account).cloned())
        }

        fn set(&self, account: &str, token: &str) -> Result<()> {
            if self.fail_set.borrow().contains(account) {
                anyhow::bail!("injected set failure");
            }
            self.entries
                .borrow_mut()
                .insert(account.to_string(), token.to_string());
            Ok(())
        }

        fn delete(&self, account: &str) -> Result<()> {
            if self.fail_delete.borrow().contains(account) {
                anyhow::bail!("injected delete failure");
            }
            self.entries.borrow_mut().remove(account);
            Ok(())
        }
    }

    #[test]
    fn set_restores_previous_secret_on_rollback() {
        let ops = FakeSecretOps::default();
        let path = "/tmp/nbox/config.toml";
        ops.insert(path, "lab", "old-token");

        let prepared = PreparedTokenChange::prepare_with_ops(
            &ops,
            path,
            Some("lab"),
            "lab",
            &TokenAction::Set("new-token".to_string()),
        )
        .unwrap();
        assert_eq!(ops.get_entry(path, "lab").as_deref(), Some("new-token"));

        prepared.rollback_with_ops(&ops);
        assert_eq!(ops.get_entry(path, "lab").as_deref(), Some("old-token"));
    }

    #[test]
    fn set_deletes_new_secret_on_add_rollback() {
        let ops = FakeSecretOps::default();
        let path = "/tmp/nbox/config.toml";

        let prepared = PreparedTokenChange::prepare_with_ops(
            &ops,
            path,
            None,
            "lab",
            &TokenAction::Set("new-token".to_string()),
        )
        .unwrap();
        assert_eq!(ops.get_entry(path, "lab").as_deref(), Some("new-token"));

        prepared.rollback_with_ops(&ops);
        assert_eq!(ops.get_entry(path, "lab"), None);
    }

    #[test]
    fn clear_restores_deleted_secret_on_rollback() {
        let ops = FakeSecretOps::default();
        let path = "/tmp/nbox/config.toml";
        ops.insert(path, "lab", "old-token");

        let prepared = PreparedTokenChange::prepare_with_ops(
            &ops,
            path,
            Some("lab"),
            "lab",
            &TokenAction::Clear,
        )
        .unwrap();
        assert_eq!(ops.get_entry(path, "lab"), None);

        prepared.rollback_with_ops(&ops);
        assert_eq!(ops.get_entry(path, "lab").as_deref(), Some("old-token"));
    }

    #[test]
    fn rename_keep_copies_then_commits_old_key_delete() {
        let ops = FakeSecretOps::default();
        let path = "/tmp/nbox/config.toml";
        ops.insert(path, "old", "old-token");

        let prepared = PreparedTokenChange::prepare_with_ops(
            &ops,
            path,
            Some("old"),
            "new",
            &TokenAction::Keep,
        )
        .unwrap();
        assert_eq!(ops.get_entry(path, "old").as_deref(), Some("old-token"));
        assert_eq!(ops.get_entry(path, "new").as_deref(), Some("old-token"));

        assert_eq!(prepared.commit_with_ops(&ops), None);
        assert_eq!(ops.get_entry(path, "old"), None);
        assert_eq!(ops.get_entry(path, "new").as_deref(), Some("old-token"));
    }

    #[test]
    fn rename_keep_restores_destination_on_rollback() {
        let ops = FakeSecretOps::default();
        let path = "/tmp/nbox/config.toml";
        ops.insert(path, "old", "old-token");
        ops.insert(path, "new", "existing-new-token");

        let prepared = PreparedTokenChange::prepare_with_ops(
            &ops,
            path,
            Some("old"),
            "new",
            &TokenAction::Keep,
        )
        .unwrap();
        assert_eq!(ops.get_entry(path, "new").as_deref(), Some("old-token"));

        prepared.rollback_with_ops(&ops);
        assert_eq!(ops.get_entry(path, "old").as_deref(), Some("old-token"));
        assert_eq!(
            ops.get_entry(path, "new").as_deref(),
            Some("existing-new-token")
        );
    }

    #[test]
    fn rename_keep_get_failure_is_best_effort_warning() {
        let ops = FakeSecretOps::default();
        let path = "/tmp/nbox/config.toml";
        ops.fail_get
            .borrow_mut()
            .insert(FakeSecretOps::key(path, "old"));

        let prepared = PreparedTokenChange::prepare_with_ops(
            &ops,
            path,
            Some("old"),
            "new",
            &TokenAction::Keep,
        )
        .unwrap();

        assert_eq!(
            prepared.commit_with_ops(&ops),
            Some(TokenNotice::MigrationSkipped)
        );
        assert_eq!(ops.get_entry(path, "new"), None);
    }

    #[test]
    fn prepare_set_failure_keeps_previous_secret() {
        let ops = FakeSecretOps::default();
        let path = "/tmp/nbox/config.toml";
        ops.insert(path, "lab", "old-token");
        ops.fail_set
            .borrow_mut()
            .insert(FakeSecretOps::key(path, "lab"));

        let err = PreparedTokenChange::prepare_with_ops(
            &ops,
            path,
            Some("lab"),
            "lab",
            &TokenAction::Set("new-token".to_string()),
        )
        .unwrap_err();

        assert!(err.to_string().contains("pasted token was NOT stored"));
        assert_eq!(ops.get_entry(path, "lab").as_deref(), Some("old-token"));
    }
}
