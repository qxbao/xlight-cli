// SPDX-License-Identifier: GPL-3.0-only

//! `AccountIndex` — metadata-only account registry backing `xlightcli-auth` (docs/PLAN.md §6.1,
//! §11.2; CODEBASE.md §2).
//!
//! No secret ever lives here (INV-4): `keyring_ref` is only a lookup key that
//! `xlightcli_auth::store::SecretStore` uses to find the real credential in the OS keyring / file
//! store. Losing or leaking this table leaks *which accounts exist*, never a token.
//!
//! **Phase 0 / Wave 2 scope:** a single `rusqlite::Connection` behind a `std::sync::Mutex`, with
//! each call hopping onto a blocking thread via `tokio::task::spawn_blocking`. This is *not* yet
//! the persistence worker thread + bounded command channel described in PATTERNS.md §10 — that
//! full writer-thread architecture (shared by events/messages/artifacts too) lands in Phase 1. For
//! the single `accounts` table and the call volumes involved in login/refresh, a mutex-guarded
//! blocking connection is simple and correct; it is not meant to survive as-is once the event log
//! is added.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rusqlite::{OptionalExtension, params};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use xlightcli_protocol::{AuthKind, ProviderId, TransportId};

use crate::db;
use crate::error::StorageError;

/// Metadata for one stored account. Mirrors the `accounts` table (docs/PLAN.md §11.2) minus the
/// surrogate id, `is_default`, `created_at`/`updated_at` bookkeeping columns, which `AccountIndex`
/// manages internally.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountRecord {
    pub provider: ProviderId,
    pub transport: TransportId,
    /// Upstream account identifier (opaque: email, user id, org+user, ...).
    pub account_id: String,
    pub label: Option<String>,
    pub auth_kind: AuthKind,
    pub expiry: Option<OffsetDateTime>,
    /// Lookup key for `SecretStore` — never a secret itself.
    pub keyring_ref: String,
    /// Provider-specific extra metadata (e.g. plan name); never a secret (enforced by convention,
    /// not by type — adapters must not put tokens here).
    pub metadata: serde_json::Value,
}

/// Async wrapper around a sqlite-backed account index. Cheap to clone (shares the connection);
/// safe to share across tasks.
#[derive(Clone)]
pub struct AccountIndex {
    conn: Arc<Mutex<rusqlite::Connection>>,
}

impl std::fmt::Debug for AccountIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountIndex").finish_non_exhaustive()
    }
}

