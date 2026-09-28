// SPDX-License-Identifier: GPL-3.0-only

//! Process-wide wiring (CODEBASE.md §5): builds the shared `Arc`s (`reqwest::Client`,
//! `AuthBroker`, `ProviderRegistry`) that `runtime`/`tui` will receive in later phases. `app` is
//! the only crate allowed to know concrete `provider-*` types (INV-2).
//!
//! In Phase 1 Wave B, `build_runtime` / `build_runtime_at` / `build_runtime_with_paths` wire the
//! full layered `xlightcli-config` loader (`ConfigLoader`) and workspace trust gate (`TrustStore`),
//! passing the resolved `Config` directly to `RuntimeDeps`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use xlightcli_auth::{
    AuthAdapter, AuthBroker, AuthError, AuthMethod, CredentialSet, DiscoveredCredential, LoginUi,
    StoreKind,
};
use xlightcli_config::{ConfigLoader, ExperimentalFlags, PartialConfig, TrustStore};
use xlightcli_provider::{HttpClientConfig, Provider, ProviderRegistry, build_client};
use xlightcli_provider_agy::AgyProvider;
use xlightcli_provider_claude::ClaudeProvider;
use xlightcli_provider_codex::CodexProvider;

/// Errors that can occur while assembling the runtime in [`build_runtime`] / [`build_runtime_at`].
#[derive(Debug)]
pub enum WiringError {
    Storage(xlightcli_storage::StorageError),
    Config(xlightcli_config::ConfigError),
}

impl std::fmt::Display for WiringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(e) => write!(f, "storage error: {e}"),
            Self::Config(e) => write!(f, "config error: {e}"),
        }
    }
}

impl std::error::Error for WiringError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(e) => Some(e),
            Self::Config(e) => Some(e),
        }
    }
}

impl From<xlightcli_storage::StorageError> for WiringError {
    fn from(e: xlightcli_storage::StorageError) -> Self {
        Self::Storage(e)
    }
}

impl From<xlightcli_config::ConfigError> for WiringError {
    fn from(e: xlightcli_config::ConfigError) -> Self {
        Self::Config(e)
    }
}

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

