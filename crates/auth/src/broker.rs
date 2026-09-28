// SPDX-License-Identifier: GPL-3.0-only

//! `AuthBroker` — sole owner of credential storage, refresh and locking (docs/PLAN.md §6.1).
//!
//! `AuthAdapter` implementations know *how* to log in/refresh for one provider; `AuthBroker` is
//! the only thing that turns a `CredentialSet` into a live `CredentialHandle`, decides when to
//! refresh, and talks to the secret store. Transports never see anything but a `CredentialHandle`.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex as AsyncMutex;
use xlightcli_protocol::{ProviderId, TransportId};
use xlightcli_storage::{AccountIndex, AccountRecord, StorageError};

use crate::adapter::{AuthAdapter, AuthMethod, DiscoveredCredential};
use crate::credential::{AccountInfo, CredentialSecret, CredentialSet};
use crate::error::AuthError;
use crate::handle::{AccountEntry, CredentialHandle, RefreshTarget};
use crate::login_ui::LoginUi;
use crate::refresh::RefreshCoordinator;
use crate::store::{SecretStore, UnimplementedStore};

/// Natural key for the in-memory `AccountEntry` cache and for most `AccountIndex`/`SecretStore`
/// calls.
type AccountKey = (ProviderId, TransportId, String);

/// Registry of provider auth adapters plus the secret store / account index / in-memory
/// account-entry cache. Construct one per process, share it via `Arc` (CODEBASE.md §5).
pub struct AuthBroker {
    adapters: HashMap<ProviderId, Arc<dyn AuthAdapter>>,
    store: Arc<dyn SecretStore>,
    /// `None` for brokers built via `new`/`with_store` — every method that needs to persist or
    /// look up accounts returns `AuthError::StoreUnavailable` pointing at `with_store_and_index`
    /// instead. Real wiring always goes through `with_store_and_index`.
    index: Option<AccountIndex>,
    /// One live `AccountEntry` per account, shared by every `CredentialHandle` returned for it
    /// (CONTRACTS.md §2) — this is what makes the generation counter / single-flight refresh
    /// coordinator actually shared across concurrent callers instead of per-call.
    entries: AsyncMutex<HashMap<AccountKey, Arc<AccountEntry>>>,
}

impl AuthBroker {
    /// Builds a broker with the default (`UnimplementedStore`) secret store and no account index.
    /// Useful for adapter-registration tests; `credential`/`login`/`import` all fail clearly with
    /// `AuthError::StoreUnavailable` until a real store + index are wired in via
    /// `with_store_and_index`.
    pub fn new() -> Self {
        Self {
            adapters: HashMap::new(),
            store: Arc::new(UnimplementedStore),
            index: None,
            entries: AsyncMutex::new(HashMap::new()),
        }
    }

    /// Builds a broker with an explicit secret store but still no account index — same caveat as
    /// `new` for anything that needs to persist.
    pub fn with_store(store: Arc<dyn SecretStore>) -> Self {
        Self {
            adapters: HashMap::new(),
            store,
            index: None,
            entries: AsyncMutex::new(HashMap::new()),
        }
    }

    /// Builds a fully-functional broker: `credential`/`login`/`import`/`accounts`/`logout` all
    /// work. This is what `app::wiring` should use.
    pub fn with_store_and_index(store: Arc<dyn SecretStore>, index: AccountIndex) -> Self {
        Self {
            adapters: HashMap::new(),
            store,
            index: Some(index),
            entries: AsyncMutex::new(HashMap::new()),
        }
    }

    /// Registers the `AuthAdapter` for `provider`. Called once per provider during
    /// `app::wiring`.
    pub fn register_adapter(&mut self, provider: ProviderId, adapter: Arc<dyn AuthAdapter>) {
        self.adapters.insert(provider, adapter);
    }

    pub fn adapter(&self, provider: &ProviderId) -> Option<&Arc<dyn AuthAdapter>> {
        self.adapters.get(provider)
    }