impl AccountIndex {
    /// Opens (creating if needed) the sqlite database at `path` and applies pending migrations.
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let path = path.into();
        let conn = tokio::task::spawn_blocking(move || db::open_and_migrate(&path)).await??;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    #[cfg(test)]
    async fn open_in_memory() -> Result<Self, StorageError> {
        let conn = tokio::task::spawn_blocking(db::open_in_memory_and_migrate).await??;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Inserts a new account or updates the mutable fields of an existing one (matched by
    /// `(provider, transport, account_id)`). Never touches `is_default` — use `set_default`.
    pub async fn upsert(&self, record: AccountRecord) -> Result<(), StorageError> {
        self.with_conn(move |conn| {
            let now = now_rfc3339();
            let expiry = record
                .expiry
                .map(|e| e.format(&Rfc3339))
                .transpose()
                .map_err(|e| StorageError::InvalidStoredData(format!("expiry: {e}")))?;
            conn.execute(
                "INSERT INTO accounts
                    (provider, transport, account_id, label, auth_kind, expiry, keyring_ref,
                     metadata_json, is_default, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9, ?9)
                 ON CONFLICT (provider, transport, account_id) DO UPDATE SET
                    label = excluded.label,
                    auth_kind = excluded.auth_kind,
                    expiry = excluded.expiry,
                    keyring_ref = excluded.keyring_ref,
                    metadata_json = excluded.metadata_json,
                    updated_at = excluded.updated_at",
                params![
                    record.provider.as_str(),
                    record.transport.as_str(),
                    record.account_id,
                    record.label,
                    auth_kind_to_str(record.auth_kind),
                    expiry,
                    record.keyring_ref,
                    record.metadata.to_string(),
                    now,
                ],
            )?;
            Ok(())
        })
        .await
    }

    /// Lists accounts, optionally filtered to one provider.
    pub async fn list(
        &self,
        provider: Option<ProviderId>,
    ) -> Result<Vec<AccountRecord>, StorageError> {
        self.with_conn(move |conn| match &provider {
            Some(provider) => {
                let mut stmt = conn.prepare(
                    "SELECT provider, transport, account_id, label, auth_kind, expiry,
                            keyring_ref, metadata_json
                     FROM accounts WHERE provider = ?1
                     ORDER BY provider, transport, account_id",
                )?;
                let rows = stmt
                    .query_map(params![provider.as_str()], row_to_record)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows.into_iter().collect()
            }
            None => {
                let mut stmt = conn.prepare(
                    "SELECT provider, transport, account_id, label, auth_kind, expiry,
                            keyring_ref, metadata_json
                     FROM accounts
                     ORDER BY provider, transport, account_id",
                )?;
                let rows = stmt
                    .query_map([], row_to_record)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows.into_iter().collect()
            }
        })
        .await
    }

    /// Looks up a single account by its natural key.
    pub async fn get(
        &self,
        provider: &ProviderId,
        transport: &TransportId,
        account_id: &str,
    ) -> Result<Option<AccountRecord>, StorageError> {
        let provider = provider.clone();
        let transport = transport.clone();
        let account_id = account_id.to_owned();
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT provider, transport, account_id, label, auth_kind, expiry,
                        keyring_ref, metadata_json
                 FROM accounts WHERE provider = ?1 AND transport = ?2 AND account_id = ?3",
                params![provider.as_str(), transport.as_str(), account_id],
                row_to_record,
            )
            .optional()?
            .transpose()
        })
        .await
    }

    /// Marks `(provider, transport, account_id)` as the default account for that
    /// `(provider, transport)` pair, clearing the flag on every other account for the same pair.
    /// Errors with `AccountNotFound` if no such account exists.
    pub async fn set_default(
        &self,
        provider: &ProviderId,
        transport: &TransportId,
        account_id: &str,
    ) -> Result<(), StorageError> {
        let provider = provider.clone();
        let transport = transport.clone();
        let account_id = account_id.to_owned();
        self.with_conn(move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "UPDATE accounts SET is_default = 0 WHERE provider = ?1 AND transport = ?2",
                params![provider.as_str(), transport.as_str()],
            )?;
            let updated = tx.execute(
                "UPDATE accounts SET is_default = 1
                 WHERE provider = ?1 AND transport = ?2 AND account_id = ?3",
                params![provider.as_str(), transport.as_str(), account_id],
            )?;
            if updated == 0 {
                return Err(StorageError::AccountNotFound {
                    provider: provider.into_string(),
                    transport: transport.into_string(),
                    account_id,
                });
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Returns the account currently marked default for `(provider, transport)`, if any.
    pub async fn get_default(
        &self,
        provider: &ProviderId,
        transport: &TransportId,
    ) -> Result<Option<AccountRecord>, StorageError> {
        let provider = provider.clone();
        let transport = transport.clone();
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT provider, transport, account_id, label, auth_kind, expiry,
                        keyring_ref, metadata_json
                 FROM accounts WHERE provider = ?1 AND transport = ?2 AND is_default = 1",
                params![provider.as_str(), transport.as_str()],
                row_to_record,
            )
            .optional()?
            .transpose()
        })
        .await
    }

    /// Deletes an account. No-op (`Ok(())`) if it doesn't exist — callers that need to know
    /// whether something was actually removed should `get` first.
    pub async fn delete(
        &self,
        provider: &ProviderId,
        transport: &TransportId,
        account_id: &str,
    ) -> Result<(), StorageError> {
        let provider = provider.clone();
        let transport = transport.clone();
        let account_id = account_id.to_owned();
        self.with_conn(move |conn| {
            conn.execute(
                "DELETE FROM accounts WHERE provider = ?1 AND transport = ?2 AND account_id = ?3",
                params![provider.as_str(), transport.as_str(), account_id],
            )?;
            Ok(())
        })
        .await
    }

    /// Runs `f` against the connection on a blocking thread, mapping a panicked/cancelled task
    /// into `StorageError::Join`. The mutex is `std::sync::Mutex` (not `tokio::sync::Mutex`)
    /// because it is only ever locked from inside blocking closures, never across an `.await`.
    async fn with_conn<T, F>(&self, f: F) -> Result<T, StorageError>
    where
        T: Send + 'static,
        F: FnOnce(&mut rusqlite::Connection) -> Result<T, StorageError> + Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            // A poisoned mutex means a previous access panicked mid-transaction; sqlite itself
            // has no in-memory state to corrupt beyond what's already on disk, so recovering the
            // guard and continuing is safe and preferable to poisoning every future call.
            let mut guard = conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            f(&mut guard)
        })
        .await?
    }
}

fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<AccountRecord, StorageError>> {
    Ok((|| {
        let provider: String = row.get(0)?;
        let transport: String = row.get(1)?;
        let account_id: String = row.get(2)?;
        let label: Option<String> = row.get(3)?;
        let auth_kind: String = row.get(4)?;
        let expiry: Option<String> = row.get(5)?;
        let keyring_ref: String = row.get(6)?;
        let metadata_json: String = row.get(7)?;

        let auth_kind = auth_kind_from_str(&auth_kind)?;
        let expiry = expiry
            .map(|e| OffsetDateTime::parse(&e, &Rfc3339))
            .transpose()
            .map_err(|e| StorageError::InvalidStoredData(format!("expiry: {e}")))?;
        let metadata = serde_json::from_str(&metadata_json)
            .map_err(|e| StorageError::InvalidStoredData(format!("metadata_json: {e}")))?;

        Ok(AccountRecord {
            provider: ProviderId::new(provider),
            transport: TransportId::new(transport),
            account_id,
            label,
            auth_kind,
            expiry,
            keyring_ref,
            metadata,
        })
    })())
}

fn auth_kind_to_str(kind: AuthKind) -> &'static str {
    match kind {
        AuthKind::Subscription => "subscription",
        AuthKind::ApiKey => "api_key",
    }
}

fn auth_kind_from_str(s: &str) -> Result<AuthKind, StorageError> {
    match s {
        "subscription" => Ok(AuthKind::Subscription),
        "api_key" => Ok(AuthKind::ApiKey),
        other => Err(StorageError::InvalidStoredData(format!(
            "unknown auth_kind {other:?}"
        ))),
    }
}

