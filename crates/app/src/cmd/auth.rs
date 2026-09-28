// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli auth {list,login,logout,import}` (docs/PLAN.md §6).
//!
//! `list` shows both what's actually logged in (`AuthBroker::accounts`, backed by the persisted
//! `AccountIndex`) and what's merely discoverable-but-not-yet-imported
//! (`Provider::auth().discover_existing()`, read-only, D-017) — the latter never touches
//! `AuthBroker` since it doesn't need its storage/refresh machinery at all.

use std::sync::Arc;

use xlightcli_auth::{AuthError, AuthMethod};
use xlightcli_protocol::{AuthKind, ProviderId, Stability, TransportId};
use xlightcli_provider::{Provider, TransportAdapter};

use crate::cli::{AuthCommand, LoginMethodArg};
use crate::cmd::error::CliError;
use crate::login_ui::TerminalLoginUi;
use crate::wiring::AppContext;

pub async fn dispatch(ctx: &AppContext, command: &AuthCommand) -> Result<(), CliError> {
    match command {
        AuthCommand::List => list(ctx).await,
        AuthCommand::Login {
            provider,
            transport,
            method,
        } => login(ctx, provider, transport.as_deref(), *method).await,
        AuthCommand::Logout {
            provider,
            transport,
            account,
        } => logout(ctx, provider, transport.as_deref(), account.as_deref()).await,
        AuthCommand::Import { provider } => import(ctx, provider).await,
    }
}

