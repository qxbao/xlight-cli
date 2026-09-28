# CONTRACTS.md — Public API after Wave 1 (Phase 0) + Phase 1 Wave A

> Summary of the public API of `protocol`, `auth`, `provider`, `config`, and each adapter's
> constructor, so Wave 2 agents can work in parallel without reading all the code. §1-§6 are the
> original Phase 0 (Wave 1) contracts; §7-§12 are the Phase 1 Wave A additions (`config`, `storage`,
> `tools`, `runtime`, `tui`, `app`). Design details: [`docs/PLAN.md`](PLAN.md); mandatory patterns:
> [`../PATTERNS.md`](../PATTERNS.md).
>
> Convention: every API below already **builds + clippy (`-D warnings`) + fmt clean**, both with
> and without `--all-features`. The signature is the source of truth; Wave B **must not change a
> public signature** without updating this file + reporting back to the team lead.

## 0. Status by crate

| Crate | Status | What Wave B does |
|-------|-----------|----------------|
| `xlightcli-protocol` | **usable** — full canonical types incl. `WorkspaceId` (Wave A addition), serde roundtrip tests | Extend when a new field is needed (rare) |
| `xlightcli-config` | **usable** (Wave A) — full Phase 1 schema, `PartialConfig` per layer, deterministic `merge`/`resolve`, `Origin` tracking, `ConfigLoader`, `TrustStore`, minimal env-var layer; see §7 | Wire into `app`; extend the env-var allowlist as needed; richer per-leaf `Origin` if `config show --origin` needs it |
| `xlightcli-storage` | **usable** (Wave A) — `AccountIndex` (Wave 2, unchanged) + `Storage` (writer thread, migration `0002_sessions.sql`, full event-sourced schema); see §8 | Real `messages`-from-`events` projection if needed beyond the Wave A explicit-write scope; more query helpers as `ContextManager` needs them |
| `xlightcli-auth` | **usable** (Wave 2) — keyring/file store, loopback server, device code, single-flight refresh with generation counter, `AuthBroker::credential/login/import/accounts/logout` all implemented | — |
| `xlightcli-provider` | **complete infra** — trait, SSE parser, http, retry, gate, `map_status`, `testing` | Don't change the trait; just use it |
| `xlightcli-provider-{codex,claude,agy}` | skeleton — struct + constructor + empty modules | Implement `auth`, `wire`, `transport_*`, `quota`, `import`, `features` + `impl Provider` |
| `xlightcli-tools` | **usable** (Wave B) — `Tool`/`ToolContext`/`ToolRegistry`, `PermissionEngine`/`PermissionGate` (real rule evaluation), `ProcessLauncher`/`OutputSpool` (real, tested); all 9 built-in tools have real, tested `run` bodies; `WorkspacePath::resolve` now also canonicalizes existing ancestors to catch a symlink escape; see §9 | Nothing changes once `runtime` wires a real `PermissionGate`/`ToolContext` construction path |
| `xlightcli-mcp` | empty skeleton (doc comment) | Phase 3 |
| `xlightcli-runtime` | **usable** (Wave B) — `RuntimeHandle` (real channel/session plumbing, real core-command bodies, real `submit_user_input`/`respond_to_permission`/`cancel_turn`), `AgentLoop::run_turn` (real multi-step tool loop + permission gate + partial-output-on-error), `ContextManager::build_turn_request` (real), `exec::run_exec` (real); see §10 for the exact (changed) shapes | Later: real LLM-based compaction + persisted `summaries`, `/compact`/`/login`/`/logout` bodies, workspace-trust check for `--dangerously-skip-permissions`, `stream-json` incremental rendering (app-side) |
| `xlightcli-tui` | **usable** (Wave A) — real terminal lifecycle (`run`), `App`/`view::*`/`Keymap`/`Theme` module skeleton, no rendering; see §11 | Implement `ratatui` rendering for each view |
| `xlightcli` (app), `xtask` | **usable** (Wave A) — `exec`/`init` CLI surface + bare-TUI entry wired to real `RuntimeHandle` construction; `exec`'s turn + `init`'s wizard are stubs; see §12 | Nothing changes once `runtime`'s stubs are filled in |

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

---

## 7. `xlightcli-config` (Phase 1 Wave A)

Full schema `crate::schema`, per-layer `crate::partial::PartialConfig`, deterministic merge
`crate::merge`, `Origin` tracking `crate::origin`, minimal env-var layer `crate::env`, workspace
trust `crate::trust`, orchestration `crate::loader::ConfigLoader`. All real, tested code (not
stubs) — see the crate's `lib.rs` doc for the exact scope decisions (env-var allowlist, `Origin`
section-level granularity).

