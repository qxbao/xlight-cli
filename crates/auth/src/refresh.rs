// SPDX-License-Identifier: GPL-3.0-only

//! Single-flight refresh coordination (PATTERNS.md §3, docs/PLAN.md §6.3): if N tasks hit 401 for
//! the same account concurrently, exactly one refresh request should be issued; the rest await
//! its result.
//!
//! This module only implements the generic "run this closure at most once concurrently" part.
//! The domain-specific half — deciding *whether* a refresh is even still needed (the generation
//! counter that stops a caller who's already looking at a freshly-refreshed token from kicking
//! off a redundant one) — lives in `handle::AccountEntry` / `CredentialHandle::run_refresh`, which
//! calls `refresh_single_flight` only after that check says a real refresh is required.

use std::future::Future;

use futures::future::{BoxFuture, FutureExt, Shared};
use tokio::sync::Mutex;

use crate::credential::CredentialSet;
use crate::error::AuthError;

type InFlight = Shared<BoxFuture<'static, Result<CredentialSet, AuthError>>>;

/// Coordinates refresh calls for one account. One instance lives inside each account's
/// `handle::AccountEntry` (`handle::RefreshTarget`), so it's already scoped per-account —
/// coordinating across *different* accounts is not this type's job.
pub struct RefreshCoordinator {
    inflight: Mutex<Option<InFlight>>,
}

impl std::fmt::Debug for RefreshCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RefreshCoordinator").finish_non_exhaustive()
    }
}

impl Default for RefreshCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl RefreshCoordinator {
    pub fn new() -> Self {
        Self {
            inflight: Mutex::new(None),
        }
    }

    /// Runs `refresh` at most once even under concurrent callers; late callers observe the
    /// in-flight attempt's result instead of issuing a second upstream request.
    ///
    /// Implementation: the first caller to arrive (finding no in-flight attempt) becomes the
    /// "leader" — it installs its `refresh()` future (boxed + made `Shared` so its `Result` can be
    /// cloned to every waiter) into `inflight` and awaits it directly. Every other caller that
    /// arrives while that slot is occupied just clones the same `Shared` future and awaits it too,
    /// without ever calling its own `refresh` closure. Once the leader's future resolves, the
    /// leader (not the followers, to avoid a clear-vs-install race) clears the slot, so the next
    /// caller starts a fresh attempt rather than replaying a stale cached result forever.
    pub async fn refresh_single_flight<F, Fut>(
        &self,
        refresh: F,
    ) -> Result<CredentialSet, AuthError>
    where
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<CredentialSet, AuthError>> + Send + 'static,
    {
        let mut guard = self.inflight.lock().await;
        if let Some(existing) = guard.as_ref() {
            let shared = existing.clone();
            drop(guard);
            return shared.await;
        }
        let shared: InFlight = refresh().boxed().shared();
        *guard = Some(shared.clone());
        drop(guard);

        let result = shared.await;

        let mut guard = self.inflight.lock().await;
        *guard = None;
        drop(guard);

        result
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use xlightcli_protocol::{AuthKind, ProviderId, TransportId};

    use super::*;
    use crate::credential::{AccountInfo, CredentialSecret};

    fn sample_set() -> CredentialSet {
        CredentialSet {
            account: AccountInfo {
                provider: ProviderId::new("codex"),
                transport: TransportId::new("chatgpt"),
                account_id: "acc-1".into(),
                label: None,
                auth_kind: AuthKind::Subscription,
                metadata: serde_json::json!({}),
            },
            secret: CredentialSecret::Bearer {
                access_token: secrecy::SecretString::from("XLC-SENTINEL-SECRET".to_string()),
                refresh_token: None,
                expires_at: None,
            },
        }
    }

    #[tokio::test]
    async fn concurrent_callers_trigger_exactly_one_refresh() {
        let coordinator = Arc::new(RefreshCoordinator::new());
        let call_count = Arc::new(AtomicUsize::new(0));

        let mut tasks = Vec::new();
        for _ in 0..50 {
            let coordinator = Arc::clone(&coordinator);
            let call_count = Arc::clone(&call_count);
            tasks.push(tokio::spawn(async move {
                coordinator
                    .refresh_single_flight(move || {
                        let call_count = Arc::clone(&call_count);
                        async move {
                            call_count.fetch_add(1, Ordering::SeqCst);
                            // Yield so the other 49 tasks have a chance to arrive and join this
                            // same in-flight attempt instead of each starting their own.
                            tokio::task::yield_now().await;
                            tokio::task::yield_now().await;
                            Ok(sample_set())
                        }
                    })
                    .await
            }));
        }

        for task in tasks {
            let result = task.await.unwrap();
            assert!(result.is_ok());
        }

        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn sequential_calls_after_completion_each_refresh_again() {
        let coordinator = RefreshCoordinator::new();
        let call_count = Arc::new(AtomicUsize::new(0));

        for _ in 0..3 {
            let call_count = Arc::clone(&call_count);
            coordinator
                .refresh_single_flight(move || {
                    let call_count = Arc::clone(&call_count);
                    async move {
                        call_count.fetch_add(1, Ordering::SeqCst);
                        Ok(sample_set())
                    }
                })
                .await
                .unwrap();
        }

        assert_eq!(call_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn refresh_failure_is_shared_by_every_waiter() {
        let coordinator = Arc::new(RefreshCoordinator::new());
        let mut tasks = Vec::new();
        for _ in 0..10 {
            let coordinator = Arc::clone(&coordinator);
            tasks.push(tokio::spawn(async move {
                coordinator
                    .refresh_single_flight(|| async {
                        tokio::task::yield_now().await;
                        Err(AuthError::RefreshRejected)
                    })
                    .await
            }));
        }
        for task in tasks {
            let result = task.await.unwrap();
            assert!(matches!(result, Err(AuthError::RefreshRejected)));
        }
    }
}