fn lookup<'a>(
    ctx: &'a AppContext,
    provider_arg: &str,
) -> Result<(&'a Arc<dyn Provider>, ProviderId), CliError> {
    let provider_id = ProviderId::new(provider_arg);
    let provider = ctx.providers.get(&provider_id).ok_or_else(|| {
        CliError::invalid_input(format!(
            "unknown provider {provider_arg:?} (registered: {})",
            ctx.providers
                .ids()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })?;
    Ok((provider, provider_id))
}

async fn list(ctx: &AppContext) -> Result<(), CliError> {
    match ctx.auth.accounts(None).await {
        Ok(accounts) if accounts.is_empty() => {
            crate::output::info("(no accounts logged in yet — see below for importable ones)");
        }
        Ok(accounts) => {
            for account in accounts {
                crate::output::info(&format!(
                    "{}/{}: {} ({:?})",
                    account.provider, account.transport, account.account_id, account.auth_kind
                ));
            }
        }
        // Not fatal: `provider list`/`dev probe` don't need the account index, so a broker built
        // without one (`wiring::build()`'s fallback) shouldn't make `auth list` unusable — just
        // less informative. Discovery below still runs regardless.
        Err(err) => crate::output::warn(&format!("could not list logged-in accounts: {err}")),
    }

    if ctx.providers.is_empty() {
        return Ok(());
    }
    for provider in ctx.providers.iter() {
        for credential in provider.auth().discover_existing().await {
            crate::output::info(&format!(
                "(discoverable, not imported) {}/{}: {} (source: {})",
                credential.provider,
                credential.transport,
                credential.account_label,
                credential.source.display(),
            ));
        }
    }
    Ok(())
}

async fn login(
    ctx: &AppContext,
    provider_arg: &str,
    transport_arg: Option<&str>,
    method_arg: Option<LoginMethodArg>,
) -> Result<(), CliError> {
    let (provider, provider_id) = lookup(ctx, provider_arg)?;
    let (transport, method) = select_transport_and_method(provider, transport_arg, method_arg)?;
    let transport_id = transport.id();
    ensure_experimental_opt_in(transport.as_ref())?;

    crate::output::info(&format!(
        "Logging into {provider_id} ({transport_id}) via {method:?}..."
    ));
    match ctx.auth.login(&provider_id, method, &TerminalLoginUi).await {
        Ok(account) => {
            crate::output::info(&format!(
                "Logged in: {} ({provider_id}/{})",
                account.account_id, account.transport
            ));
            Ok(())
        }
        Err(AuthError::NotLoggedIn { .. }) => Err(CliError::invalid_input(format!(
            "provider {provider_id} has no auth adapter registered"
        ))),
        Err(AuthError::NotImplemented(detail)) => Err(CliError::other(format!(
            "{provider_id} does not support this login method yet ({detail})"
        ))),
        Err(err) => {
            let mut msg = format!("login failed: {err}");
            if method == AuthMethod::BrowserOAuth && err.to_string().contains("already in use") {
                msg.push_str(&format!(
                    "\nhint: another process holds the OAuth callback port (often a stale login \
                     from another CLI). Free it, or run `xlightcli auth login {provider_id} \
                     --method device`."
                ));
            }
            Err(CliError::other(msg))
        }
    }
}

/// Auth kind a CLI login method produces.
fn method_kind(method: LoginMethodArg) -> AuthKind {
    match method {
        LoginMethodArg::ApiKey => AuthKind::ApiKey,
        LoginMethodArg::Browser | LoginMethodArg::Device => AuthKind::Subscription,
    }
}

fn to_auth_method(method: LoginMethodArg) -> AuthMethod {
    match method {
        LoginMethodArg::Browser => AuthMethod::BrowserOAuth,
        LoginMethodArg::Device => AuthMethod::DeviceCode,
        LoginMethodArg::ApiKey => AuthMethod::ApiKey,
    }
}

/// Picks a transport and login method that agree on the auth kind, so an OAuth token is never
/// stored under an API-key transport (or vice versa).
fn select_transport_and_method(
    provider: &Arc<dyn Provider>,
    transport_arg: Option<&str>,
    method_arg: Option<LoginMethodArg>,
) -> Result<(Arc<dyn TransportAdapter>, AuthMethod), CliError> {
    let provider_id = provider.id();
    let available = || {
        provider
            .transports()
            .iter()
            .map(|t| format!("{} ({:?})", t.id(), t.required_auth()))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let transport = match (transport_arg, method_arg) {
        (Some(t), _) => provider
            .transport(&TransportId::new(t))
            .cloned()
            .ok_or_else(|| {
                CliError::invalid_input(format!(
                    "{provider_id} has no transport `{t}` (available: {})",
                    available()
                ))
            })?,
        (None, Some(m)) => provider
            .transports()
            .iter()
            .find(|t| t.required_auth() == method_kind(m))
            .cloned()
            .ok_or_else(|| {
                CliError::invalid_input(format!(
                    "{provider_id} has no transport for --method {m:?} in this build \
                     (available: {}); experimental transports need \
                     `--features experimental` at build time",
                    available()
                ))
            })?,
        (None, None) => provider.transports().first().cloned().ok_or_else(|| {
            CliError::invalid_input(format!(
                "provider {provider_id} has no transports registered"
            ))
        })?,
    };
    let method = match method_arg {
        Some(m) if method_kind(m) != transport.required_auth() => {
            let needs = match transport.required_auth() {
                AuthKind::ApiKey => "use --method api-key",
                AuthKind::Subscription => "use --method browser or --method device",
            };
            return Err(CliError::invalid_input(format!(
                "--method {m:?} cannot log into {provider_id}/{}; {needs}",
                transport.id()
            )));
        }
        Some(m) => to_auth_method(m),
        None => match transport.required_auth() {
            AuthKind::ApiKey => AuthMethod::ApiKey,
            AuthKind::Subscription => AuthMethod::BrowserOAuth,
        },
    };
    Ok((transport, method))
}

/// D-002: logging into (or importing for) an experimental transport already uses the official
/// client's OAuth identity, so the runtime opt-in is required *before* any auth traffic, not
/// only before model requests.
fn ensure_experimental_opt_in(transport: &dyn TransportAdapter) -> Result<(), CliError> {
    if transport.stability() != Stability::Experimental {
        return Ok(());
    }
    let flags = crate::wiring::experimental_flags_from_env();
    let (opted_in, env) = match transport.id().as_str() {
        "claude-subscription" => (
            flags.claude_subscription,
            "XLIGHTCLI_EXPERIMENTAL_CLAUDE_SUBSCRIPTION",
        ),
        "antigravity" => (
            flags.antigravity_subscription,
            "XLIGHTCLI_EXPERIMENTAL_ANTIGRAVITY",
        ),
        _ => (false, "<no opt-in defined>"),
    };
    if opted_in {
        crate::output::warn(&format!(
            "{} is an experimental transport: it signs in as the official client, which the \
             provider's terms may prohibit (D-002). Proceeding because {env}=1.",
            transport.id()
        ));
        return Ok(());
    }
    Err(CliError::invalid_input(format!(
        "{} is an experimental transport (D-002): it signs in as the official client, which the \
         provider's terms may prohibit and which can get the account restricted. To accept that \
         risk, set {env}=1 and retry.",
        transport.id()
    )))
}

async fn logout(
    ctx: &AppContext,
    provider_arg: &str,
    transport_arg: Option<&str>,
    account_arg: Option<&str>,
) -> Result<(), CliError> {
    let (provider, provider_id) = lookup(ctx, provider_arg)?;
    let transport_id = match transport_arg {
        Some(t) => TransportId::new(t),
        None => provider
            .transports()
            .first()
            .map(|t| t.id())
            .ok_or_else(|| {
                CliError::invalid_input(format!(
                    "provider {provider_id} has no transports registered"
                ))
            })?,
    };

    let account_id = match account_arg {
        Some(a) => a.to_string(),
        None => resolve_sole_account(ctx, &provider_id, &transport_id).await?,
    };

    ctx.auth
        .logout(&provider_id, &transport_id, &account_id)
        .await
        .map_err(|err| CliError::other(format!("logout failed: {err}")))?;
    crate::output::info(&format!(
        "Logged out: {account_id} ({provider_id}/{transport_id})"
    ));
    Ok(())
}

/// Resolves `--account` when omitted: only sensible if exactly one account is logged in for
/// `(provider, transport)` — otherwise ambiguous, and INV-10 says surface that clearly rather than
/// guess.
async fn resolve_sole_account(
    ctx: &AppContext,
    provider_id: &ProviderId,
    transport_id: &TransportId,
) -> Result<String, CliError> {
    let accounts = ctx
        .auth
        .accounts(Some(provider_id.clone()))
        .await
        .map_err(|err| CliError::other(format!("could not list accounts: {err}")))?;
    let matching: Vec<_> = accounts
        .into_iter()
        .filter(|a| &a.transport == transport_id)
        .collect();
    match matching.as_slice() {
        [] => Err(CliError::invalid_input(format!(
            "not logged in to {provider_id} ({transport_id})"
        ))),
        [only] => Ok(only.account_id.clone()),
        _ => Err(CliError::invalid_input(format!(
            "multiple accounts logged in for {provider_id}/{transport_id}; specify --account <id>"
        ))),
    }
}

async fn import(ctx: &AppContext, provider_arg: &str) -> Result<(), CliError> {
    let (provider, provider_id) = lookup(ctx, provider_arg)?;
    let found = provider.auth().discover_existing().await;
    let Some(candidate) = found.first() else {
        crate::output::info(&format!(
            "no existing credentials discovered for {provider_id}"
        ));
        return Ok(());
    };

    crate::output::info(&format!(
        "Found existing credential for {provider_id}: {} (source: {})",
        candidate.account_label,
        candidate.source.display(),
    ));
    if let Some(transport) = provider.transport(&candidate.transport) {
        ensure_experimental_opt_in(transport.as_ref())?;
    }
    if provider_id.as_str() == "codex" {
        crate::output::warn(
            "Codex refresh tokens rotate on use (D-017): once xlightcli imports and refreshes \
             this credential, the official Codex CLI may be signed out.",
        );
    }
    if !crate::output::confirm("Import this credential into xlightcli's secret store?")? {
        crate::output::info("Import cancelled.");
        return Ok(());
    }

    match ctx.auth.import(candidate).await {
        Ok(account) => {
            crate::output::info(&format!("Imported: {} ({provider_id})", account.account_id));
            Ok(())
        }
        Err(AuthError::NotImplemented(detail)) => Err(CliError::other(format!(
            "import ran but xlightcli can't persist the credential yet ({detail}); known gap in \
             AuthBroker::import (secret-store persistence)"
        ))),
        Err(err) => Err(CliError::other(format!("import failed: {err}"))),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use xlightcli_provider::ProviderRegistry;
    use xlightcli_provider::testing::MockProvider;

    use super::*;

    fn ctx_with_one_mock_provider() -> AppContext {
        let mut providers = ProviderRegistry::new();
        let mut auth = xlightcli_auth::AuthBroker::new();
        let provider: Arc<dyn xlightcli_provider::Provider> =
            Arc::new(MockProvider::scripted("mock", "mock-transport", Vec::new()));
        // Mirrors `wiring::build()`: an adapter must be registered in `AuthBroker` too, or
        // `AuthBroker::login`/`import` short-circuit with `NotLoggedIn` before ever reaching the
        // provider's own (mock) auth flow.
        auth.register_adapter(
            provider.id(),
            Arc::new(crate::wiring::ProviderAuthAdapter(provider.clone())),
        );
        providers.register(provider);
        AppContext { auth, providers }
    }

    #[tokio::test]
    async fn list_on_unknown_registry_is_fine_when_empty() {
        let ctx = AppContext {
            auth: xlightcli_auth::AuthBroker::new(),
            providers: ProviderRegistry::new(),
        };
        assert!(list(&ctx).await.is_ok());
    }

    #[tokio::test]
    async fn list_reports_no_credentials_for_mock_provider() {
        let ctx = ctx_with_one_mock_provider();
        assert!(list(&ctx).await.is_ok());
    }

    #[tokio::test]
    async fn login_unknown_provider_is_invalid_input() {
        let ctx = ctx_with_one_mock_provider();
        let err = login(&ctx, "nope", None, Some(LoginMethodArg::Browser))
            .await
            .unwrap_err();
        assert_eq!(err.exit_code_number(), 2);
    }

    #[tokio::test]
    async fn login_known_provider_hits_the_not_implemented_persistence_stub() {
        let ctx = ctx_with_one_mock_provider();
        let err = login(&ctx, "mock", None, Some(LoginMethodArg::ApiKey))
            .await
            .unwrap_err();
        // MockProvider's auth adapter always returns NotImplemented (see
        // `xlightcli_provider::testing::MockAuthAdapter`), which we map to exit 1, not 2 — the
        // provider *was* found, it's the persistence step that's unimplemented.
        assert_eq!(err.exit_code_number(), 1);
    }

    #[tokio::test]
    async fn logout_unknown_provider_is_invalid_input() {
        let ctx = ctx_with_one_mock_provider();
        let err = logout(&ctx, "nope", None, None).await.unwrap_err();
        assert_eq!(err.exit_code_number(), 2);
    }

    #[tokio::test]
    async fn logout_without_an_index_reports_error_not_fake_success() {
        // `AuthBroker::new()` (no store/index): `--account` omitted forces a lookup through
        // `accounts()`, which fails clearly (`StoreUnavailable`) instead of the command silently
        // succeeding at nothing (INV-10).
        let ctx = ctx_with_one_mock_provider();
        let err = logout(&ctx, "mock", None, None).await.unwrap_err();
        assert_eq!(err.exit_code_number(), 1);
    }

    /// A minimal `AuthAdapter` whose `login()` always succeeds — `MockAuthAdapter` (used by
    /// `xlightcli_provider::testing::MockProvider`) always returns `NotImplemented`, so it can't
    /// exercise a real login -> logout round trip against a fully-wired `AuthBroker`.
    struct AlwaysSucceedsAdapter;

    #[async_trait::async_trait]
    impl xlightcli_auth::AuthAdapter for AlwaysSucceedsAdapter {
        fn methods(&self) -> &[xlightcli_auth::AuthMethod] {
            &[xlightcli_auth::AuthMethod::ApiKey]
        }

        async fn discover_existing(&self) -> Vec<xlightcli_auth::DiscoveredCredential> {
            Vec::new()
        }

        async fn import(
            &self,
            _found: &xlightcli_auth::DiscoveredCredential,
        ) -> Result<xlightcli_auth::CredentialSet, AuthError> {
            Err(AuthError::NotImplemented("unused in this test"))
        }

        async fn login(
            &self,
            _method: xlightcli_auth::AuthMethod,
            _ui: &dyn xlightcli_auth::LoginUi,
        ) -> Result<xlightcli_auth::CredentialSet, AuthError> {
            Ok(xlightcli_auth::CredentialSet {
                account: xlightcli_auth::AccountInfo {
                    provider: ProviderId::new("mock"),
                    transport: TransportId::new("mock-transport"),
                    account_id: "acc-1".into(),
                    label: None,
                    auth_kind: xlightcli_protocol::AuthKind::ApiKey,
                    metadata: serde_json::json!({}),
                },
                secret: xlightcli_auth::CredentialSecret::Bearer {
                    access_token: secrecy::SecretString::from("XLC-SENTINEL-AUTH-TEST".to_string()),
                    refresh_token: None,
                    expires_at: None,
                },
            })
        }

        async fn refresh(
            &self,
            current: &xlightcli_auth::CredentialSet,
        ) -> Result<xlightcli_auth::CredentialSet, AuthError> {
            Ok(current.clone())
        }

        async fn revoke(&self, _current: &xlightcli_auth::CredentialSet) -> Result<(), AuthError> {
            Ok(())
        }
    }

    async fn ctx_with_real_broker(dir: &std::path::Path) -> AppContext {
        let mut providers = ProviderRegistry::new();
        providers.register(Arc::new(MockProvider::scripted(
            "mock",
            "mock-transport",
            Vec::new(),
        )));

        let store: Arc<dyn xlightcli_auth::SecretStore> =
            Arc::new(xlightcli_auth::FileStore::new(dir.join("credentials")));
        let index = xlightcli_storage::AccountIndex::open(dir.join("accounts.db"))
            .await
            .unwrap();
        let mut auth = xlightcli_auth::AuthBroker::with_store_and_index(store, index);
        auth.register_adapter(ProviderId::new("mock"), Arc::new(AlwaysSucceedsAdapter));

        AppContext { auth, providers }
    }

    #[tokio::test]
    async fn login_then_logout_round_trip_with_a_real_broker() {
        let temp = tempfile::tempdir().unwrap();
        let ctx = ctx_with_real_broker(temp.path()).await;

        login(&ctx, "mock", None, Some(LoginMethodArg::ApiKey))
            .await
            .expect("login should succeed against the real broker");
        assert_eq!(
            ctx.auth.accounts(None).await.unwrap().len(),
            1,
            "expected exactly one persisted account after login"
        );

        // `--account` omitted: there's exactly one, so it should resolve automatically.
        logout(&ctx, "mock", None, None)
            .await
            .expect("logout should succeed and auto-resolve the sole account");
        assert!(
            ctx.auth.accounts(None).await.unwrap().is_empty(),
            "expected no accounts left after logout"
        );
    }

    #[tokio::test]
    async fn logout_with_no_accounts_left_is_invalid_input() {
        let temp = tempfile::tempdir().unwrap();
        let ctx = ctx_with_real_broker(temp.path()).await;
        let err = logout(&ctx, "mock", None, None).await.unwrap_err();
        assert_eq!(err.exit_code_number(), 2);
    }
}