```rust
// schema.rs — the fully-resolved config; every sub-type here has a matching `Partial*` in
// partial.rs (same field set, everything `Option`).
pub struct Config {
    pub default_provider: Option<ProviderId>,
    pub provider: BTreeMap<String, ProviderDefaults>,     // [provider.<id>]
    pub experimental: ExperimentalFlags,                   // unchanged from Wave 1 (flags.rs)
    pub disabled_transports: Vec<TransportId>,
    pub permissions: PermissionsConfig,
    pub agents: AgentsConfig,
    pub concurrency: ConcurrencyConfig,
    pub budget: BudgetConfig,                              // wraps `default: BudgetDefaults`
    pub context: ContextConfig,
    pub tools: ToolsConfig,
    pub auth: AuthConfig,
    pub rules: RulesConfig,
    pub ui: UiConfig,
}
pub struct ProviderDefaults { pub transport: Option<TransportId>, pub default_model: Option<ModelId> }

pub enum ExecutionMode { Default, AcceptEdits, Plan }               // D-025; re-exported by `tools`/`runtime`
pub enum PermissionMode { ReadOnly, Strict, Ask, AutoEdit, FullAuto } // default Ask; .rank() for trust checks
pub enum RuleEffect { Allow, Ask, Deny }
pub struct PermissionRule { pub effect: RuleEffect, pub action: String, pub target: String }
impl PermissionRule {
    pub fn parse(effect: RuleEffect, raw: &str) -> Result<Self, ConfigError>; // "action(target)"
    pub fn to_raw(&self) -> String;
}
pub struct PermissionsConfig { pub mode: PermissionMode, pub allow: Vec<String>, pub ask: Vec<String>, pub deny: Vec<String> }
impl PermissionsConfig {
    pub fn rules(&self) -> Vec<PermissionRule>;                    // parsed, deny-then-ask-then-allow order
    pub fn rules_checked(&self) -> Vec<Result<PermissionRule, ConfigError>>;
}

pub struct AgentsConfig { pub max_active: u32, pub max_depth: u32, pub allow_provider_override: bool } // 8, 4, false
pub struct ConcurrencyConfig { pub llm_requests: u32, pub shell_jobs: u32, pub browser_jobs: u32 }      // 4, 4, 1
pub struct BudgetDefaults { pub max_turns: u32, pub max_output_tokens: u64, pub max_wall_clock: String } // 50, 200_000, "30m"
impl BudgetDefaults { pub fn max_wall_clock_duration(&self) -> Result<std::time::Duration, ConfigError>; }
pub struct BudgetConfig { pub default: BudgetDefaults }
pub struct ContextConfig { pub compaction_threshold: f32 }          // 0.8
pub struct ToolsConfig { pub spool_head_bytes: u64, pub spool_tail_bytes: u64, pub shell_timeout_secs: u64, pub env_passthrough: Vec<String> } // 8KiB, 32KiB, 120, []
pub enum AuthStoreKind { Keyring, File }                            // separate from xlightcli_auth::StoreKind (config can't depend on auth)
pub struct AuthConfig { pub store: AuthStoreKind }
pub struct RulesConfig { pub sources: Vec<String> }                 // AGENTS.md, AGENTS.override.md, CLAUDE.md, GEMINI.md, .agents/rules/*.md, .xlightcli/rules/*.md
pub struct UiConfig { pub keybind: BTreeMap<String, String>, pub theme: Option<String> }

// origin.rs — section-level granularity (documented scope decision; see module doc)
pub enum Origin { Default, Global, Project, ProjectLocal, Env, Cli } // Ord matches layer precedence
pub struct OriginMap;
impl OriginMap { pub fn get(&self, section: &str) -> Origin; pub fn iter(&self) -> impl Iterator<Item = (&'static str, Origin)>; }

// loader.rs
pub struct LoadedConfig { pub config: Config, pub origins: OriginMap }
pub struct ConfigLoader;
impl ConfigLoader {
    pub fn new(global_path: impl Into<PathBuf>) -> Self;
    pub fn with_project_dir(self, dir: impl Into<PathBuf>) -> Self;
    pub async fn load(&self, trust: &TrustStore, repo_root: Option<&Path>,
                       env_vars: impl Iterator<Item = (String, String)>,
                       cli_overrides: PartialConfig) -> Result<LoadedConfig, ConfigError>;
    // Not async in the current impl (no IO inside is actually async — kept sync); callers may
    // `.await` a wrapping async fn regardless. Layer order: global -> project -> project.local ->
    // env -> cli. `[experimental]` is stripped from project/project.local unconditionally;
    // `crate::trust::filter_untrusted` additionally strips other sensitive keys when untrusted.
}

// trust.rs
pub struct TrustStore;
impl TrustStore {
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, ConfigError>;
    pub fn is_trusted(&self, repo_root: &Path) -> bool;
    pub fn trust(&self, repo_root: &Path) -> Result<(), ConfigError>;
    pub fn revoke(&self, repo_root: &Path) -> Result<(), ConfigError>;
}
pub fn filter_untrusted(partial: PartialConfig) -> PartialConfig;

// env.rs — recognizes a fixed, documented subset: XLIGHTCLI_DEFAULT_PROVIDER,
// XLIGHTCLI_EXPERIMENTAL_{CLAUDE_SUBSCRIPTION,ANTIGRAVITY_SUBSCRIPTION}, XLIGHTCLI_PERMISSIONS_MODE,
// XLIGHTCLI_AUTH_STORE, XLIGHTCLI_TOOLS_SHELL_TIMEOUT_SECS. Unrecognized XLIGHTCLI_* -> ignored.
pub fn partial_from_env(vars: impl Iterator<Item = (String, String)>) -> PartialConfig;

// error.rs
pub enum ConfigError { Io { .. }, Toml { .. }, TomlSerialize { .. }, InvalidRule(String), InvalidDuration(String), Trust(String) }
```

`PartialConfig` (partial.rs) mirrors every field above as `Option<...>` (nested tables as
`Option<PartialXConfig>`); `crate::merge::merge(base, over)` deep-merges tables and **replaces**
(never concatenates) arrays; `crate::merge::resolve(partial)` fills every remaining `None` with the
hardcoded default shown above.

---

## 8. `xlightcli-storage` (Phase 1 Wave A addition: `Storage`)

`AccountIndex` (Wave 2) is unchanged. New: `Storage`, the writer-thread-backed handle for the full
event-sourced schema (migration `crates/storage/src/migrations/0002_sessions.sql`: `workspaces`,
`sessions`, `agents`, `events`, `messages`, `tool_calls`, `artifacts`, `summaries`,
`provider_usage`). Depends on the new `xlightcli_protocol::WorkspaceId` (Wave A addition to
`protocol`, `uuid_id!`-based like `SessionId`/`AgentId`).