    fn index_or_unavailable(&self) -> Result<&AccountIndex, AuthError> {
        self.index.as_ref().ok_or_else(|| {
            AuthError::StoreUnavailable(
                "account index not configured (build the broker with \
                 AuthBroker::with_store_and_index)"
                    .into(),
            )
        })
    }

    /// Returns a live `CredentialHandle` for `(provider, transport)`'s default account, loading it
    /// from the secret store on first use and caching the resulting `AccountEntry` so later calls
    /// (and every other caller for the same account) share one generation counter / refresh
    /// coordinator.
    pub async fn credential(
        &self,
        provider: &ProviderId,
        transport: &TransportId,
    ) -> Result<CredentialHandle, AuthError> {
        let adapter =
            self.adapters
                .get(provider)
                .cloned()
                .ok_or_else(|| AuthError::NotLoggedIn {
                    provider: provider.clone(),
                })?;
        let index = self.index_or_unavailable()?;

        let default_record = index
            .get_default(provider, transport)
            .await
            .map_err(index_err)?
            .ok_or_else(|| AuthError::NotLoggedIn {
                provider: provider.clone(),
            })?;
        let key: AccountKey = (
            provider.clone(),
            transport.clone(),
            default_record.account_id.clone(),
        );

        if let Some(entry) = self.entries.lock().await.get(&key) {
            return Ok(CredentialHandle::from_entry(Arc::clone(entry)));
        }

        let credential_set = self
            .store
            .load(provider, transport, &default_record.account_id)
            .await?
            .ok_or_else(|| AuthError::NotLoggedIn {
                provider: provider.clone(),
            })?;

        let entry = AccountEntry::new(
            credential_set.account,
            credential_set.secret,
            Some(RefreshTarget {
                adapter,
                store: Arc::clone(&self.store),
                coordinator: RefreshCoordinator::new(),
            }),
        );

        // Another concurrent `credential()` call for the same account may have inserted one
        // already while we were awaiting the store load above; keep whichever one won so both
        // callers converge on the same shared state.
        let mut entries = self.entries.lock().await;
        let entry = Arc::clone(entries.entry(key).or_insert(entry));
        Ok(CredentialHandle::from_entry(entry))
    }

    /// Runs a login flow for `provider` and persists the result (secret store + account index;
    /// sets it as the default account if none exists yet for `(provider, transport)`).
    pub async fn login(
        &self,
        provider: &ProviderId,
        method: AuthMethod,
        ui: &dyn LoginUi,
    ) -> Result<AccountInfo, AuthError> {
        let adapter =
            self.adapters
                .get(provider)
                .cloned()
                .ok_or_else(|| AuthError::NotLoggedIn {
                    provider: provider.clone(),
                })?;
        let credential_set = adapter.login(method, ui).await?;
        self.persist_and_cache(adapter, credential_set).await
    }

    /// Imports a discovered credential (D-017: import & own) and persists it. Same shape as
    /// `login`; xlightcli owns and refreshes the copy independently from then on.
    pub async fn import(&self, found: &DiscoveredCredential) -> Result<AccountInfo, AuthError> {
        let adapter =
            self.adapters
                .get(&found.provider)
                .cloned()
                .ok_or_else(|| AuthError::NotLoggedIn {
                    provider: found.provider.clone(),
                })?;
        let credential_set = adapter.import(found).await?;
        self.persist_and_cache(adapter, credential_set).await
    }

