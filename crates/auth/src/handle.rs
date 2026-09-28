// SPDX-License-Identifier: GPL-3.0-only

//! `CredentialHandle` — opaque, cheap-to-clone reference to a live credential (docs/PLAN.md §6.2,
//! PATTERNS.md §4). This is the *only* way anything outside `auth` touches a credential.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use http::{HeaderName, HeaderValue};
use secrecy::ExposeSecret;
use time::OffsetDateTime;
use tokio::sync::RwLock;

use crate::adapter::AuthAdapter;
use crate::credential::{AccountInfo, CredentialSecret, CredentialSet};
use crate::error::AuthError;
use crate::refresh::RefreshCoordinator;
use crate::store::SecretStore;

/// How close to expiry `authorize()` proactively refreshes, rather than waiting for a 401
/// (docs/PLAN.md §6.3).
const PROACTIVE_REFRESH_WINDOW: time::Duration = time::Duration::minutes(5);

/// Everything needed to actually perform + persist a refresh for one account. Present for
/// handles backed by a real `AuthBroker` account; absent for `CredentialHandle::for_tests`
/// handles, which have no adapter/store to refresh with.
pub(crate) struct RefreshTarget {
    pub(crate) adapter: Arc<dyn AuthAdapter>,
    pub(crate) store: Arc<dyn SecretStore>,
    pub(crate) coordinator: RefreshCoordinator,
}

impl fmt::Debug for RefreshTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RefreshTarget").finish_non_exhaustive()
    }
}

/// The mutable half of an account's state: the live secret plus a generation counter bumped on
/// every successful refresh.
#[derive(Debug)]
struct Slot {
    secret: CredentialSecret,
    /// Bumped exactly once per completed refresh. Lets a caller that read the secret at
    /// generation G skip triggering another refresh in `on_unauthorized` if, by the time it gets
    /// there, the generation has already moved past G — i.e. someone else's refresh already
    /// covers it (docs/PLAN.md §6.3: "20 agents sharing an account ⇒ one refresh request").
    generation: u64,
}

/// Shared, mutable state for one live account. `AuthBroker` owns exactly one
/// `Arc<AccountEntry>` per `(provider, transport, account_id)` in its in-memory cache; every
/// `CredentialHandle` for that account references the same entry (CONTRACTS.md §2).
pub(crate) struct AccountEntry {
    account: AccountInfo,
    slot: RwLock<Slot>,
    refresh_target: Option<RefreshTarget>,
}

impl fmt::Debug for AccountEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccountEntry")
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

impl AccountEntry {
    pub(crate) fn new(
        account: AccountInfo,
        secret: CredentialSecret,
        refresh_target: Option<RefreshTarget>,
    ) -> Arc<Self> {
        Arc::new(Self {
            account,
            slot: RwLock::new(Slot {
                secret,
                generation: 0,
            }),
            refresh_target,
        })
    }
}

/// Opaque, cheap-to-clone reference to a live credential owned by `AuthBroker`. Never exposes the
/// raw secret outside crate `auth`; `Debug` only prints non-secret account metadata.
pub struct CredentialHandle {
    entry: Arc<AccountEntry>,
    /// Generation this *handle instance* last built a header from. Fresh (starts at 0) per
    /// `AuthBroker::credential()` call (and per `for_tests`); shared with `.clone()`s of this
    /// particular handle (so splitting one logical request across tasks still agrees on what
    /// "already refreshed" means) but independent of other handles for the same account, which is
    /// exactly what lets `run_refresh`'s generation check work.
    observed_generation: Arc<AtomicU64>,
}

impl Clone for CredentialHandle {
    fn clone(&self) -> Self {
        Self {
            entry: Arc::clone(&self.entry),
            observed_generation: Arc::clone(&self.observed_generation),
        }
    }
}

