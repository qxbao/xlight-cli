# CONTRACTS.md — Public API after Wave 1 (Phase 0)

> Summary of the public API of `protocol`, `auth`, `provider`, `config`, and each adapter's
> constructor, so Wave 2 agents can work in parallel without reading all the code. Design details:
> [`docs/PLAN.md`](PLAN.md); mandatory patterns: [`../PATTERNS.md`](../PATTERNS.md).
>
> Convention: every API below already **builds + clippy (`-D warnings`) + fmt clean**, both with
> and without `--all-features`. The signature is the source of truth; Wave 2 **must not change a
> public signature** without updating this file + reporting back to the team lead.

## 0. Status by crate

| Crate | Status | What Wave 2 does |
|-------|-----------|----------------|
| `xlightcli-protocol` | **usable** — full canonical types, has serde roundtrip tests | Extend when a new field is needed (rare) |
| `xlightcli-config` | minimal (paths + `ExperimentalFlags`/`Config` stub) | Phase 1: `layer`/`merge`/`schema`/`trust` |
| `xlightcli-storage` | **`AccountIndex` usable** (Wave 2) — rusqlite + WAL, migration `0001_accounts.sql`; full event-sourced schema still Phase 1 | — |
| `xlightcli-auth` | **usable** (Wave 2) — keyring/file store, loopback server, device code, single-flight refresh with generation counter, `AuthBroker::credential/login/import/accounts/logout` all implemented | — |
| `xlightcli-provider` | **complete infra** — trait, SSE parser, http, retry, gate, `map_status`, `testing` | Don't change the trait; just use it |
| `xlightcli-provider-{codex,claude,agy}` | skeleton — struct + constructor + empty modules | Implement `auth`, `wire`, `transport_*`, `quota`, `import`, `features` + `impl Provider` |
| `xlightcli-tools`, `xlightcli-mcp`, `xlightcli-runtime`, `xlightcli-tui` | empty skeleton (doc comment) | Phase 1+ |
| `xlightcli` (app), `xtask` | CLI stub (`dev probe` reports a clear error), wiring empty | Wave 2 wires `wiring::build` up to real providers |

---

## 1. `xlightcli-protocol`

No IO, no dependency on any internal crate. Every type is `Debug + Clone`, most are `Serialize +
Deserialize` (except `ProviderError`/`AuthFailure` — errors don't need serde).

### Ids (`ids.rs`)

```rust
pub struct ProviderId(String);   // newtype, Display, From<String>/From<&str>, ::new(impl Into<String>)
pub struct TransportId(String);
pub struct ModelId(String);
pub struct ToolCallId(String);
pub struct SessionId(Uuid);      // ::new() random v4, ::from_uuid, ::as_uuid
pub struct AgentId(Uuid);
pub struct CommandId(String);    // ::new(namespace, name) -> "namespace.name"; .namespace() -> &str
```

### Turn / message (`turn.rs`, `message.rs`, `tool.rs`)

```rust
pub struct TurnRequest {
    pub model: ModelId,
    pub system: SystemPrompt,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub reasoning: Option<ReasoningConfig>,
    pub max_output_tokens: Option<u32>,
    pub provider_options: ProviderOptions,
}
impl TurnRequest {
    pub fn simple(model: ModelId, text: impl Into<String>) -> Self;
}

pub struct SystemPrompt { pub text: String }
pub struct ProviderOptions(/* opaque serde_json::Map */);
impl ProviderOptions {
    pub fn new() -> Self;
    pub fn get(&self, key: &str) -> Option<&serde_json::Value>;
    pub fn insert(&mut self, key: impl Into<String>, value: serde_json::Value) -> Option<serde_json::Value>;
    pub fn is_empty(&self) -> bool;
    pub fn as_map(&self) -> &serde_json::Map<String, serde_json::Value>;
}

pub enum ReasoningEffort { Minimal, Low, Medium, High }
pub struct ReasoningConfig { pub effort: ReasoningEffort, pub include_text: bool }

pub enum Role { System, User, Assistant }
pub struct Message { pub role: Role, pub content: Vec<ContentBlock> }
impl Message { pub fn user_text(text: impl Into<String>) -> Self; }

pub enum ContentBlock {
    Text { text: String },
    Image { media_type: String, source: ImageSource },
    Reasoning { text: Option<String>, opaque: Option<OpaqueBlob> },
    // [Wave 3 addition] `opaque` carries per-tool-call continuity data (D-009), e.g. Gemini's
    // `thoughtSignature` attached directly to a `functionCall` part — analogous to
    // `Reasoning.opaque` but scoped to this one call. `#[serde(default, skip_serializing_if =
    // "Option::is_none")]`: old payloads without the field still deserialize; `None` doesn't
    // change the serialized shape. codex/claude always construct this as `None` (their
    // continuity data lives on `Reasoning` blocks instead); agy is the first user.
    ToolUse { id: ToolCallId, name: String, input: serde_json::Value, opaque: Option<OpaqueBlob> },
    ToolResult { call_id: ToolCallId, content: Vec<ToolResultPart>, is_error: bool },
}
pub enum ImageSource { Base64 { data: String }, Artifact { path: String } }
pub enum ToolResultPart { Text { text: String }, Image { media_type: String, source: ImageSource } }
pub struct OpaqueBlob { pub provider: ProviderId, pub transport: TransportId, pub data: serde_json::Value }