```rust
#[derive(Clone)] pub struct Storage; // cheap to clone: shares the writer channel + reader connection
impl Storage {
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self, StorageError>;
    // Opens the write connection + migrates, opens a second reader connection, spawns the writer
    // thread, and calls mark_interrupted_on_open() automatically.

    pub async fn create_workspace(&self, root: PathBuf, repo_id: Option<String>) -> Result<WorkspaceId, StorageError>;
    pub async fn create_session(&self, workspace_id: WorkspaceId, provider: ProviderId, transport: TransportId,
                                 model: ModelId, title: Option<String>) -> Result<SessionId, StorageError>;
    pub async fn list_sessions(&self, workspace_id: Option<WorkspaceId>) -> Result<Vec<SessionRecord>, StorageError>;
    pub async fn get_session(&self, session_id: SessionId) -> Result<Option<SessionRecord>, StorageError>;
    pub async fn update_session_status(&self, session_id: SessionId, status: SessionStatus) -> Result<(), StorageError>;
    pub async fn mark_interrupted_on_open(&self) -> Result<u64, StorageError>; // marks every `active` session `interrupted`

    pub async fn register_agent(&self, session_id: SessionId, parent_id: Option<AgentId>,
                                 profile: serde_json::Value) -> Result<AgentId, StorageError>;
    pub async fn get_agent(&self, agent_id: AgentId) -> Result<Option<AgentRecord>, StorageError>;
    pub async fn list_agents(&self, session_id: SessionId) -> Result<Vec<AgentRecord>, StorageError>;

    pub async fn append_events(&self, session_id: SessionId, events: Vec<NewEvent>) -> Result<Vec<i64>, StorageError>;
    // Atomic batch insert; assigns a per-session-monotonic `seq` to each. `events` is the
    // append-only source of truth (docs/PLAN.md §11.2).
    pub async fn list_events(&self, session_id: SessionId) -> Result<Vec<EventRecord>, StorageError>;

    // `messages` scope decision (Wave A, documented in storage.rs module doc): NOT auto-derived
    // from `events` by the writer thread. Callers write both explicitly — call this once per
    // complete `Message` (e.g. from `AgentEvent::Completed`, plus the user's own turn).
    pub async fn append_message(&self, session_id: SessionId, agent_id: AgentId, turn: i64,
                                 role: Role, content: Vec<ContentBlock>) -> Result<i64, StorageError>;
    pub async fn load_messages(&self, session_id: SessionId, page: MessagePage) -> Result<Vec<MessageRecord>, StorageError>;
    // `MessagePage { before_id: Option<i64>, limit: u32 }`; returns oldest-first within the page.

    pub async fn record_tool_call(&self, call: NewToolCall) -> Result<i64, StorageError>;
    pub async fn finish_tool_call(&self, id: i64, status: ToolCallStatus, artifact_id: Option<i64>) -> Result<(), StorageError>;
    pub async fn list_tool_calls(&self, agent_id: AgentId) -> Result<Vec<ToolCallRecord>, StorageError>;

    pub async fn register_artifact(&self, artifact: NewArtifact) -> Result<i64, StorageError>;
    pub async fn list_artifacts(&self, session_id: SessionId) -> Result<Vec<ArtifactRecord>, StorageError>;

    pub async fn record_usage(&self, row: NewUsageRow) -> Result<i64, StorageError>;
    pub async fn list_usage(&self, session_id: SessionId) -> Result<Vec<UsageRecord>, StorageError>;
}

pub enum SessionStatus { Active, Completed, Interrupted, Failed }
pub enum AgentState { Queued, Running, WaitingTool, WaitingPermission, Done, Failed, Cancelled }
pub enum StoredEventKind { TurnStarted, TextDelta, ReasoningDelta, ToolCallStarted, ToolCalled,
                            ToolCompleted, Usage, RateLimit, TurnCompleted, AgentCompleted, Interrupted }
pub enum ToolCallStatus { Running, Succeeded, Failed, Cancelled }

pub struct NewEvent { pub agent_id: AgentId, pub kind: StoredEventKind, pub payload: serde_json::Value }
pub struct EventRecord { pub id: i64, pub session_id: SessionId, pub agent_id: AgentId, pub seq: i64,
                          pub kind: StoredEventKind, pub payload: serde_json::Value, pub created_at: OffsetDateTime }
pub struct SessionRecord { pub id: SessionId, pub workspace_id: WorkspaceId, pub provider: ProviderId,
                            pub transport: TransportId, pub model: ModelId, pub title: Option<String>,
                            pub created_at: OffsetDateTime, pub updated_at: OffsetDateTime, pub status: SessionStatus }
pub struct MessageRecord { pub id: i64, pub session_id: SessionId, pub agent_id: AgentId, pub turn: i64,
                            pub role: Role, pub content: Vec<ContentBlock>, pub created_at: OffsetDateTime }
pub struct MessagePage { pub before_id: Option<i64>, pub limit: u32 }
pub struct NewToolCall { pub agent_id: AgentId, pub call_id: ToolCallId, pub name: String, pub input: serde_json::Value }
pub struct ToolCallRecord { /* + status, artifact_id, started_at, finished_at */ }
pub struct NewArtifact { pub session_id: SessionId, pub path: PathBuf, pub bytes: u64, pub sha256: String, pub kind: String }
pub struct ArtifactRecord { /* + id, created_at */ }
pub struct NewUsageRow { pub session_id: SessionId, pub agent_id: AgentId, pub transport: TransportId,
                          pub model: ModelId, pub usage: xlightcli_protocol::Usage }
pub struct UsageRecord { /* + id, input/output/cached_tokens, at */ }
pub struct AgentRecord { pub id: AgentId, pub session_id: SessionId, pub parent_id: Option<AgentId>,
                          pub profile: serde_json::Value, pub state: AgentState,
                          pub created_at: OffsetDateTime, pub finished_at: Option<OffsetDateTime> }
pub struct WorkspaceRecord { pub id: WorkspaceId, pub root: PathBuf, pub repo_id: Option<String>, pub created_at: OffsetDateTime }

pub enum StorageError { Sqlite(..), Join(..), InvalidStoredData(String), AccountNotFound { .. }, WriterUnavailable }
```