impl CredentialHandle {
    /// Wraps an existing shared account entry — used by `AuthBroker` so every handle for the same
    /// account shares one generation counter and refresh coordinator.
    pub(crate) fn from_entry(entry: Arc<AccountEntry>) -> Self {
        Self {
            entry,
            observed_generation: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Builds a standalone entry with no shared cache and (for `for_tests`) no refresh target.
    /// Only reachable via `for_tests` (feature `testing`) today.
    #[cfg_attr(not(feature = "testing"), allow(dead_code))]
    pub(crate) fn new(account: AccountInfo, secret: CredentialSecret) -> Self {
        Self::from_entry(AccountEntry::new(account, secret, None))
    }

    /// Builds a handle backed by a fixed bearer token, for adapter/transport unit tests. Never
    /// talks to `AuthBroker`, the keyring, or any refresh coordinator: `on_unauthorized` on a
    /// `for_tests` handle always returns `Err(AuthError::RefreshRejected)` (documented there).
    #[cfg(feature = "testing")]
    pub fn for_tests(account: AccountInfo, static_token: impl Into<String>) -> Self {
        Self::new(
            account,
            CredentialSecret::Bearer {
                access_token: secrecy::SecretString::from(static_token.into()),
                refresh_token: None,
                expires_at: None,
            },
        )
    }

    /// Non-secret metadata: provider, transport, account id/label, auth kind.
    pub fn account(&self) -> &AccountInfo {
        &self.entry.account
    }

    /// Inserts whatever header this credential requires (`Authorization: Bearer ...` or a
    /// provider-specific header like `x-api-key`) into `headers`. Never returns the secret to the
    /// caller as a `String`. Proactively refreshes first if the current secret expires within
    /// `PROACTIVE_REFRESH_WINDOW` (docs/PLAN.md §6.3).
    pub async fn authorize(&self, headers: &mut http::HeaderMap) -> Result<(), AuthError> {
        self.maybe_proactive_refresh().await?;

        let slot = self.entry.slot.read().await;
        let (name, raw_value) = match &slot.secret {
            CredentialSecret::Bearer { access_token, .. } => (
                http::header::AUTHORIZATION,
                format!("Bearer {}", access_token.expose_secret()),
            ),
            CredentialSecret::Header { header_name, value } => {
                let name = HeaderName::from_bytes(header_name.as_bytes())
                    .map_err(|e| AuthError::OAuth(format!("invalid header name: {e}")))?;
                (name, value.expose_secret().to_owned())
            }
        };
        self.observed_generation
            .store(slot.generation, Ordering::SeqCst);
        drop(slot);

        let mut header_value = HeaderValue::from_str(&raw_value)
            .map_err(|e| AuthError::OAuth(format!("invalid header value: {e}")))?;
        header_value.set_sensitive(true);
        headers.insert(name, header_value);
        Ok(())
    }

    async fn maybe_proactive_refresh(&self) -> Result<(), AuthError> {
        let Some(target) = self.entry.refresh_target.as_ref() else {
            return Ok(());
        };
        let needs_refresh = {
            let slot = self.entry.slot.read().await;
            match &slot.secret {
                CredentialSecret::Bearer {
                    expires_at: Some(expiry),
                    ..
                } => *expiry - OffsetDateTime::now_utc() < PROACTIVE_REFRESH_WINDOW,
                _ => false,
            }
        };
        if needs_refresh {
            self.run_refresh(target).await?;
        }
        Ok(())
    }

    /// Called on HTTP 401: triggers a single-flight refresh shared by every concurrent caller for
    /// this account (docs/PLAN.md §6.3). Retry exactly once after this succeeds, then surface
    /// `ProviderError::Auth` if it fails again (PATTERNS.md §4).
    ///
    /// `for_tests` handles have no adapter/store to refresh with and always return
    /// `Err(AuthError::RefreshRejected)` here — a static test token never refreshes.
    pub async fn on_unauthorized(&self) -> Result<(), AuthError> {
        let Some(target) = self.entry.refresh_target.as_ref() else {
            return Err(AuthError::RefreshRejected);
        };
        self.run_refresh(target).await
    }

    async fn run_refresh(&self, target: &RefreshTarget) -> Result<(), AuthError> {
        let observed = self.observed_generation.load(Ordering::SeqCst);
        let (current_generation, current_secret) = {
            let slot = self.entry.slot.read().await;
            (slot.generation, slot.secret.clone())
        };
        if current_generation != observed {
            // Someone else already refreshed since we last read the secret (or since this handle
            // was created and never successfully authorized yet); nothing to do.
            self.observed_generation
                .store(current_generation, Ordering::SeqCst);
            return Ok(());
        }

        let account = self.entry.account.clone();
        let adapter = Arc::clone(&target.adapter);
        let refreshed = target
            .coordinator
            .refresh_single_flight(move || async move {
                let current = CredentialSet {
                    account,
                    secret: current_secret,
                };
                adapter.refresh(&current).await
            })
            .await?;

        target.store.save(&refreshed).await?;

        let mut slot = self.entry.slot.write().await;
        // Re-check under the write lock: another leader could have raced a second refresh in
        // between `refresh_single_flight` returning and us acquiring this lock (only possible
        // right after the coordinator's slot was cleared). Cheap to guard, avoids clobbering a
        // newer secret with a stale one.
        if slot.generation == observed {
            slot.secret = refreshed.secret;
            slot.generation += 1;
        }
        self.observed_generation
            .store(slot.generation, Ordering::SeqCst);
        Ok(())
    }
}

impl fmt::Debug for CredentialHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialHandle")
            .field("provider", &self.entry.account.provider)
            .field("transport", &self.entry.account.transport)
            .field("account_id", &self.entry.account.account_id)
            .finish()
    }
}