pub struct ToolDefinition { pub name: String, pub description: String, pub input_schema: serde_json::Value }
```

### Event / usage (`event.rs`)

```rust
pub enum AgentEvent {
    TurnStarted { model: ModelId },
    TextDelta { index: u32, text: String },
    ReasoningDelta { index: u32, text: String },
    ToolCallStarted { index: u32, id: ToolCallId, name: String },
    Usage(Usage),
    RateLimit(RateLimitInfo),
    Completed { message: Message, stop: StopReason, usage: Usage },
}
// serde: tag = "type", rename_all = "snake_case"

pub enum StopReason { EndTurn, ToolUse, MaxTokens, Refusal, Cancelled, Other(String) }
// serde: externally tagged (NOT internally tagged — see the comment in event.rs for why)

pub struct Usage { pub input_tokens: u64, pub output_tokens: u64, pub cached_input_tokens: u64, pub reasoning_tokens: u64 }
pub struct RateLimitInfo { pub limit: Option<u64>, pub remaining: Option<u64>, pub reset_at: Option<time::OffsetDateTime> }
pub struct QuotaSnapshot { pub plan: Option<String>, pub used_percent: Option<f32>, pub resets_at: Option<OffsetDateTime>, pub detail: serde_json::Value }
```

### Capability / error (`capability.rs`, `error.rs`)

```rust
pub enum CapabilityMode { Native, Core, Compatible, Unsupported }
pub enum Stability { Stable, Experimental }
pub enum AuthKind { Subscription, ApiKey }
pub struct ProtocolVersion(pub u32); // Ord

pub struct ProviderCapabilities {
    pub reasoning: bool, pub images: bool, pub tool_calls: bool, pub parallel_tool_calls: bool,
    pub web_search: CapabilityMode, pub mcp: CapabilityMode, pub session_resume: CapabilityMode,
    pub usage: CapabilityMode, pub quota: CapabilityMode, pub context_window: Option<u32>,
}
pub struct ModelInfo { pub id: ModelId, pub display_name: String, pub context_window: Option<u32>,
                        pub max_output_tokens: Option<u32>, pub supports_reasoning: bool }