pub(crate) fn now_rfc3339() -> String {
    // `OffsetDateTime::now_utc().format(&Rfc3339)` cannot fail for a valid `OffsetDateTime`
    // (the only failure mode is a component out of range, which `now_utc()` never produces) —
    // falling back to an empty string just avoids an `expect()` in a non-test path.
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    fn sample(provider: &str, transport: &str, account_id: &str) -> AccountRecord {
        AccountRecord {
            provider: ProviderId::new(provider),
            transport: TransportId::new(transport),
            account_id: account_id.to_owned(),
            label: Some("me@example.com".into()),
            auth_kind: AuthKind::Subscription,
            expiry: Some(OffsetDateTime::now_utc()),
            keyring_ref: format!("{provider}:{transport}:{account_id}"),
            metadata: json!({"plan": "pro"}),
        }
    }

    #[tokio::test]
    async fn upsert_then_get_roundtrips() {
        let index = AccountIndex::open_in_memory().await.unwrap();
        let record = sample("codex", "chatgpt", "acc-1");
        index.upsert(record.clone()).await.unwrap();

        let fetched = index
            .get(&record.provider, &record.transport, &record.account_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fetched.account_id, "acc-1");
        assert_eq!(fetched.label.as_deref(), Some("me@example.com"));
        assert_eq!(fetched.metadata, json!({"plan": "pro"}));
    }

    #[tokio::test]
    async fn get_missing_account_is_none() {
        let index = AccountIndex::open_in_memory().await.unwrap();
        let found = index
            .get(
                &ProviderId::new("codex"),
                &TransportId::new("chatgpt"),
                "nope",
            )
            .await
            .unwrap();
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn upsert_is_idempotent_and_updates_mutable_fields() {
        let index = AccountIndex::open_in_memory().await.unwrap();
        let mut record = sample("codex", "chatgpt", "acc-1");
        index.upsert(record.clone()).await.unwrap();

        record.label = Some("renamed@example.com".into());
        index.upsert(record.clone()).await.unwrap();

        let all = index.list(None).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].label.as_deref(), Some("renamed@example.com"));
    }

    #[tokio::test]
    async fn list_filters_by_provider() {
        let index = AccountIndex::open_in_memory().await.unwrap();
        index
            .upsert(sample("codex", "chatgpt", "acc-1"))
            .await
            .unwrap();
        index
            .upsert(sample("claude", "anthropic-api", "acc-2"))
            .await
            .unwrap();

        let codex_only = index.list(Some(ProviderId::new("codex"))).await.unwrap();
        assert_eq!(codex_only.len(), 1);
        assert_eq!(codex_only[0].provider.as_str(), "codex");

        let all = index.list(None).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn set_default_is_exclusive_per_provider_transport() {
        let index = AccountIndex::open_in_memory().await.unwrap();
        let provider = ProviderId::new("codex");
        let transport = TransportId::new("chatgpt");
        index
            .upsert(sample("codex", "chatgpt", "acc-1"))
            .await
            .unwrap();
        index
            .upsert(sample("codex", "chatgpt", "acc-2"))
            .await
            .unwrap();

        index
            .set_default(&provider, &transport, "acc-1")
            .await
            .unwrap();
        assert_eq!(
            index
                .get_default(&provider, &transport)
                .await
                .unwrap()
                .unwrap()
                .account_id,
            "acc-1"
        );

        index
            .set_default(&provider, &transport, "acc-2")
            .await
            .unwrap();
        assert_eq!(
            index
                .get_default(&provider, &transport)
                .await
                .unwrap()
                .unwrap()
                .account_id,
            "acc-2"
        );
    }

    #[tokio::test]
    async fn set_default_on_missing_account_errors() {
        let index = AccountIndex::open_in_memory().await.unwrap();
        let err = index
            .set_default(
                &ProviderId::new("codex"),
                &TransportId::new("chatgpt"),
                "nope",
            )
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::AccountNotFound { .. }));
    }

    #[tokio::test]
    async fn delete_removes_account() {
        let index = AccountIndex::open_in_memory().await.unwrap();
        let record = sample("codex", "chatgpt", "acc-1");
        index.upsert(record.clone()).await.unwrap();
        index
            .delete(&record.provider, &record.transport, &record.account_id)
            .await
            .unwrap();
        assert!(
            index
                .get(&record.provider, &record.transport, &record.account_id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn open_creates_parent_directories_and_persists_across_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("nested").join("accounts.db");
        let record = sample("codex", "chatgpt", "acc-1");

        {
            let index = AccountIndex::open(db_path.clone()).await.unwrap();
            index.upsert(record.clone()).await.unwrap();
        }
        {
            let index = AccountIndex::open(db_path.clone()).await.unwrap();
            let fetched = index
                .get(&record.provider, &record.transport, &record.account_id)
                .await
                .unwrap();
            assert!(fetched.is_some());
        }
    }
}