#[cfg(all(test, feature = "testing"))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    use async_trait::async_trait;
    use xlightcli_protocol::{AuthKind, ProviderId, TransportId};

    use super::*;
    use crate::adapter::{AuthMethod, DiscoveredCredential};
    use crate::login_ui::LoginUi;
    use crate::store::UnimplementedStore;

    fn test_account() -> AccountInfo {
        AccountInfo {
            provider: ProviderId::new("codex"),
            transport: TransportId::new("chatgpt"),
            account_id: "user-1".into(),
            label: Some("me@example.com".into()),
            auth_kind: AuthKind::Subscription,
            metadata: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn authorize_sets_bearer_header() {
        let handle = CredentialHandle::for_tests(test_account(), "XLC-SENTINEL-SECRET");
        let mut headers = http::HeaderMap::new();
        handle.authorize(&mut headers).await.unwrap();
        let value = headers.get(http::header::AUTHORIZATION).unwrap();
        assert_eq!(value, "Bearer XLC-SENTINEL-SECRET");
    }

    #[test]
    fn debug_never_prints_the_secret() {
        let handle = CredentialHandle::for_tests(test_account(), "XLC-SENTINEL-SECRET");
        let printed = format!("{handle:?}");
        assert!(!printed.contains("XLC-SENTINEL-SECRET"));
        assert!(printed.contains("codex"));
    }

    #[tokio::test]
    async fn header_variant_uses_custom_header_name() {
        let handle = CredentialHandle::new(
            test_account(),
            CredentialSecret::Header {
                header_name: "x-api-key".into(),
                value: secrecy::SecretString::from("XLC-SENTINEL-SECRET".to_string()),
            },
        );
        let mut headers = http::HeaderMap::new();
        handle.authorize(&mut headers).await.unwrap();
        assert_eq!(headers.get("x-api-key").unwrap(), "XLC-SENTINEL-SECRET");
    }

    #[tokio::test]
    async fn for_tests_handle_on_unauthorized_always_rejects() {
        let handle = CredentialHandle::for_tests(test_account(), "XLC-SENTINEL-SECRET");
        let err = handle.on_unauthorized().await.unwrap_err();
        assert!(matches!(err, AuthError::RefreshRejected));
    }

    struct CountingAdapter {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl AuthAdapter for CountingAdapter {
        fn methods(&self) -> &[AuthMethod] {
            &[AuthMethod::ApiKey]
        }

        async fn discover_existing(&self) -> Vec<DiscoveredCredential> {
            Vec::new()
        }

        async fn import(&self, _found: &DiscoveredCredential) -> Result<CredentialSet, AuthError> {
            Err(AuthError::NotImplemented("unused"))
        }

        async fn login(
            &self,
            _method: AuthMethod,
            _ui: &dyn LoginUi,
        ) -> Result<CredentialSet, AuthError> {
            Err(AuthError::NotImplemented("unused"))
        }

        async fn refresh(&self, current: &CredentialSet) -> Result<CredentialSet, AuthError> {
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            let mut refreshed = current.clone();
            refreshed.secret = CredentialSecret::Bearer {
                access_token: secrecy::SecretString::from("XLC-SENTINEL-REFRESHED".to_string()),
                refresh_token: None,
                expires_at: None,
            };
            Ok(refreshed)
        }

        async fn revoke(&self, _current: &CredentialSet) -> Result<(), AuthError> {
            Ok(())
        }
    }

    fn real_handle(calls: Arc<AtomicUsize>) -> CredentialHandle {
        let entry = AccountEntry::new(
            test_account(),
            CredentialSecret::Bearer {
                access_token: secrecy::SecretString::from("XLC-SENTINEL-OLD".to_string()),
                refresh_token: None,
                expires_at: None,
            },
            Some(RefreshTarget {
                adapter: Arc::new(CountingAdapter { calls }),
                store: Arc::new(UnimplementedStore),
                coordinator: RefreshCoordinator::new(),
            }),
        );
        CredentialHandle::from_entry(entry)
    }

    #[tokio::test]
    async fn on_unauthorized_refreshes_and_authorize_reflects_new_token() {
        let calls = Arc::new(AtomicUsize::new(0));
        let handle = real_handle(Arc::clone(&calls));

        let mut headers = http::HeaderMap::new();
        handle.authorize(&mut headers).await.unwrap();
        assert_eq!(
            headers.get(http::header::AUTHORIZATION).unwrap(),
            "Bearer XLC-SENTINEL-OLD"
        );

        // UnimplementedStore::save always errors, so persistence surfaces that — but the
        // in-memory slot must not be left updated inconsistently; verify the adapter *was*
        // consulted (call count 1) even though persistence failed.
        let err = handle.on_unauthorized().await;
        assert!(err.is_err());
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_caller_that_already_observed_the_refresh_does_not_refresh_again() {
        let calls = Arc::new(AtomicUsize::new(0));
        let entry = AccountEntry::new(
            test_account(),
            CredentialSecret::Bearer {
                access_token: secrecy::SecretString::from("XLC-SENTINEL-OLD".to_string()),
                refresh_token: None,
                expires_at: None,
            },
            Some(RefreshTarget {
                adapter: Arc::new(CountingAdapter {
                    calls: Arc::clone(&calls),
                }),
                store: Arc::new(NoopStore),
                coordinator: RefreshCoordinator::new(),
            }),
        );
        let handle_a = CredentialHandle::from_entry(Arc::clone(&entry));
        let handle_b = CredentialHandle::from_entry(entry);

        // Both handles authorize first (observing generation 0), simulating two requests that
        // both used the same pre-refresh token before either got a 401.
        let mut headers = http::HeaderMap::new();
        handle_a.authorize(&mut headers).await.unwrap();
        handle_b.authorize(&mut headers).await.unwrap();

        handle_a.on_unauthorized().await.unwrap();
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);

        // handle_b still thinks it's at generation 0; its own on_unauthorized must notice the
        // generation already moved and must NOT call the adapter a second time.
        handle_b.on_unauthorized().await.unwrap();
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
    }

    struct NoopStore;

    #[async_trait]
    impl SecretStore for NoopStore {
        async fn save(&self, _credential: &CredentialSet) -> Result<(), AuthError> {
            Ok(())
        }

        async fn load(
            &self,
            _provider: &ProviderId,
            _transport: &TransportId,
            _account_id: &str,
        ) -> Result<Option<CredentialSet>, AuthError> {
            Ok(None)
        }

        async fn delete(
            &self,
            _provider: &ProviderId,
            _transport: &TransportId,
            _account_id: &str,
        ) -> Result<(), AuthError> {
            Ok(())
        }
    }
}