#[derive(thiserror::Error)]
pub enum ProviderError {
    Auth(#[from] AuthFailure),
    RateLimited { retry_after: Option<Duration>, info: RateLimitInfo },
    QuotaExhausted { resets_at: Option<SystemTime> },
    InvalidRequest(String),
    ProtocolMismatch { expected: ProtocolVersion, detail: String },
    TransportDisabled { reason: String },
    Network(String),
    Upstream { status: u16, body_excerpt: String },
    Cancelled,
}
pub enum AuthFailure { NoCredential, Rejected, RefreshFailed(String) }
```

### Import fragment (`import.rs`)

```rust
pub struct ConfigFragment { pub sections: serde_json::Map<String, serde_json::Value>, pub summary: String }
```

---

## 2. `xlightcli-auth`

Never exposes a secret outside `secrecy::SecretString`. Feature `testing` enables
`CredentialHandle::for_tests`. **Wave 2 status: usable** — every item below (except the parts
marked "Wave 3+") is really implemented, with the same signature as Wave 1. **Additive** items (not
present in Wave 1) are marked `[Wave 2 addition]`.

```rust
pub enum AuthMethod { ReuseExisting, BrowserOAuth, DeviceCode, ApiKey }

pub struct DiscoveredCredential { pub provider: ProviderId, pub transport: TransportId,
                                   pub account_label: String, pub source: PathBuf }

#[async_trait]
pub trait AuthAdapter: Send + Sync {
    fn methods(&self) -> &[AuthMethod];
    async fn discover_existing(&self) -> Vec<DiscoveredCredential>;
    async fn import(&self, found: &DiscoveredCredential) -> Result<CredentialSet, AuthError>;
    async fn login(&self, method: AuthMethod, ui: &dyn LoginUi) -> Result<CredentialSet, AuthError>;
    async fn refresh(&self, current: &CredentialSet) -> Result<CredentialSet, AuthError>;
    async fn revoke(&self, current: &CredentialSet) -> Result<(), AuthError>;
}

pub struct AccountInfo { pub provider: ProviderId, pub transport: TransportId, pub account_id: String,
                          pub label: Option<String>, pub auth_kind: AuthKind,
                          pub metadata: serde_json::Value }
// [Wave 3 addition] `metadata`: non-secret, provider-specific data (e.g. agy's Cloud Code Assist
// project id, discovered during OAuth login) — never a secret (convention, not enforced by type;
// same rule as `xlightcli_storage::AccountRecord::metadata`, which this is persisted into
// verbatim by `AuthBroker::persist_and_cache`/`record_to_account_info`). `#[serde(default)]` so
// pre-existing payloads still deserialize. Not `Eq` (`serde_json::Value` doesn't derive it) —
// `AccountInfo` therefore dropped `Eq` too (kept `PartialEq`), matching `AccountRecord`.
pub enum CredentialSecret {
    Bearer { access_token: SecretString, refresh_token: Option<SecretString>, expires_at: Option<OffsetDateTime> },
    Header { header_name: String, value: SecretString },
}
pub struct CredentialSet { pub account: AccountInfo, pub secret: CredentialSecret } // safe Debug, NOT Serialize

// No secret visible outside auth — only the handle:
pub struct CredentialHandle { /* Clone, Debug only prints provider/transport/account_id */ }
impl CredentialHandle {
    #[cfg(feature = "testing")]
    pub fn for_tests(account: AccountInfo, static_token: impl Into<String>) -> Self;
    pub fn account(&self) -> &AccountInfo;
    pub async fn authorize(&self, headers: &mut http::HeaderMap) -> Result<(), AuthError>; // proactively refreshes if expiry < 5' (Wave 2)
    pub async fn on_unauthorized(&self) -> Result<(), AuthError>;
    // Wave 2: single-flight refresh via RefreshCoordinator, generation counter prevents a
    // duplicate refresh (docs/PLAN.md §6.3). A `for_tests` handle always returns
    // AuthError::RefreshRejected (no adapter/store to refresh with).
}

#[async_trait]
pub trait LoginUi: Send + Sync {
    async fn show_browser_url(&self, url: &str);
    async fn show_device_code(&self, verification_uri: &str, user_code: &str);
    async fn prompt_api_key(&self, provider: &ProviderId) -> Result<SecretString, AuthError>;
}

pub struct AuthBroker { /* ... */ }
impl AuthBroker {
    pub fn new() -> Self;                                    // no store, no index: credential/login/import all StoreUnavailable
    pub fn with_store(store: Arc<dyn SecretStore>) -> Self;   // still no index: same caveat
    // [Wave 2 addition] the constructor app::wiring should actually use:
    pub fn with_store_and_index(store: Arc<dyn SecretStore>, index: xlightcli_storage::AccountIndex) -> Self;
    pub fn register_adapter(&mut self, provider: ProviderId, adapter: Arc<dyn AuthAdapter>);
    pub fn adapter(&self, provider: &ProviderId) -> Option<&Arc<dyn AuthAdapter>>;
    pub async fn credential(&self, provider: &ProviderId, transport: &TransportId) -> Result<CredentialHandle, AuthError>;
    pub async fn login(&self, provider: &ProviderId, method: AuthMethod, ui: &dyn LoginUi) -> Result<AccountInfo, AuthError>;
    pub async fn import(&self, found: &DiscoveredCredential) -> Result<AccountInfo, AuthError>;
    // [Wave 2 addition]
    pub async fn accounts(&self, provider: Option<ProviderId>) -> Result<Vec<AccountInfo>, AuthError>;
    pub async fn logout(&self, provider: &ProviderId, transport: &TransportId, account_id: &str) -> Result<(), AuthError>;
}
// Every CredentialHandle for the same (provider, transport, default account) shares one
// in-memory AccountEntry (generation counter + RefreshCoordinator), cached inside AuthBroker.

#[derive(thiserror::Error)]
pub enum AuthError { NotLoggedIn { provider: ProviderId }, RefreshRejected, StoreUnavailable(String),
                      OAuth(String), NotImplemented(&'static str) }

// store.rs — the ONLY place allowed to (de)serialize a secret:
#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn save(&self, credential: &CredentialSet) -> Result<(), AuthError>;
    async fn load(&self, provider: &ProviderId, transport: &TransportId, account_id: &str) -> Result<Option<CredentialSet>, AuthError>;
    async fn delete(&self, provider: &ProviderId, transport: &TransportId, account_id: &str) -> Result<(), AuthError>;
}
pub struct UnimplementedStore; // default, always returns StoreUnavailable

// [Wave 2 addition] store.rs — two real backends + a chooser:
pub struct KeyringStore { /* service name, default "xlightcli" */ }
impl KeyringStore {
    pub fn new(service: impl Into<String>) -> Self;
}
impl Default for KeyringStore { /* service = "xlightcli" */ }
pub struct FileStore { /* dir */ }
impl FileStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self; // dir 0700, file 0600, atomic temp+rename (D-019)
}
pub enum StoreKind { Keyring, File }
impl StoreKind {
    pub fn build(self, file_dir: PathBuf) -> Arc<dyn SecretStore>;
}
// Both KeyringStore/FileStore impl SecretStore. Keyring uses the `keyring` crate's default "v1"
// feature (Secret Service via zbus on Linux, Keychain on macOS) — no extra feature flag needed in
// the workspace.

// refresh.rs
pub struct RefreshCoordinator;
impl RefreshCoordinator {
    pub fn new() -> Self;
    pub async fn refresh_single_flight<F, Fut>(&self, refresh: F) -> Result<CredentialSet, AuthError>
        where F: FnOnce() -> Fut + Send, Fut: Future<Output = Result<CredentialSet, AuthError>> + Send;
    // Implemented (Wave 2): first caller ("leader") installs a `futures::future::Shared` future;
    // late callers clone+await it instead of re-invoking `refresh`. Leader clears the slot after
    // completion so a later call starts a fresh attempt. The *decision* of whether to call this
    // at all (generation counter) lives in `handle::AccountEntry`, not here.
}

// discovery.rs — pure helper, does NOT write files, does NOT spawn a CLI
pub struct DiscoveryLocation { pub description: String, pub path: PathBuf }
pub fn existing(candidates: Vec<DiscoveryLocation>) -> Vec<DiscoveryLocation>;
pub fn under_home(home: &Path, relative: &str) -> PathBuf;

// redact.rs — defense in depth for tracing
pub fn redact(input: &str) -> String;
// [Wave 2 addition] tracing MakeWriter wrapper the app can install:
pub struct RedactingMakeWriter<M> { /* wraps any tracing_subscriber::fmt::MakeWriter */ }
impl<M> RedactingMakeWriter<M> {
    pub fn new(inner: M) -> Self;
}
// impl<'a, M: MakeWriter<'a>> MakeWriter<'a> for RedactingMakeWriter<M>; usage:
//   tracing_subscriber::fmt().with_writer(RedactingMakeWriter::new(std::io::stderr)).init();

// oauth.rs — complete (Wave 2): PKCE + authorization URL builder + loopback server + device code +
// token exchange/refresh, all implemented + tested (wiremock for token endpoints; loopback over a
// real TCP socket — passes in this sandbox, but ported callers shouldn't assume that if a
// different environment blocks loopback).
pub struct PkceCodes { pub verifier: SecretString, pub challenge: String, pub challenge_method: &'static str }
pub fn generate_pkce() -> PkceCodes;
pub struct AuthorizationUrlParams<'a> { authorize_endpoint, client_id, redirect_uri, scope, state,
                                         code_challenge, code_challenge_method, extra_params: &'a [(&'a str, &'a str)] }
pub fn build_authorization_url(params: AuthorizationUrlParams<'_>) -> Result<url::Url, AuthError>;
pub struct AuthorizationCallback { pub code: SecretString, pub state: String }
pub async fn run_loopback_callback_server(expected_state: &str, timeout: Duration) -> Result<AuthorizationCallback, AuthError>;
pub struct LoopbackServer { /* 127.0.0.1-only TcpListener */ }
impl LoopbackServer {
    pub async fn bind(port: Option<u16>, callback_path: &str, redirect_host: &str) -> Result<Self, AuthError>;
    pub fn redirect_uri(&self) -> &str;
    pub async fn wait_for_callback(self, expected_state: &str, timeout: Duration) -> Result<AuthorizationCallback, AuthError>;
    // Accepts exactly one GET on callback_path, constant-time state compare, error= param surfaced,
    // small HTML reply, closes.
}
pub struct DeviceCodeSession { pub device_code: SecretString, pub user_code: String, pub verification_uri: String,
                                pub interval: Duration, pub expires_in: Duration }
pub async fn start_device_code_flow(http: &reqwest::Client, device_authorization_endpoint: &str, client_id: &str, scope: &str) -> Result<DeviceCodeSession, AuthError>;
pub struct TokenResponse { pub access_token: SecretString, pub refresh_token: Option<SecretString>,
                            pub expires_in: Option<Duration>, pub token_type: String, pub raw: serde_json::Value }
pub async fn poll_device_code_token(http: &reqwest::Client, token_endpoint: &str, session: &DeviceCodeSession, client_id: &str) -> Result<TokenResponse, AuthError>;
// RFC 8628: authorization_pending -> retry; slow_down -> interval += 5s; expired_token/access_denied -> Err.
pub async fn exchange_code_for_token(http: &reqwest::Client, token_endpoint: &str, client_id: &str, redirect_uri: &str, code: &SecretString, pkce_verifier: &SecretString) -> Result<TokenResponse, AuthError>;
pub async fn refresh_access_token(http: &reqwest::Client, token_endpoint: &str, client_id: &str, refresh_token: &SecretString) -> Result<TokenResponse, AuthError>;
// [Wave 3 addition] additive `client_secret: Option<&SecretString>` variants — some providers
// register their PKCE "installed app" OAuth client as confidential anyway (e.g. Google's
// Desktop-app client type: agy's `antigravity` transport) and reject the token request without
// one. The plain functions above are now thin wrappers calling these with `client_secret: None`;
// existing callers are unaffected.
pub async fn exchange_code_for_token_with_secret(http: &reqwest::Client, token_endpoint: &str, client_id: &str, client_secret: Option<&SecretString>, redirect_uri: &str, code: &SecretString, pkce_verifier: &SecretString) -> Result<TokenResponse, AuthError>;
pub async fn refresh_access_token_with_secret(http: &reqwest::Client, token_endpoint: &str, client_id: &str, client_secret: Option<&SecretString>, refresh_token: &SecretString) -> Result<TokenResponse, AuthError>;
```

---

## 3. `xlightcli-provider`

Depends only on `protocol` + `auth` (no dependency on `config`). Traits used via `dyn` →
`#[async_trait]` (D-006).

```rust
pub type EventStream = BoxStream<'static, Result<AgentEvent, ProviderError>>;

#[async_trait]
pub trait TransportAdapter: Send + Sync {
    fn id(&self) -> TransportId;
    fn stability(&self) -> Stability;
    fn required_auth(&self) -> AuthKind;
    fn capabilities(&self) -> &ProviderCapabilities;
    fn protocol_version(&self) -> ProtocolVersion;
    async fn list_models(&self, cred: &CredentialHandle) -> Result<Vec<ModelInfo>, ProviderError>;
    async fn quota(&self, cred: &CredentialHandle) -> Result<Option<QuotaSnapshot>, ProviderError>;
    fn apply_effort(&self, req: &mut TurnRequest, effort: ReasoningEffort);
    async fn stream(&self, req: TurnRequest, cred: CredentialHandle, cancel: CancellationToken) -> Result<EventStream, ProviderError>;
}

pub trait Provider: Send + Sync {
    fn id(&self) -> ProviderId;
    fn display_name(&self) -> &str;
    fn auth(&self) -> &dyn AuthAdapter;
    fn transports(&self) -> &[Arc<dyn TransportAdapter>];
    fn features(&self) -> &dyn ProviderFeaturePack;
    fn importer(&self) -> Option<&dyn ConfigImporter> { None }
    fn transport(&self, id: &TransportId) -> Option<&Arc<dyn TransportAdapter>>; // default impl provided
}

// Command system — minimal placeholder for Phase 0; Phase 2 will EXTEND (not break) this shape:
pub struct CommandDefinition { pub id: CommandId, pub alias: &'static str, pub mode: CapabilityMode,
                                pub requires_transport: Option<TransportId>, pub summary: &'static str }
pub struct ProviderCommand { pub id: CommandId, pub raw_args: String }
pub struct CommandContext { pub session: SessionId }
pub struct RichText(pub String);
pub struct TurnRequestPatch { pub extra_system_text: Option<String>, pub extra_user_text: Option<String> }
pub enum CommandResult { Message(RichText), StartTurn(TurnRequestPatch), Unavailable { reason: String } }
#[derive(thiserror::Error)] pub enum CommandError { Failed(String) }

#[async_trait]
pub trait ProviderFeaturePack: Send + Sync {
    fn commands(&self) -> Vec<CommandDefinition>;
    async fn execute(&self, cmd: ProviderCommand, ctx: CommandContext) -> Result<CommandResult, CommandError>;
}

#[async_trait]
pub trait ConfigImporter: Send + Sync {
    fn source_name(&self) -> &'static str;
    async fn import(&self) -> Result<ConfigFragment, ProviderError>;
}

pub struct ProviderRegistry;
impl ProviderRegistry {
    pub fn new() -> Self;
    pub fn register(&mut self, provider: Arc<dyn Provider>);
    pub fn get(&self, id: &ProviderId) -> Option<&Arc<dyn Provider>>;
    pub fn ids(&self) -> impl Iterator<Item = &ProviderId>;
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Provider>>;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
}

// sse.rs — incremental SSE parser, ALREADY implemented + thoroughly tested (CRLF, lone CR,
// multi-line data, comments, event/id/retry, arbitrary chunk boundaries, error propagation).
pub struct SseEvent { pub event: Option<String>, pub data: String, pub id: Option<String>, pub retry: Option<u64> }
pub fn parse<S>(inner: S) -> SseParser<S> where S: Stream<Item = Result<Bytes, ProviderError>>;
impl<S: Stream<Item = Result<Bytes, ProviderError>> + Unpin> Stream for SseParser<S> { type Item = Result<SseEvent, ProviderError>; }

// http.rs
pub struct HttpClientConfig { pub user_agent: String, pub connect_timeout: Duration, pub request_timeout: Duration }
pub fn build_client(config: &HttpClientConfig) -> Result<reqwest::Client, ProviderError>;

// error.rs
pub const BODY_EXCERPT_LIMIT: usize; // 512
pub fn body_excerpt(body: &str) -> String;
pub fn map_status(status: u16, headers: &http::HeaderMap, body: &str) -> ProviderError;

// retry.rs
pub struct Backoff; // ::new(base, max), .delay_for_attempt(attempt: u32) -> Duration, ::default()

// gate.rs — does NOT depend on xlightcli-config; the caller (provider-*) reads
// ExperimentalFlags itself and passes the resulting bool in here.
pub struct TransportGate;
impl TransportGate {
    pub fn new(transport: TransportId, stability: Stability) -> Self;
    pub fn disable(&self); pub fn enable(&self); pub fn is_disabled(&self) -> bool;
    pub fn stability(&self) -> Stability;
    pub fn ensure_enabled(&self, experimental_opt_in: bool) -> Result<(), ProviderError>;
}

// testing.rs — feature "testing" (pulls in xlightcli-auth/testing)
pub struct MockTransport; impl MockTransport { pub fn new(id: impl Into<TransportId>, script: Vec<Result<AgentEvent, ProviderError>>) -> Self; }
pub struct MockProvider; impl MockProvider { pub fn scripted(provider_id: impl Into<ProviderId>, transport_id: impl Into<TransportId>, script: Vec<Result<AgentEvent, ProviderError>>) -> Self; }
pub async fn parse_sse_fixture(path: &Path) -> Result<Vec<SseEvent>, ProviderError>;
```

---

## 4. `xlightcli-config` (minimal, Phase 0)

```rust
// paths.rs — XDG on both Linux/macOS (D-011), honors XDG_* env vars (empty = treated as unset)
pub fn config_dir() -> PathBuf;      // $XDG_CONFIG_HOME/xlightcli, default ~/.config/xlightcli
pub fn data_dir() -> PathBuf;        // $XDG_DATA_HOME/xlightcli
pub fn state_dir() -> PathBuf;       // $XDG_STATE_HOME/xlightcli
pub fn global_config_file() -> PathBuf;  // config_dir()/config.toml
pub fn database_path() -> PathBuf;       // data_dir()/xlightcli.db
pub fn artifacts_dir() -> PathBuf;       // data_dir()/artifacts
pub fn worktrees_dir() -> PathBuf;       // data_dir()/worktrees
pub fn logs_dir() -> PathBuf;            // state_dir()/logs
pub fn project_config_dir(repo_root: &Path) -> PathBuf; // <repo>/.xlightcli

// flags.rs
pub struct ExperimentalFlags { pub claude_subscription: bool, pub antigravity_subscription: bool } // Default = both off
pub struct Config { pub default_provider: Option<ProviderId>, pub experimental: ExperimentalFlags,
                     pub disabled_transports: Vec<TransportId> }
impl Config { pub fn transport_allowed(&self, transport_id: &TransportId) -> bool; }
```

## 4.1 `xlightcli-storage` (Wave 2: `AccountIndex` only)

Depends only on `protocol`. Migration `crates/storage/src/migrations/0001_accounts.sql` (the
`accounts` table, docs/PLAN.md §11.2 — has NO secret column). A blocking `rusqlite::Connection`
(WAL) behind `tokio::task::spawn_blocking` — not yet Phase 1's real persistence worker thread
(PATTERNS.md §10); the doc comment in `lib.rs`/`accounts.rs` says so explicitly.

```rust
pub struct AccountRecord {
    pub provider: ProviderId,
    pub transport: TransportId,
    pub account_id: String,
    pub label: Option<String>,
    pub auth_kind: AuthKind,
    pub expiry: Option<time::OffsetDateTime>,
    pub keyring_ref: String,           // lookup key for SecretStore — not a secret
    pub metadata: serde_json::Value,
}

pub struct AccountIndex { /* Clone, Arc<Mutex<Connection>> */ }
impl AccountIndex {
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self, StorageError>;
    pub async fn upsert(&self, record: AccountRecord) -> Result<(), StorageError>;
    pub async fn list(&self, provider: Option<ProviderId>) -> Result<Vec<AccountRecord>, StorageError>;
    pub async fn get(&self, provider: &ProviderId, transport: &TransportId, account_id: &str) -> Result<Option<AccountRecord>, StorageError>;
    pub async fn set_default(&self, provider: &ProviderId, transport: &TransportId, account_id: &str) -> Result<(), StorageError>; // exclusive per (provider, transport)
    pub async fn get_default(&self, provider: &ProviderId, transport: &TransportId) -> Result<Option<AccountRecord>, StorageError>;
    pub async fn delete(&self, provider: &ProviderId, transport: &TransportId, account_id: &str) -> Result<(), StorageError>;
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    Sqlite(#[from] rusqlite::Error),
    Join(#[from] tokio::task::JoinError),
    InvalidStoredData(String),
    AccountNotFound { provider: String, transport: String, account_id: String },
}
```

The rest of `storage` (`db`, `worker`, `events`, `artifacts`, `query` for the full schema) is still
an empty skeleton — Phase 1.

---

## 5. Adapter crate constructor (identical across all 3 crates)

```rust
// provider-codex
pub struct CodexProvider;
impl CodexProvider { pub fn new(http: reqwest::Client, flags: &xlightcli_config::ExperimentalFlags) -> Self; }

// provider-claude (feature "claude-subscription", NOT default)
pub struct ClaudeProvider;
impl ClaudeProvider { pub fn new(http: reqwest::Client, flags: &xlightcli_config::ExperimentalFlags) -> Self; }

// provider-agy (feature "antigravity-subscription", NOT default)
pub struct AgyProvider;
impl AgyProvider { pub fn new(http: reqwest::Client, flags: &xlightcli_config::ExperimentalFlags) -> Self; }
```

Each adapter also exposes an endpoint override (Wave 2, used by wiremock tests and for pointing a
transport at a verified/alternate base URL). All fields have `Default` values = the constants in the
crate's `consts.rs` (marked UNVERIFIED until live-checked):

```rust
pub struct CodexEndpoints  { /* chatgpt/openai-api base URLs, OAuth endpoints, … */ }  impl Default
pub struct ClaudeEndpoints { /* anthropic_api_base_url, oauth_*, … */ }                impl Default
pub struct AgyEndpoints    { /* gemini/cloud-code base URLs, oauth_*, antigravity_project_id override */ } impl Default
impl CodexProvider  { pub fn with_endpoints(self, endpoints: CodexEndpoints) -> Self; }
impl ClaudeProvider { pub fn with_endpoints(self, endpoints: ClaudeEndpoints) -> Self; }
impl AgyProvider    { pub fn with_endpoints(self, endpoints: AgyEndpoints) -> Self; }
```

The `xlightcli` binary forwards the experimental features: `claude-subscription`,
`antigravity-subscription`, and `experimental` (both).

Modules follow the PATTERNS.md §5 layout, named exactly per CODEBASE.md §2 (all implemented in Wave 2):

| Crate | Modules |
|-------|------------------------------|
| `provider-codex` | `auth`, `wire::{request,response}`, `transport_chatgpt`, `transport_api`, `quota`, `import`, `features` |
| `provider-claude` | `auth`, `wire::{request,response}`, `transport_api`, `transport_subscription` (feature-gated), `quota`, `import`, `features::insights` |
| `provider-agy` | `auth`, `wire::{request,response}`, `transport_gemini`, `transport_antigravity` (feature-gated), `quota`, `import`, `features` |

No crate has a real `impl Provider`/`impl TransportAdapter` yet — that's Wave 2's first task, after
`docs/providers/<provider>.md` has been written.

---

## 6. Notes for implementing Wave 2

1. **Don't change a public signature** listed above without updating this file. If a change is
   unavoidable (e.g. `ProviderCommand`/`CommandContext` once Phase 2 is really implemented),
   describe the design first per AGENTS.md §4.2 and update `docs/PLAN.md`/`CODEBASE.md`.
2. `CredentialHandle::for_tests` (feature `testing`) is enough to test a transport without a real
   `AuthBroker` — use it in `provider-*` tests instead of hand-rolling a mock.
3. `provider::testing::MockProvider`/`MockTransport` (feature `testing`) are used by `runtime`
   tests later; `parse_sse_fixture` is used for wire translator tests.
4. Every stub returns `AuthError::NotImplemented(...)` / `bail!("... not implemented yet (Wave 2)")`
   — grep for `NotImplemented` or `Wave 2` to find every spot that still needs implementing.
5. `TransportGate` doesn't read config — the adapter reads `ExperimentalFlags`/
   `Config::disabled_transports` itself and passes the resulting bool into `ensure_enabled`.
6. Run before committing: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`
   (and again with `--all-features`), `cargo test --workspace` (and `--all-features`).