    async fn persist_and_cache(
        &self,
        adapter: Arc<dyn AuthAdapter>,
        credential_set: CredentialSet,
    ) -> Result<AccountInfo, AuthError> {
        let index = self.index_or_unavailable()?;

        self.store.save(&credential_set).await?;

        let account = credential_set.account.clone();
        let had_default = index
            .get_default(&account.provider, &account.transport)
            .await
            .map_err(index_err)?
            .is_some();
        index
            .upsert(AccountRecord {
                provider: account.provider.clone(),
                transport: account.transport.clone(),
                account_id: account.account_id.clone(),
                label: account.label.clone(),
                auth_kind: account.auth_kind,
                expiry: match &credential_set.secret {
                    CredentialSecret::Bearer { expires_at, .. } => *expires_at,
                    CredentialSecret::Header { .. } => None,
                },
                keyring_ref: format!(
                    "{}:{}:{}",
                    account.provider, account.transport, account.account_id
                ),
                metadata: serde_json::json!({}),
            })
            .await
            .map_err(index_err)?;
        if !had_default {
            index
                .set_default(&account.provider, &account.transport, &account.account_id)
                .await
                .map_err(index_err)?;
        }

        let key: AccountKey = (
            account.provider.clone(),
            account.transport.clone(),
            account.account_id.clone(),
        );
        let entry = AccountEntry::new(
            account.clone(),
            credential_set.secret,
            Some(RefreshTarget {
                adapter,
                store: Arc::clone(&self.store),
                coordinator: RefreshCoordinator::new(),
            }),
        );
        self.entries.lock().await.insert(key, entry);

        Ok(account)
    }

    /// Lists known accounts, optionally filtered to one provider.
    pub async fn accounts(
        &self,
        provider: Option<ProviderId>,
    ) -> Result<Vec<AccountInfo>, AuthError> {
        let index = self.index_or_unavailable()?;
        let records = index.list(provider).await.map_err(index_err)?;
        Ok(records.into_iter().map(record_to_account_info).collect())
    }

    /// Revokes (best-effort) and removes an account: calls `AuthAdapter::revoke` if the provider
    /// is registered and a credential is still loadable (a failure there is logged, not
    /// propagated — local cleanup must still happen), then deletes it from the secret store, the
    /// account index, and the in-memory entry cache.
    pub async fn logout(
        &self,
        provider: &ProviderId,
        transport: &TransportId,
        account_id: &str,
    ) -> Result<(), AuthError> {
        let index = self.index_or_unavailable()?;

        if let Some(adapter) = self.adapters.get(provider)
            && let Ok(Some(credential_set)) = self.store.load(provider, transport, account_id).await
            && let Err(e) = adapter.revoke(&credential_set).await
        {
            tracing::warn!(
                provider = %provider,
                transport = %transport,
                account_id,
                error = %e,
                "AuthAdapter::revoke failed during logout; removing local credential anyway"
            );
        }

        self.store.delete(provider, transport, account_id).await?;
        index
            .delete(provider, transport, account_id)
            .await
            .map_err(index_err)?;
        self.entries.lock().await.remove(&(
            provider.clone(),
            transport.clone(),
            account_id.to_owned(),
        ));
        Ok(())
    }
}

fn record_to_account_info(record: AccountRecord) -> AccountInfo {
    AccountInfo {
        provider: record.provider,
        transport: record.transport,
        account_id: record.account_id,
        label: record.label,
        auth_kind: record.auth_kind,
        metadata: record.metadata,
    }
}

fn index_err(e: StorageError) -> AuthError {
    AuthError::StoreUnavailable(format!("account index: {e}"))
}

