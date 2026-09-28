// SPDX-License-Identifier: GPL-3.0-only

//! Process-wide wiring (CODEBASE.md §5): builds the shared `Arc`s (`reqwest::Client`,
//! `AuthBroker`, `ProviderRegistry`) that `runtime`/`tui` will receive in later phases. `app` is
//! the only crate allowed to know concrete `provider-*` types (INV-2).
//!
//! Phase 0 config surface (CODEBASE.md §2: `config` is a minimal stub — layered
//! `config.toml` parsing is Phase 1): experimental opt-in and the secret-store backend are read
//! straight from env vars here, not from a config file yet.

use std::sync::Arc;

use async_trait::async_trait;
use xlightcli_auth::{
    AuthAdapter, AuthBroker, AuthError, AuthMethod, CredentialSet, DiscoveredCredential, LoginUi,
    StoreKind,
};
use xlightcli_config::ExperimentalFlags;
use xlightcli_provider::{HttpClientConfig, Provider, ProviderRegistry, build_client};
use xlightcli_provider_agy::AgyProvider;
use xlightcli_provider_claude::ClaudeProvider;
use xlightcli_provider_codex::CodexProvider;

/// Everything a command needs to talk to providers. Constructed once per process.
#[derive(Debug)]
pub struct AppContext {
    pub auth: AuthBroker,
    pub providers: ProviderRegistry,
}

/// `Provider::auth()` only returns a borrowed `&dyn AuthAdapter` (docs/CONTRACTS.md §3), but
/// `AuthBroker::register_adapter` needs an owned `Arc<dyn AuthAdapter>`. Rather than construct a
/// second, separate adapter instance (which would risk drifting from whatever
/// caching/rate-limiting state the real adapter holds), this forwards every call to the *same*
/// `Arc<dyn Provider>` already held by the registry — one instance, two trait views.
///
/// `pub(crate)` so `cmd::auth`'s tests can wire a `MockProvider` into both an `AppContext`'s
/// `ProviderRegistry` and its `AuthBroker` the same way `wiring::build()` does for the real
/// providers, instead of every test call site reinventing this.
pub(crate) struct ProviderAuthAdapter(pub(crate) Arc<dyn Provider>);

#[async_trait]
impl AuthAdapter for ProviderAuthAdapter {
    fn methods(&self) -> &[AuthMethod] {
        self.0.auth().methods()
    }

    async fn discover_existing(&self) -> Vec<DiscoveredCredential> {
        self.0.auth().discover_existing().await
    }

    async fn import(&self, found: &DiscoveredCredential) -> Result<CredentialSet, AuthError> {
        self.0.auth().import(found).await
    }

    async fn login(
        &self,
        method: AuthMethod,
        ui: &dyn LoginUi,
    ) -> Result<CredentialSet, AuthError> {
        self.0.auth().login(method, ui).await
    }

    async fn refresh(&self, current: &CredentialSet) -> Result<CredentialSet, AuthError> {
        self.0.auth().refresh(current).await
    }

    async fn revoke(&self, current: &CredentialSet) -> Result<(), AuthError> {
        self.0.auth().revoke(current).await
    }
}

/// Reads the Phase 0 experimental opt-in (D-002) from env — global `config.toml` parsing of
/// `[experimental]` is Phase 1 (CODEBASE.md §2 `config` status):
/// `XLIGHTCLI_EXPERIMENTAL_CLAUDE_SUBSCRIPTION=1` / `XLIGHTCLI_EXPERIMENTAL_ANTIGRAVITY=1`.
pub(crate) fn experimental_flags_from_env() -> ExperimentalFlags {
    fn is_set(key: &str) -> bool {
        std::env::var(key).is_ok_and(|v| v == "1")
    }
    ExperimentalFlags {
        claude_subscription: is_set("XLIGHTCLI_EXPERIMENTAL_CLAUDE_SUBSCRIPTION"),
        antigravity_subscription: is_set("XLIGHTCLI_EXPERIMENTAL_ANTIGRAVITY"),
    }
}

/// `XLIGHTCLI_AUTH_STORE=file|keyring` (default `keyring`, D-019).
fn store_kind_from_env() -> StoreKind {
    match std::env::var("XLIGHTCLI_AUTH_STORE").as_deref() {
        Ok("file") => StoreKind::File,
        _ => StoreKind::Keyring,
    }
}

/// Registers `CodexProvider`/`ClaudeProvider`/`AgyProvider` into `providers`, and a
/// `ProviderAuthAdapter` forwarding to each into `auth`. Isolated in its own function (Wave 2
/// brief): this is the one place to touch once every `provider-*` crate implements
/// `xlightcli_provider::Provider`.
fn register_providers(
    providers: &mut ProviderRegistry,
    auth: &mut AuthBroker,
    http: reqwest::Client,
    flags: &ExperimentalFlags,
) {
    let codex: Arc<dyn Provider> = Arc::new(CodexProvider::new(http.clone(), flags));
    let claude: Arc<dyn Provider> = Arc::new(ClaudeProvider::new(http.clone(), flags));
    let agy: Arc<dyn Provider> = Arc::new(AgyProvider::new(http, flags));

    for provider in [codex, claude, agy] {
        auth.register_adapter(
            provider.id(),
            Arc::new(ProviderAuthAdapter(provider.clone())),
        );
        providers.register(provider);
    }
}

/// Builds the shared `reqwest::Client`, `ProviderRegistry` (all three providers + their
/// transports), and `AuthBroker` (each provider's adapter registered, `SecretStore` chosen per
/// `XLIGHTCLI_AUTH_STORE`, backed by the persisted `AccountIndex` so `credential`/`login`/
/// `import`/`accounts`/`logout` are all fully functional).
///
/// Async because opening the `AccountIndex` (SQLite, `xlightcli-storage`) is IO. If it can't be
/// opened, this degrades to `AuthBroker::with_store` (no persistence — every auth call fails
/// clearly with `AuthError::StoreUnavailable`, per INV-11/INV-10) instead of failing the whole
/// process: `dev probe`, `provider list/info` don't need an account index at all.
pub async fn build() -> AppContext {
    let flags = experimental_flags_from_env();
    let http =
        build_client(&HttpClientConfig::default()).unwrap_or_else(|_| reqwest::Client::new());

    let store =
        store_kind_from_env().build(xlightcli_config::paths::data_dir().join("credentials"));
    let mut auth =
        match xlightcli_storage::AccountIndex::open(xlightcli_config::paths::database_path()).await
        {
            Ok(index) => AuthBroker::with_store_and_index(store, index),
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "could not open account index; auth persistence disabled for this run"
                );
                AuthBroker::with_store(store)
            }
        };
    let mut providers = ProviderRegistry::new();
    register_providers(&mut providers, &mut auth, http, &flags);

    AppContext { auth, providers }
}