/// Reads experimental opt-in flags (D-002) from env vars:
/// `XLIGHTCLI_EXPERIMENTAL_CLAUDE_SUBSCRIPTION=1` / `XLIGHTCLI_EXPERIMENTAL_ANTIGRAVITY=1`.
pub(crate) fn experimental_flags_from_env() -> ExperimentalFlags {
    fn is_set(key: &str) -> bool {
        std::env::var(key).is_ok_and(|v| v == "1" || v == "true")
    }
    ExperimentalFlags {
        claude_subscription: is_set("XLIGHTCLI_EXPERIMENTAL_CLAUDE_SUBSCRIPTION"),
        antigravity_subscription: is_set("XLIGHTCLI_EXPERIMENTAL_ANTIGRAVITY")
            || is_set("XLIGHTCLI_EXPERIMENTAL_ANTIGRAVITY_SUBSCRIPTION"),
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
    let mut flags = experimental_flags_from_env();
    let global_config = xlightcli_config::paths::global_config_file();
    let trust_path = xlightcli_config::paths::data_dir().join("trust.toml");
    if let Ok(store) = TrustStore::load(&trust_path) {
        let loader = ConfigLoader::new(&global_config);
        if let Ok(loaded) = loader.load(&store, None, std::env::vars(), PartialConfig::empty()) {
            if loaded.config.experimental.claude_subscription {
                flags.claude_subscription = true;
            }
            if loaded.config.experimental.antigravity_subscription {
                flags.antigravity_subscription = true;
            }
        }
    }

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

/// Everything the runtime-backed surfaces (`exec`, the bare TUI) need — built on top of
/// [`AppContext`]'s provider/auth wiring plus `xlightcli-storage`/`xlightcli-tools`, which
/// `dev`/`auth`/`provider` never touch (kept as a separate entry point so `build()`'s existing
/// callers/tests are unaffected, docs/CONTRACTS.md §7 Wave A brief: "keep all existing commands
/// working").
#[derive(Debug)]
pub struct RuntimeContext {
    pub handle: xlightcli_runtime::RuntimeHandle,
}

/// Builds a [`RuntimeContext`]: loads resolved global and project configuration via
/// [`ConfigLoader`] and [`TrustStore`], opens the real `xlightcli-storage` database
/// (`xlightcli_config::paths::database_path()`), wires auth and providers according to
/// the resolved config, and registers built-in tools.
pub async fn build_runtime() -> Result<RuntimeContext, WiringError> {
    let repo_root = std::env::current_dir().ok();
    build_runtime_with_paths(
        xlightcli_config::paths::database_path(),
        xlightcli_config::paths::global_config_file(),
        xlightcli_config::paths::data_dir().join("trust.toml"),
        repo_root.as_deref(),
    )
    .await
}

/// Same as [`build_runtime`], but opens storage at `db_path`. When `db_path` is not the standard
/// XDG database path (i.e. in tests), global config and trust store default to paths relative to
/// `db_path`'s parent directory, ensuring strict test isolation from the host environment.
pub async fn build_runtime_at(db_path: PathBuf) -> Result<RuntimeContext, WiringError> {
    let repo_root = std::env::current_dir().ok();
    let (global_config, trust_path) = if db_path == xlightcli_config::paths::database_path() {
        (
            xlightcli_config::paths::global_config_file(),
            xlightcli_config::paths::data_dir().join("trust.toml"),
        )
    } else {
        let parent = db_path.parent().unwrap_or(Path::new("."));
        let config_file = if parent.join("config").join("config.toml").exists() {
            parent.join("config").join("config.toml")
        } else {
            parent.join("config.toml")
        };
        (config_file, parent.join("trust.toml"))
    };
    build_runtime_with_paths(db_path, global_config, trust_path, repo_root.as_deref()).await
}

/// Builds a [`RuntimeContext`] with fully explicit paths for database, global config, trust store,
/// and optional repository root. Used by [`build_runtime`], [`build_runtime_at`], and tests
/// requiring isolated project/trust configurations.
pub async fn build_runtime_with_paths(
    db_path: PathBuf,
    global_config_path: PathBuf,
    trust_path: PathBuf,
    repo_root: Option<&Path>,
) -> Result<RuntimeContext, WiringError> {
    let mut loader = ConfigLoader::new(&global_config_path);
    if let Some(root) = repo_root {
        loader = loader.with_project_dir(xlightcli_config::paths::project_config_dir(root));
    }
    let trust_store = TrustStore::load(&trust_path)?;
    let loaded = loader.load(
        &trust_store,
        repo_root,
        std::env::vars(),
        PartialConfig::empty(),
    )?;
    let mut config = loaded.config;

    // Honor both env var aliases for experimental flags alongside global config:
    if std::env::var("XLIGHTCLI_EXPERIMENTAL_ANTIGRAVITY").is_ok_and(|v| v == "1" || v == "true") {
        config.experimental.antigravity_subscription = true;
    }
    if std::env::var("XLIGHTCLI_EXPERIMENTAL_CLAUDE_SUBSCRIPTION")
        .is_ok_and(|v| v == "1" || v == "true")
    {
        config.experimental.claude_subscription = true;
    }

    let http =
        build_client(&HttpClientConfig::default()).unwrap_or_else(|_| reqwest::Client::new());

    let store_kind = match config.auth.store {
        xlightcli_config::AuthStoreKind::File => StoreKind::File,
        xlightcli_config::AuthStoreKind::Keyring => StoreKind::Keyring,
    };
    let credentials_dir = if db_path == xlightcli_config::paths::database_path() {
        xlightcli_config::paths::data_dir().join("credentials")
    } else {
        db_path
            .parent()
            .unwrap_or(Path::new("."))
            .join("credentials")
    };
    let store = store_kind.build(credentials_dir);

    let mut auth = match xlightcli_storage::AccountIndex::open(&db_path).await {
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
    register_providers(&mut providers, &mut auth, http, &config.experimental);

    let storage = xlightcli_storage::Storage::open(db_path).await?;
    let deps = xlightcli_runtime::RuntimeDeps {
        providers: Arc::new(providers),
        auth: Arc::new(auth),
        tools: Arc::new(xlightcli_tools::ToolRegistry::with_builtins()),
        storage,
        config,
    };
    let handle =
        xlightcli_runtime::RuntimeHandle::new(deps, xlightcli_runtime::RuntimeConfig::default());
    Ok(RuntimeContext { handle })
}