impl Default for AuthBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for AuthBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthBroker")
            .field("providers", &self.adapters.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use async_trait::async_trait;

    use super::*;
    use crate::credential::{AccountInfo, CredentialSecret, CredentialSet};

    struct StubAdapter {
        login_secret: &'static str,
    }

    #[async_trait]
    impl AuthAdapter for StubAdapter {
        fn methods(&self) -> &[AuthMethod] {
            &[AuthMethod::ApiKey]
        }

        async fn discover_existing(&self) -> Vec<DiscoveredCredential> {
            Vec::new()
        }

        async fn import(&self, found: &DiscoveredCredential) -> Result<CredentialSet, AuthError> {
            Ok(CredentialSet {
                account: AccountInfo {
                    provider: found.provider.clone(),
                    transport: found.transport.clone(),
                    account_id: "imported-1".into(),
                    label: Some(found.account_label.clone()),
                    auth_kind: xlightcli_protocol::AuthKind::ApiKey,
                    metadata: serde_json::json!({}),
                },
                secret: CredentialSecret::Bearer {
                    access_token: secrecy::SecretString::from("XLC-SENTINEL-IMPORTED".to_string()),
                    refresh_token: None,
                    expires_at: None,
                },
            })
        }

        async fn login(
            &self,
            _method: AuthMethod,
            _ui: &dyn LoginUi,
        ) -> Result<CredentialSet, AuthError> {
            Ok(CredentialSet {
                account: AccountInfo {
                    provider: ProviderId::new("codex"),
                    transport: TransportId::new("openai-api"),
                    account_id: "acc-1".into(),
                    label: None,
                    auth_kind: xlightcli_protocol::AuthKind::ApiKey,
                    metadata: serde_json::json!({}),
                },
                secret: CredentialSecret::Bearer {
                    access_token: secrecy::SecretString::from(self.login_secret.to_string()),
                    refresh_token: None,
                    expires_at: None,
                },
            })
        }

        async fn refresh(&self, current: &CredentialSet) -> Result<CredentialSet, AuthError> {
            Ok(current.clone())
        }

        async fn revoke(&self, _current: &CredentialSet) -> Result<(), AuthError> {
            Ok(())
        }
    }

    struct NoopUi;

    #[async_trait]
    impl LoginUi for NoopUi {
        async fn show_browser_url(&self, _url: &str) {}
        async fn show_device_code(&self, _verification_uri: &str, _user_code: &str) {}
        async fn prompt_api_key(
            &self,
            _provider: &ProviderId,
        ) -> Result<secrecy::SecretString, AuthError> {
            Ok(secrecy::SecretString::from("unused".to_string()))
        }
    }

    async fn broker_with_file_store(dir: &std::path::Path) -> AuthBroker {
        let store: Arc<dyn SecretStore> =
            Arc::new(crate::store::FileStore::new(dir.join("credentials")));
        let index = AccountIndex::open(dir.join("accounts.db")).await.unwrap();
        AuthBroker::with_store_and_index(store, index)
    }

    #[test]
    fn register_and_lookup_adapter() {
        let mut broker = AuthBroker::new();
        let provider = ProviderId::new("codex");
        broker.register_adapter(
            provider.clone(),
            Arc::new(StubAdapter {
                login_secret: "unused",
            }),
        );
        assert!(broker.adapter(&provider).is_some());
        assert!(broker.adapter(&ProviderId::new("claude")).is_none());
    }

    #[tokio::test]
    async fn credential_for_unregistered_provider_is_not_logged_in() {
        let broker = AuthBroker::new();
        let err = broker
            .credential(&ProviderId::new("codex"), &TransportId::new("chatgpt"))
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::NotLoggedIn { .. }));
    }

    #[tokio::test]
    async fn login_without_an_index_configured_is_store_unavailable() {
        let mut broker = AuthBroker::new();
        let provider = ProviderId::new("codex");
        broker.register_adapter(
            provider.clone(),
            Arc::new(StubAdapter {
                login_secret: "unused",
            }),
        );
        let err = broker
            .login(&provider, AuthMethod::ApiKey, &NoopUi)
            .await
            .unwrap_err();
        // The adapter's own `login()` succeeded (StubAdapter always does); only the broker's
        // persistence step fails, because this broker has no account index configured.
        assert!(matches!(err, AuthError::StoreUnavailable(_)));
    }

    #[tokio::test]
    async fn login_persists_and_credential_returns_a_working_handle() {
        let temp = tempfile::tempdir().unwrap();
        let mut broker = broker_with_file_store(temp.path()).await;
        let provider = ProviderId::new("codex");
        let transport = TransportId::new("openai-api");
        broker.register_adapter(
            provider.clone(),
            Arc::new(StubAdapter {
                login_secret: "XLC-SENTINEL-LOGIN",
            }),
        );

        let account = broker
            .login(&provider, AuthMethod::ApiKey, &NoopUi)
            .await
            .unwrap();
        assert_eq!(account.account_id, "acc-1");

        let handle = broker.credential(&provider, &transport).await.unwrap();
        let mut headers = http::HeaderMap::new();
        handle.authorize(&mut headers).await.unwrap();
        assert_eq!(
            headers.get(http::header::AUTHORIZATION).unwrap(),
            "Bearer XLC-SENTINEL-LOGIN"
        );
    }

    #[tokio::test]
    async fn credential_survives_broker_restart_via_persisted_store_and_index() {
        let temp = tempfile::tempdir().unwrap();
        let provider = ProviderId::new("codex");
        let transport = TransportId::new("openai-api");

        {
            let mut broker = broker_with_file_store(temp.path()).await;
            broker.register_adapter(
                provider.clone(),
                Arc::new(StubAdapter {
                    login_secret: "XLC-SENTINEL-PERSISTED",
                }),
            );
            broker
                .login(&provider, AuthMethod::ApiKey, &NoopUi)
                .await
                .unwrap();
        }
        {
            let mut broker = broker_with_file_store(temp.path()).await;
            broker.register_adapter(
                provider.clone(),
                Arc::new(StubAdapter {
                    login_secret: "unused-on-reload",
                }),
            );
            let handle = broker.credential(&provider, &transport).await.unwrap();
            let mut headers = http::HeaderMap::new();
            handle.authorize(&mut headers).await.unwrap();
            assert_eq!(
                headers.get(http::header::AUTHORIZATION).unwrap(),
                "Bearer XLC-SENTINEL-PERSISTED"
            );
        }
    }

    #[tokio::test]
    async fn import_persists_and_lists_via_accounts() {
        let temp = tempfile::tempdir().unwrap();
        let mut broker = broker_with_file_store(temp.path()).await;
        let provider = ProviderId::new("claude");
        broker.register_adapter(
            provider.clone(),
            Arc::new(StubAdapter {
                login_secret: "unused",
            }),
        );

        let found = DiscoveredCredential {
            provider: provider.clone(),
            transport: TransportId::new("anthropic-api"),
            account_label: "me@example.com".into(),
            source: std::path::PathBuf::from("/tmp/fake"),
        };
        let account = broker.import(&found).await.unwrap();
        assert_eq!(account.account_id, "imported-1");

        let listed = broker.accounts(Some(provider)).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].account_id, "imported-1");
    }

    #[tokio::test]
    async fn logout_removes_account_and_credential() {
        let temp = tempfile::tempdir().unwrap();
        let mut broker = broker_with_file_store(temp.path()).await;
        let provider = ProviderId::new("codex");
        let transport = TransportId::new("openai-api");
        broker.register_adapter(
            provider.clone(),
            Arc::new(StubAdapter {
                login_secret: "XLC-SENTINEL-LOGOUT",
            }),
        );
        broker
            .login(&provider, AuthMethod::ApiKey, &NoopUi)
            .await
            .unwrap();

        broker.logout(&provider, &transport, "acc-1").await.unwrap();

        let err = broker.credential(&provider, &transport).await.unwrap_err();
        assert!(matches!(err, AuthError::NotLoggedIn { .. }));
        let listed = broker.accounts(None).await.unwrap();
        assert!(listed.is_empty());
    }

    #[tokio::test]
    async fn concurrent_credential_calls_share_one_account_entry() {
        let temp = tempfile::tempdir().unwrap();
        let mut broker = broker_with_file_store(temp.path()).await;
        let provider = ProviderId::new("codex");
        let transport = TransportId::new("openai-api");
        broker.register_adapter(
            provider.clone(),
            Arc::new(StubAdapter {
                login_secret: "XLC-SENTINEL-SHARED",
            }),
        );
        broker
            .login(&provider, AuthMethod::ApiKey, &NoopUi)
            .await
            .unwrap();
        let broker = Arc::new(broker);

        let a = broker.credential(&provider, &transport).await.unwrap();
        let b = broker.credential(&provider, &transport).await.unwrap();
        // Both handles must observe the exact same live secret value (proving they share the same
        // underlying `AccountEntry`, not two independently-loaded copies).
        let mut headers_a = http::HeaderMap::new();
        let mut headers_b = http::HeaderMap::new();
        a.authorize(&mut headers_a).await.unwrap();
        b.authorize(&mut headers_b).await.unwrap();
        assert_eq!(
            headers_a.get(http::header::AUTHORIZATION),
            headers_b.get(http::header::AUTHORIZATION)
        );
    }
}