Writer-thread scope decision (documented in `crate::worker` module doc): each `StorageCmd` commits
in its own sqlite transaction (a multi-row command like `AppendEvents` is still atomic across its
own rows); coalescing several distinct commands into one shared transaction (PATTERNS.md §10's "N
events or 50 ms" batching) is a pure writer-thread implementation detail Wave B can add without an
API change.

---

## 9. `xlightcli-tools` (Phase 1 Wave A)

```rust
// tool.rs
pub enum ToolEffect { ReadOnly, WritesWorkspace, Executes, Network }
pub enum ToolOutput { Text(String), Structured(serde_json::Value), Spooled(SpooledOutput) }
impl ToolOutput { pub fn model_facing_text(&self) -> String; } // never dumps a Spooled output's full bytes

pub enum ToolError { InvalidInput(String), PermissionDenied(String), PathEscape(String),
                      Spawn(#[from] SpawnError), Io(#[from] std::io::Error), Cancelled,
                      NotImplemented { tool: &'static str, detail: &'static str } }

pub struct WorkspacePath;
impl WorkspacePath {
    pub fn new(root: impl Into<PathBuf>) -> Self;
    pub fn root(&self) -> &Path;
    pub fn resolve(&self, relative: impl AsRef<Path>) -> Result<PathBuf, ToolError>;
    // Wave B: lexical resolution (join + collapse `.`/`..`, must still start_with root) PLUS
    // canonicalizing the deepest existing ancestor to catch a symlink escape anywhere along the
    // path; the not-yet-existing trailing components (e.g. a fresh write_file target) are
    // re-joined onto the canonical ancestor unchanged. Falls back to the lexical-only result when
    // the root itself doesn't exist on disk (e.g. a synthetic root in a unit test).
}

pub struct ToolSpoolConfig { pub artifacts_dir: PathBuf, pub limits: SpoolLimits }
#[derive(Clone)] pub struct ToolContext {
    pub workspace: WorkspacePath, pub permissions: Arc<dyn PermissionGate>,
    pub launcher: Arc<ProcessLauncher>, pub cancel: CancellationToken,
    pub session_id: SessionId, pub call_id: ToolCallId, /* + private artifacts_dir/spool_limits */
}
impl ToolContext {
    pub fn new(workspace: WorkspacePath, permissions: Arc<dyn PermissionGate>, launcher: Arc<ProcessLauncher>,
               cancel: CancellationToken, session_id: SessionId, call_id: ToolCallId, spool: ToolSpoolConfig) -> Self;
    pub async fn check_permission(&self, action: PermissionAction<'_>) -> Result<(), ToolError>;
    pub async fn open_spool(&self) -> Result<OutputSpool, ToolError>;
    pub fn artifact_ref(&self, path: PathBuf) -> ArtifactRef;
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn definition(&self) -> &xlightcli_protocol::ToolDefinition;
    fn effect(&self, input: &serde_json::Value) -> ToolEffect;
    async fn run(&self, input: serde_json::Value, ctx: ToolContext) -> Result<ToolOutput, ToolError>;
}

// registry.rs
#[derive(Clone, Default)] pub struct ToolRegistry;
impl ToolRegistry {
    pub fn new() -> Self;
    pub fn register(&mut self, tool: Arc<dyn Tool>);
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>>;
    pub fn contains(&self, name: &str) -> bool;
    pub fn definitions(&self) -> Vec<xlightcli_protocol::ToolDefinition>;
    pub fn subset(&self, names: &[impl AsRef<str>]) -> Self;
    pub fn with_builtins() -> Self; // registers all 9 Phase-1 built-ins (see below)
}

// permission.rs — PermissionMode/ExecutionMode/PermissionRule/RuleEffect are re-exports of the
// xlightcli_config types (single definition, PATTERNS.md-style: config owns the persisted shape).
pub enum PermissionAction<'a> { ReadFile(&'a Path), WriteFile(&'a Path), Command(&'a str),
                                 ReadUrl(&'a str), Mcp(&'a str), Unsandboxed }
pub enum PermissionDecision { Allow, Ask { reason: String }, Deny { reason: String } }
pub struct PermissionRequest { pub action: String, pub target: String,
                                pub tool_call_id: Option<ToolCallId>, pub reason: String }
pub struct PermissionEngine;
impl PermissionEngine {
    pub fn new(mode: PermissionMode, rules: Vec<PermissionRule>) -> Self;
    pub fn from_config(cfg: &xlightcli_config::PermissionsConfig) -> Self;
    pub fn evaluate(&self, action: &PermissionAction<'_>) -> PermissionDecision;
    // Real rule matching: `*`/glob via `globset`, `regex:<pattern>` via `regex-lite`. Precedence
    // deny > ask > allow; falls back to a mode-driven default when nothing matches.
}
#[async_trait]
pub trait PermissionGate: Send + Sync + std::fmt::Debug {
    async fn check(&self, action: PermissionAction<'_>) -> Result<(), ToolError>;
}
pub struct StaticPermissionGate; // test/dev-only: evaluates synchronously, never prompts
pub enum AskPolicy { AutoAllow, AutoDeny }

// launcher.rs — the ONLY file with #[allow(clippy::disallowed_methods)] for Command::new.
pub const PROVIDER_CLI_BLOCKLIST: &[&str] = &["codex", "claude", "agy", "antigravity"];
pub enum SpawnPurpose { ShellTool, Mcp, Hook, Git }              // blocklist checked only for Mcp/Hook/Git
pub enum EnvPolicy { Scrubbed { passthrough: Vec<String> } }     // the only variant; no "inherit everything"
pub struct SpawnSpec { pub purpose: SpawnPurpose, pub program: String, pub args: Vec<String>,
                        pub cwd: PathBuf, pub env: EnvPolicy, pub timeout: Option<Duration>,
                        pub cancel: CancellationToken }
pub enum SpawnError { ForbiddenProgram { .. }, Spawn { .. }, Timeout(Duration), Cancelled, Io(..) }
#[derive(Debug, Clone, Copy, Default)] pub struct ProcessLauncher;
impl ProcessLauncher {
    pub fn new() -> Self;
    pub async fn spawn(&self, spec: SpawnSpec) -> Result<SpawnedProcess, SpawnError>;
    // New process group via tokio::process::Command::process_group(0) (safe, std-stable — no
    // unsafe needed, honoring [workspace.lints.rust] unsafe_code = "forbid").
}
pub struct SpawnedProcess;
impl SpawnedProcess {
    pub fn id(&self) -> Option<u32>;
    pub async fn pipe_into(self, spool: &mut OutputSpool) -> Result<std::process::ExitStatus, SpawnError>;
    // Streams combined stdout+stderr into `spool`; kills the WHOLE process group (rustix::process::
    // kill_process_group, also safe) on timeout/cancel.
}

// spool.rs
pub struct ArtifactRef { pub session_id: SessionId, pub call_id: ToolCallId, pub path: PathBuf }
pub struct SpoolLimits { pub head_bytes: usize, pub tail_bytes: usize } // default 8KiB/32KiB
impl SpoolLimits { pub fn from_config(cfg: &xlightcli_config::ToolsConfig) -> Self; }
pub struct SpooledOutput { pub head: Vec<u8>, pub tail: Vec<u8>, pub total_bytes: u64,
                            pub total_lines: u64, pub truncated: bool, pub artifact: ArtifactRef }
pub struct OutputSpool;
impl OutputSpool {
    pub async fn create(artifacts_dir: &Path, session_id: SessionId, call_id: ToolCallId,
                         limits: SpoolLimits) -> Result<Self, std::io::Error>;
    pub async fn write_chunk(&mut self, chunk: &[u8]) -> Result<(), std::io::Error>; // RAM bounded by limits, always
    pub async fn finish(self) -> Result<SpooledOutput, std::io::Error>;
}
```

Built-in tool structs (`crate::builtin`, all registered by `ToolRegistry::with_builtins`, every
`run` body implemented and integration-tested, `crates/tools/tests/{fs,grep,shell,git}.rs`):
`ReadFile`, `WriteFile`, `EditFile`, `ListDir`, `Glob` (`builtin::fs`); `Grep` (`builtin::grep`);
`Shell` (`builtin::shell`); `GitStatus`, `GitDiff` (`builtin::git`). Each has a real
`ToolDefinition` (JSON schema generated via `schemars` from its `*Args` struct). `read_file` streams
through `ctx.open_spool()` above 256 KiB and sniffs the first 8 KiB for a `\0` byte to reject binary
files; `edit_file` requires an exact, unique `old_string` match and returns a `similar` unified diff;
`glob`/`grep` walk via `ignore::WalkBuilder::new(root).require_git(false)` (so `.gitignore` is
honored even when the workspace root isn't itself a git checkout) capped at 5000 results/matches;
`shell` spawns `bash -lc "<command>"` via `ctx.launcher` and appends a trailing
`[exit code: N]`/`[terminated by signal]` marker to the spooled output; `git_status`/`git_diff` spawn
the real `git` binary via `ctx.launcher` (`SpawnPurpose::Git`) and surface a non-zero exit as
`ToolError::InvalidInput` with the captured output. Known gap: `shell`'s default timeout
(`DEFAULT_SHELL_TIMEOUT_SECS = 120`) is a local constant mirroring
`xlightcli_config::ToolsConfig::shell_timeout_secs`'s own default — `ToolContext` doesn't carry a
`ToolsConfig` reference, so a caller that wants the *configured* value must resolve it itself and
pass `timeout_secs` explicitly.

---

## 10. `xlightcli-runtime` (Phase 1 Wave B)

Never depends on a `provider-*` crate (INV-2, `cargo xtask check-deps`). Every `RuntimeHandle`
method signature below is **unchanged** from Wave A. `AgentLoop::run_turn`'s signature changed
(see below) — it is only ever called from `RuntimeHandle::submit_user_input`/`crate::exec::run_exec`
(both inside this crate), so the change isn't visible to `tui`/`app`.

```rust
// handle.rs
#[derive(Clone)] pub struct RuntimeDeps {
    pub providers: Arc<xlightcli_provider::ProviderRegistry>,
    pub auth: Arc<xlightcli_auth::AuthBroker>,
    pub tools: Arc<xlightcli_tools::ToolRegistry>,
    pub storage: xlightcli_storage::Storage,
    pub config: xlightcli_config::Config,
    // No `mcp` field (documented scope decision): xlightcli-mcp has no public type yet (Phase 3).
}
pub struct RuntimeConfig { pub ui_channel_capacity: usize } // default 256, impl Default
pub struct ToolCallSummary { pub name: String, pub text_preview: String, pub artifact: Option<xlightcli_tools::ArtifactRef> }
pub enum NoticeLevel { Info, Warn, Error }
pub enum UiEvent {
    TurnStarted { session_id: SessionId, agent_id: AgentId, model: ModelId },
    TextDelta { agent_id: AgentId, index: u32, text: String },
    ReasoningDelta { agent_id: AgentId, index: u32, text: String },
    ToolCallStarted { agent_id: AgentId, call_id: ToolCallId, name: String },
    ToolCallFinished { agent_id: AgentId, call_id: ToolCallId, summary: ToolCallSummary },
    PermissionRequested { agent_id: AgentId, request: xlightcli_tools::PermissionRequest },
    Usage { agent_id: AgentId, usage: Usage },
    RateLimit { agent_id: AgentId, info: RateLimitInfo },
    TurnCompleted { agent_id: AgentId, stop: StopReason },
    TurnFailed { agent_id: AgentId, error: String },
    SessionChanged { session_id: SessionId },
    ModeChanged { session_id: SessionId, mode: xlightcli_tools::ExecutionMode },
    Notice { level: NoticeLevel, message: String },
}
pub enum PermissionResponse { Allow, Deny }
pub enum CommandOutcome { Message(String), TurnStarted, Unavailable { reason: String } }

#[derive(Clone)] pub struct RuntimeHandle; // the ONLY API tui/app use
impl RuntimeHandle {
    pub fn new(deps: RuntimeDeps, config: RuntimeConfig) -> Self;
    pub fn deps(&self) -> &RuntimeDeps;
    pub fn commands(&self) -> &CommandRegistry;
    pub async fn subscribe(&self) -> Result<mpsc::Receiver<UiEvent>, RuntimeError>; // callable exactly once
    pub async fn create_session(&self, workspace_id: WorkspaceId, provider: ProviderId, transport: TransportId,
                                 model: ModelId, title: Option<String>) -> Result<SessionId, RuntimeError>;
    pub async fn list_sessions(&self) -> Result<Vec<xlightcli_storage::SessionRecord>, RuntimeError>;
    pub async fn resume_session(&self, session_id: SessionId) -> Result<(), RuntimeError>;
    pub async fn submit_user_input(&self, session_id: SessionId, text: String) -> Result<(), RuntimeError>;
    // Real (Wave B): registers/recovers the session's root agent (`ensure_agent`, crash-recovery
    // safe — reuses the previously-registered agent via `Storage::list_agents` instead of
    // duplicating it), then drives one `AgentLoop::run_turn`, streaming `UiEvent`s the whole way.
    pub async fn run_command(&self, session_id: SessionId, raw: &str) -> Result<CommandOutcome, RuntimeError>;
    // Real bodies (Wave B) for every core command except `/compact`/`/login`/`/logout`, which
    // return `Unavailable` with a clear reason (INV-10): persisted compaction summaries and an
    // interactive login/logout surface aren't implemented yet — see CODEBASE.md §2's `runtime` row.
    pub async fn respond_to_permission(&self, tool_call_id: ToolCallId, response: PermissionResponse) -> Result<(), RuntimeError>;
    // Real (Wave B): resolves the oneshot a `permission_gate::RuntimePermissionGate` is awaiting.
    pub async fn cancel_turn(&self, session_id: SessionId) -> Result<(), RuntimeError>;
    // Real (Wave B): cancels the session's in-flight-turn `CancellationToken`, if any.
    pub async fn set_execution_mode(&self, session_id: SessionId, mode: xlightcli_tools::ExecutionMode) -> Result<(), RuntimeError>;
}

// commands/ — CommandRegistry + Phase 1 core command ids
pub struct CommandRegistry;
impl CommandRegistry {
    pub fn with_core_commands() -> Self;
    pub fn resolve(&self, alias: &str) -> Option<&xlightcli_provider::CommandDefinition>;
    pub fn core_commands(&self) -> &[xlightcli_provider::CommandDefinition];
}
pub const CORE_COMMANDS: &[commands::core::CoreCommandSpec]; // help, exit, clear, resume, model,
    // context, compact, diff, permissions, config, status, login, logout, mode (Shift+Tab cycle)

// context.rs
pub fn estimate_tokens(text: &str) -> u64; // chars/4 heuristic (docs/PLAN.md §9.2 — no tokenizer dep)
pub fn estimate_message_tokens(message: &Message) -> u64; // [Wave B addition] per-`Message` estimate
pub struct ContextManager;
impl ContextManager {
    pub fn new(compaction_threshold: f32) -> Self;
    pub fn from_config(cfg: &xlightcli_config::ContextConfig) -> Self;
    // [Wave B addition] builds from the full `Config` so rule discovery honors `[rules].sources`;
    // `new`/`from_config` still work (default rule sources), purely additive.
    pub fn from_full_config(cfg: &xlightcli_config::Config) -> Self;
    // [Wave B addition] overrides where rule files are discovered from — `Storage` has no
    // `get_workspace` accessor yet, so `build_turn_request` defaults to the process cwd; tests
    // (and a future wave once that accessor exists) can override it explicitly.
    pub fn with_rule_root(self, root: impl Into<PathBuf>) -> Self;
    pub fn should_compact(&self, used_tokens: u64, context_window: u64) -> bool; // real
    // real (Wave B): base system prompt + discovered rule files, paged history (most recent 200
    // messages), tool definitions from `tools`.
    pub async fn build_turn_request(&self, storage: &xlightcli_storage::Storage,
        tools: &xlightcli_tools::ToolRegistry, session: &Session) -> Result<TurnRequest, RuntimeError>;
    // [Wave B addition] sum of `estimate_tokens`/`estimate_message_tokens` over a `TurnRequest`.
    pub fn estimate_request_tokens(&self, request: &TurnRequest) -> u64;
    // [Wave B addition] keeps the most recent 8 messages, folds everything older into one
    // deterministic placeholder `Message` (not a real LLM summary, not persisted — see CODEBASE.md
    // §2's `runtime` row for the scope decision).
    pub fn compact_messages(&self, messages: Vec<Message>, session_id: SessionId) -> Vec<Message>;
}

// agent.rs — [Wave B signature change, internal-only]: `run_turn` used to take
// `(&RuntimeDeps, &Session)`, which had no way to reach the UI channel / pending-permission map /
// per-turn CancellationToken (all live on RuntimeHandle's private state). It now takes a
// `TurnContext` bundle. Only `RuntimeHandle::submit_user_input`/`crate::exec::run_exec` (both
// inside this crate) call it, so this is invisible to `tui`/`app`.
pub struct TurnContext<'a> { pub deps: &'a RuntimeDeps, pub session: &'a Session, pub agent_id: AgentId,
    pub context: &'a ContextManager, pub permission_engine: xlightcli_tools::PermissionEngine,
    pub ui_tx: mpsc::Sender<UiEvent>, pub pending_permissions: PendingPermissions,
    pub cancel: CancellationToken, pub max_steps: u32,
    pub headless_ask_policy: Option<xlightcli_tools::AskPolicy> } // None = interactive (ask via UiEvent), Some(policy) = headless
pub struct TurnSummary { pub response_text: String, pub usage: Usage, pub stop: StopReason }
pub struct AgentLoop;
impl AgentLoop {
    pub async fn run_turn(ctx: TurnContext<'_>, user_text: String) -> Result<TurnSummary, RuntimeError>;
    // Real multi-step loop (docs/PLAN.md §9.1): builds the request via ContextManager, streams
    // through the transport, on `StopReason::ToolUse` runs each tool call through
    // `permission_gate::RuntimePermissionGate` (rules/mode + Plan/AcceptEdits execution-mode
    // overrides) and loops again; persists every message/tool-call/usage row. A provider failure
    // after some output was already produced returns `Ok(TurnSummary)` with `stop:
    // StopReason::Other("provider_error_after_partial_output: ...")` instead of `Err` (D-026 exit
    // code 3 vs 1 — see exec.rs's module doc for the app-side follow-up this needs).
}

// permission_gate.rs (private module; `AgentLoop`/`RuntimeHandle` are the only callers)
pub type PendingPermissions = Arc<tokio::sync::Mutex<HashMap<ToolCallId, tokio::sync::oneshot::Sender<PermissionResponse>>>>;
pub struct RuntimePermissionGate; // impl xlightcli_tools::PermissionGate

// exec.rs — D-026 headless contract
pub enum ExecOutputFormat { Text, Json, StreamJson }               // impl Default = Text
pub struct ExecOptions { pub prompt: String, pub output_format: ExecOutputFormat, pub model: Option<ModelId>,
    pub provider: Option<ProviderId>, pub transport: Option<TransportId>, pub mode: Option<xlightcli_tools::ExecutionMode>,
    pub continue_session: bool, pub resume_session: Option<SessionId>, pub dangerously_skip_permissions: bool,
    pub print_timeout: Duration, pub add_dir: Vec<PathBuf> }
impl ExecOptions { pub fn new(prompt: impl Into<String>) -> Self; }
pub struct ExecUsage { pub input_tokens: u64, pub output_tokens: u64, pub thinking_tokens: u64,
                        pub cache_read_tokens: u64, pub total_tokens: u64 } // impl From<xlightcli_protocol::Usage>
pub struct ExecOutput { pub conversation_id: SessionId, pub status: String, pub response: String, pub usage: ExecUsage }
pub enum ExecExitCode { Ok = 0, Error = 1, InvalidInput = 2, PartialError = 3 }
impl ExecExitCode { pub fn code(self) -> i32; }
pub async fn run_exec(handle: &RuntimeHandle, options: ExecOptions) -> Result<ExecOutput, RuntimeError>;
// Real (Wave B): resolves the session (--resume / --continue / new from --provider/--transport/
// --model + Config defaults), applies --mode, drives one turn headless (`--dangerously-skip-
// permissions` picks AutoAllow vs. AutoDeny for an `Ask` decision — there's no UI to prompt),
// enforces --print-timeout. See exec.rs's module doc for two flagged app-side follow-ups:
// (1) `app::cmd::exec::dispatch` always maps `Err` -> exit 1 / `Ok` -> exit 0 today; distinguishing
// exit 2 (RuntimeError::InvalidRequest) and exit 3 (ExecOutput.status != "ok" with a non-empty
// response) needs an app-side change. (2) real `stream-json` incremental rendering needs app to
// concurrently `handle.subscribe()` while `run_exec` runs, not just render the final ExecOutput.
// (3) --dangerously-skip-permissions does not check workspace trust yet (no TrustStore handle on
// RuntimeDeps in Wave B) — flag before treating it as multi-tenant-safe.

pub enum RuntimeError { UnknownSession(SessionId), NoActiveTurn(SessionId), UnknownCommand(String),
    NoPendingPermission(ToolCallId), AlreadySubscribed, Storage(#[from] StorageError),
    Tool(#[from] ToolError), Provider(#[from] ProviderError),
    Auth(#[from] xlightcli_auth::AuthError),          // [Wave B addition]
    InvalidRequest(String),                            // [Wave B addition]
    NotImplemented(&'static str) }

// testing.rs — feature "testing" (pulls in xlightcli-provider/testing, xlightcli-auth/testing)
pub fn mock_deps(storage: xlightcli_storage::Storage) -> RuntimeDeps; // empty ProviderRegistry, AuthBroker::new(), with_builtins() tools
pub fn mock_handle(deps: RuntimeDeps) -> RuntimeHandle;

// Re-exported at crate root so `tui` (which cannot depend on `xlightcli-tools` directly,
// CODEBASE.md §3) can still name types `UiEvent`/`RuntimeHandle` expose:
pub use xlightcli_tools::{ArtifactRef, ExecutionMode, PermissionMode, PermissionRequest};
```

`Session` (`session.rs`): `{ id, workspace_id, provider, transport, model, title, mode }`, built via
`Session::from_record(xlightcli_storage::SessionRecord)`.

---

## 11. `xlightcli-tui` (Phase 1 Wave B)

Every item below is real (Wave A's "no rendering yet"/stub notes no longer apply). Signature
changes vs. the Wave A shape are additive except where noted; `App`/`Action`/`input::action_for`
gained fields/parameters that only `tui`'s own `run` (its sole caller) uses.

```rust
pub struct TuiOptions { pub initial_session: Option<SessionId> } // impl Default
pub async fn run(handle: xlightcli_runtime::RuntimeHandle, opts: TuiOptions) -> Result<(), TuiError>;
// Real terminal lifecycle: installs a panic hook that restores the terminal before the default
// panic message prints, raw mode + alternate screen, tokio::select! over UiEvents + crossterm key
// events (Event::Resize marks the frame dirty). Renders on a ~20 FPS ticking interval whenever
// App::dirty is set (never once per delta, PATTERNS.md §3) via Terminal::draw(|f| app.draw(f)).
// Restores the terminal on the way out regardless of how the loop exited.

pub enum TuiError { Io(#[from] std::io::Error), Runtime(#[from] xlightcli_runtime::RuntimeError),
                     NotImplemented(&'static str) }

// app.rs
pub struct App { pub handle: RuntimeHandle, pub keymap: Keymap, pub theme: Theme,
                  pub transcript: view::TranscriptView, pub prompt: view::PromptView,
                  pub status_line: Option<view::StatusLineView>, pub overlay: Overlay,
                  pub execution_mode: xlightcli_runtime::ExecutionMode, pub should_quit: bool,
                  // [Wave B additions]
                  pub session_id: Option<SessionId>,   // set from UiEvent::SessionChanged/TurnStarted
                  pub quit_confirm_pending: bool,       // Ctrl+C-twice-to-quit state
                  pub dirty: bool }                     // drives crate::run's ticking redraw
impl App {
    pub fn new(handle: RuntimeHandle) -> Self;          // status_line starts Some(default), not None
    pub fn apply_ui_event(&mut self, event: xlightcli_runtime::UiEvent); // every arm now real
    // [Wave B additions]
    pub fn on_key(&mut self, key: crossterm::event::KeyEvent) -> Intent;
    // Pure state transition (no .await anywhere in its call tree): mutates transcript/prompt/
    // overlay/execution_mode and returns the one RuntimeHandle call (if any) crate::run's event
    // loop should make. This is what makes key handling unit-testable without a TTY or Tokio.
    pub fn open_diff(&mut self, view: view::DiffView); // used by `/diff`'s CommandOutcome handling
    pub fn draw(&self, frame: &mut ratatui::Frame<'_>); // status line + transcript + prompt + overlay popup
}
pub enum Overlay { None, CommandPalette(view::CommandPaletteView), Permission(view::PermissionDialogView), Diff(view::DiffView) }

// [Wave B addition] what `App::on_key` asks `crate::run`'s event loop to do with the RuntimeHandle
// (kept out of App so on_key stays synchronous/pure):
pub enum Intent { None, Quit, Submit(String), RunCommand(String),
    RespondPermission(ToolCallId, xlightcli_runtime::PermissionResponse), CancelTurn,
    SetExecutionMode(xlightcli_runtime::ExecutionMode) }

// keymap.rs
pub enum Action { Submit, Quit, Cancel /* [Wave B addition]: Esc */, CycleExecutionMode,
                   OpenCommandPalette, OpenAgentTree, ScrollTranscriptUp, ScrollTranscriptDown }
pub struct Keymap; // impl Default (shift+tab -> CycleExecutionMode, ctrl+c -> Quit, esc -> Cancel,
                    // pageup/pagedown -> Scroll* [Wave B: moved off plain Up/Down, now free for
                    // PromptView's history navigation], ...)
impl Keymap {
    pub fn apply_overrides(&mut self, overrides: &BTreeMap<String, String>); // from xlightcli_config::UiConfig::keybind
    pub fn chord_for(&self, action: Action) -> Option<&str>;
    pub fn action_for_chord(&self, chord: &str) -> Option<Action>; // [Wave B addition] reverse lookup
}

// theme.rs
pub enum ThemeRole { Foreground, Background, Accent, Muted, Success, Warning, Danger }
pub struct Theme { pub name: String } // impl Default = "default"
impl Theme { pub fn style(&self, role: ThemeRole) -> ratatui::style::Style; } // [Wave B addition]

// view/ — state + real ratatui rendering (each has a `render(&self, frame, area, theme[, ..])` method)
pub struct TranscriptView { pub lines: Vec<TranscriptLine>, pub scroll_offset: u16 }
pub enum TranscriptRole { User, Assistant, Reasoning, ToolCall, Notice, Error } // [Wave B addition]
pub struct TranscriptLine { pub role: TranscriptRole, pub text: String, pub agent_id: Option<AgentId>,
    pub tool_call_id: Option<ToolCallId>, pub artifact_path: Option<String>, pub finished: bool,
    pub collapsed: bool } // [Wave B: was just `{ text: String }`]
impl TranscriptView {
    // push_user/push_notice/push_notice_leveled/push_error: one-shot lines.
    // push_assistant_delta/push_reasoning_delta: coalesce into the last matching line per agent_id.
    // start_tool_call/finish_tool_call: a tool-call card, filled in when it finishes.
    // toggle_last_reasoning: collapse/expand the most recent reasoning block (Ctrl+R).
    // scroll_up/scroll_down.
}
pub struct PromptView { pub buffer: String, pub cursor: usize,
    pub history: Vec<String> } // [Wave B addition: multiline editing + submission history]
impl PromptView { pub fn height(&self) -> u16; pub fn take(&mut self) -> String;
    pub fn on_key(&mut self, key: &crossterm::event::KeyEvent) -> bool; }
pub struct StatusLineView { pub workspace_label: String, pub provider: ProviderId, pub transport: TransportId,
    pub model: ModelId, pub agents_total: u32, pub agents_running: u32,
    pub context_used_percent: u8, pub permission_mode: xlightcli_runtime::PermissionMode,
    pub execution_mode: xlightcli_runtime::ExecutionMode,       // [Wave B addition]
    pub last_usage: Option<Usage>, pub last_rate_limit: Option<RateLimitInfo> } // [Wave B addition]
pub struct PermissionDialogView { pub tool_call_id: ToolCallId, pub request: xlightcli_runtime::PermissionRequest,
    pub selected: usize } // [Wave B addition: 3-way Allow once / Always allow / Deny selection]
pub enum PermissionChoice { AllowOnce, AlwaysAllow, Deny } // [Wave B addition]
pub struct DiffView { pub hunks: Vec<DiffHunk>, pub selected: usize }
pub fn parse_unified_diff(text: &str) -> Vec<DiffHunk>; // [Wave B addition] splits `git diff`/`--- a/<path>` text per file
pub struct CommandPaletteView { pub query: String, pub selected: usize,
    pub matches: Vec<usize> } // [Wave B addition: indices into the caller's (alias, summary) entries]

// input.rs
pub fn quit_requested(key: &crossterm::event::KeyEvent) -> bool; // hardcoded Ctrl+C, keymap-independent
pub fn chord_of(key: &crossterm::event::KeyEvent) -> String; // [Wave B addition] normalizes a key into "ctrl+c"/"shift+tab"/... form
pub fn action_for(key: &crossterm::event::KeyEvent, keymap: &Keymap) -> Option<Action>; // [Wave B: now real, takes a Keymap]
```

`run`'s event loop still can't be meaningfully unit-tested end to end (needs a real TTY), but
`App::on_key`/`apply_ui_event`/`draw` are — `draw` via `ratatui::backend::TestBackend` + `insta`
snapshot tests (`tests/snapshots.rs`: empty session, streaming answer, tool-call card, permission
dialog, diff view, status line per execution mode).

---

## 12. `xlightcli` (app) — Phase 1 Wave A CLI additions

`wiring::AppContext` (Phase 0 shape: `{ auth: AuthBroker, providers: ProviderRegistry }`)
**unchanged** — every existing `dev`/`auth`/`provider` command and test still uses it as-is.
Runtime-backed commands use a new, separate context instead:

```rust
// wiring.rs
pub enum WiringError { Storage(xlightcli_storage::StorageError), Config(xlightcli_config::ConfigError) }
pub struct RuntimeContext { pub handle: xlightcli_runtime::RuntimeHandle }
pub async fn build_runtime() -> Result<RuntimeContext, WiringError>;
    // Resolves layered global/project config via ConfigLoader + TrustStore; opens Storage at
    // xlightcli_config::paths::database_path(); registers xlightcli_tools::ToolRegistry::with_builtins().
pub async fn build_runtime_at(db_path: PathBuf) -> Result<RuntimeContext, WiringError>;
    // Same, but an explicit db path with isolated config defaults — the seam integration tests use.
pub async fn build_runtime_with_paths(db_path: PathBuf, global_config_path: PathBuf, trust_path: PathBuf, repo_root: Option<&Path>) -> Result<RuntimeContext, WiringError>;

// cli.rs — new Command variants (existing Dev/Auth/Provider unchanged)
pub enum Command {
    Dev { .. }, Auth { .. }, Provider { .. },
    Exec(ExecArgs),
    Init,
}
pub struct ExecArgs { pub print: Option<String>, pub prompt: Option<String>,
    pub output_format: ExecOutputFormatArg, pub model: Option<String>, pub provider: Option<String>,
    pub transport: Option<String>, pub mode: Option<String>, pub continue_session: bool,
    pub resume: Option<String>, pub dangerously_skip_permissions: bool,
    pub add_dir: Vec<PathBuf>, pub print_timeout: Option<u64> }
pub enum ExecOutputFormatArg { Text, Json, StreamJson } // impl From<Self> for xlightcli_runtime::ExecOutputFormat

// cmd::exec — real turn execution via xlightcli_runtime::run_exec; D-026 exit codes 0/1/2/3; incremental stream-json
pub async fn dispatch(args: &ExecArgs) -> Result<(), CliError>;
// cmd::init — minimal real behavior: writes project config skeleton and AGENTS.md stub non-destructively
pub async fn dispatch() -> Result<(), CliError>;
```

Bare `xlightcli` (no subcommand) now builds a `RuntimeContext` and calls `xlightcli_tui::run`
instead of returning "not implemented" (`crates/app/src/lib.rs::execute`'s `None` arm).
