// SPDX-License-Identifier: GPL-3.0-only

#![cfg(test)]

//! Shared test harness for `runtime`'s own Wave B integration tests (`agent`/`handle`/`exec`
//! test modules): a `RuntimeHandle` wired to a scripted, call-count-aware test transport plus a
//! real (tempdir-backed) `AuthBroker` that can actually log in.
//!
//! **Why not `xlightcli_provider::testing::MockProvider` alone:** its `MockTransport` replays the
//! exact same script on every `stream()` call (it doesn't know how many times it's been called),
//! which can't represent "tool call on the first round trip, `EndTurn` on the second" — needed for
//! a multi-step tool loop test. Its bundled auth adapter's `login()` also always returns
//! `AuthError::NotImplemented` (it exists only to make `MockProvider` constructible, not to be
//! logged into via a real `AuthBroker`). [`TestTransport`] is a small, direct
//! `xlightcli_provider::TransportAdapter` implementation instead — no new dependency, still no
//! network, still deterministic.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use xlightcli_auth::{
    AccountInfo, AuthAdapter, AuthBroker, AuthMethod, CredentialHandle, CredentialSecret,
    CredentialSet, DiscoveredCredential, LoginUi,
};
use xlightcli_protocol::Message;
use xlightcli_protocol::{
    AgentEvent, AuthKind, CapabilityMode, ContentBlock, ModelId, ModelInfo, ProtocolVersion,
    ProviderCapabilities, ProviderError, ProviderId, QuotaSnapshot, ReasoningEffort, Role,
    SessionId, Stability, StopReason, ToolCallId, TransportId, TurnRequest, Usage,
};
use xlightcli_provider::{
    CommandContext, CommandDefinition, CommandError, CommandResult, EventStream, Provider,
    ProviderCommand, ProviderFeaturePack, ProviderRegistry, TransportAdapter,
};
use xlightcli_tools::{PermissionAction, Tool, ToolContext, ToolEffect, ToolError, ToolOutput};

use crate::handle::{RuntimeConfig, RuntimeDeps, RuntimeHandle};

pub const TEST_PROVIDER: &str = "test-provider";
pub const TEST_TRANSPORT: &str = "test-transport";
pub const TEST_MODEL: &str = "test-model";
pub const TEST_ACCOUNT_ID: &str = "test-account";

#[derive(Debug, Clone, Copy)]
struct TestAuthAdapter;

#[async_trait]
impl AuthAdapter for TestAuthAdapter {
    fn methods(&self) -> &[AuthMethod] {
        &[AuthMethod::ApiKey]
    }

    async fn discover_existing(&self) -> Vec<DiscoveredCredential> {
        Vec::new()
    }

    async fn import(
        &self,
        _found: &DiscoveredCredential,
    ) -> Result<CredentialSet, xlightcli_auth::AuthError> {
        Err(xlightcli_auth::AuthError::NotImplemented(
            "TestAuthAdapter has no import flow",
        ))
    }

    async fn login(
        &self,
        _method: AuthMethod,
        _ui: &dyn LoginUi,
    ) -> Result<CredentialSet, xlightcli_auth::AuthError> {
        Ok(CredentialSet {
            account: AccountInfo {
                provider: ProviderId::new(TEST_PROVIDER),
                transport: TransportId::new(TEST_TRANSPORT),
                account_id: TEST_ACCOUNT_ID.to_string(),
                label: None,
                auth_kind: AuthKind::ApiKey,
                metadata: serde_json::json!({}),
            },
            secret: CredentialSecret::Bearer {
                access_token: secrecy::SecretString::from("XLC-SENTINEL-TEST-TOKEN".to_string()),
                refresh_token: None,
                expires_at: None,
            },
        })
    }

    async fn refresh(
        &self,
        current: &CredentialSet,
    ) -> Result<CredentialSet, xlightcli_auth::AuthError> {
        Ok(current.clone())
    }

    async fn revoke(&self, _current: &CredentialSet) -> Result<(), xlightcli_auth::AuthError> {
        Ok(())
    }
}

#[derive(Debug)]
struct NoopLoginUi;

#[async_trait]
impl LoginUi for NoopLoginUi {
    async fn show_browser_url(&self, _url: &str) {}
    async fn show_device_code(&self, _verification_uri: &str, _user_code: &str) {}
    async fn prompt_api_key(
        &self,
        _provider: &ProviderId,
    ) -> Result<secrecy::SecretString, xlightcli_auth::AuthError> {
        Ok(secrecy::SecretString::from("unused".to_string()))
    }
}

#[derive(Debug)]
struct NoopFeatures;

#[async_trait]
impl ProviderFeaturePack for NoopFeatures {
    fn commands(&self) -> Vec<CommandDefinition> {
        Vec::new()
    }

    async fn execute(
        &self,
        _cmd: ProviderCommand,
        _ctx: CommandContext,
    ) -> Result<CommandResult, CommandError> {
        Ok(CommandResult::Unavailable {
            reason: "test stub".to_string(),
        })
    }
}

fn default_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        reasoning: false,
        images: false,
        tool_calls: true,
        parallel_tool_calls: false,
        web_search: CapabilityMode::Unsupported,
        mcp: CapabilityMode::Core,
        session_resume: CapabilityMode::Core,
        usage: CapabilityMode::Native,
        quota: CapabilityMode::Unsupported,
        context_window: None,
    }
}

/// Call-count-aware scripted transport (see the module doc for why this exists instead of
/// `xlightcli_provider::testing::MockTransport`): call *n* (0-indexed) replays
/// `scripts[min(n, scripts.len() - 1)]`, so the last script repeats if `stream()` is called more
/// times than there are scripts (useful for a `max_steps`-exceeded scenario).
#[derive(Debug)]
pub struct TestTransport {
    capabilities: ProviderCapabilities,
    scripts: Vec<Vec<Result<AgentEvent, ProviderError>>>,
    /// Delay before yielding each event — lets a test cancel mid-stream deterministically instead
    /// of racing a same-tick stream.
    delay: Duration,
    call_count: AtomicUsize,
}

impl TestTransport {
    pub fn new(scripts: Vec<Vec<Result<AgentEvent, ProviderError>>>) -> Self {
        Self {
            capabilities: default_capabilities(),
            scripts,
            delay: Duration::ZERO,
            call_count: AtomicUsize::new(0),
        }
    }

    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    pub fn with_context_window(mut self, window: u32) -> Self {
        self.capabilities.context_window = Some(window);
        self
    }
}

#[async_trait]
impl TransportAdapter for TestTransport {
    fn id(&self) -> TransportId {
        TransportId::new(TEST_TRANSPORT)
    }

    fn stability(&self) -> Stability {
        Stability::Stable
    }

    fn required_auth(&self) -> AuthKind {
        AuthKind::ApiKey
    }

    fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    fn protocol_version(&self) -> ProtocolVersion {
        ProtocolVersion(1)
    }

    async fn list_models(&self, _cred: &CredentialHandle) -> Result<Vec<ModelInfo>, ProviderError> {
        Ok(Vec::new())
    }

    async fn quota(
        &self,
        _cred: &CredentialHandle,
    ) -> Result<Option<QuotaSnapshot>, ProviderError> {
        Ok(None)
    }

    fn apply_effort(&self, _req: &mut TurnRequest, _effort: ReasoningEffort) {}

    async fn stream(
        &self,
        _req: TurnRequest,
        _cred: CredentialHandle,
        _cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        let call_index = self.call_count.fetch_add(1, Ordering::SeqCst);
        let script = self
            .scripts
            .get(call_index)
            .or_else(|| self.scripts.last())
            .cloned()
            .unwrap_or_default();
        let delay = self.delay;
        Ok(Box::pin(async_stream::stream! {
            for item in script {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                yield item;
            }
        }))
    }
}

/// Wraps a [`TestTransport`] with a working test-only `AuthAdapter` (see the module doc for why
/// `xlightcli_provider::testing::MockProvider`'s own adapter can't be used here).
struct TestProvider {
    auth: TestAuthAdapter,
    features: NoopFeatures,
    transports: Vec<Arc<dyn TransportAdapter>>,
}

impl Provider for TestProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new(TEST_PROVIDER)
    }

    fn display_name(&self) -> &str {
        "Test Provider"
    }

    fn auth(&self) -> &dyn AuthAdapter {
        &self.auth
    }

    fn transports(&self) -> &[Arc<dyn TransportAdapter>] {
        &self.transports
    }

    fn features(&self) -> &dyn ProviderFeaturePack {
        &self.features
    }
}

/// Everything one test needs: a real `RuntimeHandle` plus the tempdir backing its storage/secret
/// store (kept alive for the harness's lifetime — dropping it deletes the directory).
pub struct TestHarness {
    pub handle: RuntimeHandle,
    _dir: tempfile::TempDir,
}

/// Builds a [`TestTransport`] from `scripts` (one script per call to `stream()`, see
/// [`TestTransport`]'s doc), with optional `delay`/`context_window`, wires it into a fresh
/// `RuntimeHandle` (real tempdir-backed `Storage` + `AuthBroker`, already logged in), and
/// registers `tools` as the active `ToolRegistry`. `config` is `RuntimeDeps::config` — override
/// `budget.default.max_turns`/`context.compaction_threshold`/`permissions` per test as needed.
pub async fn build_harness(
    scripts: Vec<Vec<Result<AgentEvent, ProviderError>>>,
    delay: Duration,
    context_window: Option<u32>,
    tools: xlightcli_tools::ToolRegistry,
    config: xlightcli_config::Config,
) -> TestHarness {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("test.db");
    let storage = xlightcli_storage::Storage::open(&db_path)
        .await
        .expect("open storage");

    let mut transport = TestTransport::new(scripts).with_delay(delay);
    if let Some(window) = context_window {
        transport = transport.with_context_window(window);
    }
    let transport: Arc<dyn TransportAdapter> = Arc::new(transport);
    let provider: Arc<dyn Provider> = Arc::new(TestProvider {
        auth: TestAuthAdapter,
        features: NoopFeatures,
        transports: vec![transport],
    });

    let mut registry = ProviderRegistry::new();
    registry.register(Arc::clone(&provider));

    let store: Arc<dyn xlightcli_auth::SecretStore> = Arc::new(xlightcli_auth::FileStore::new(
        dir.path().join("credentials"),
    ));
    let index = xlightcli_storage::AccountIndex::open(dir.path().join("accounts.db"))
        .await
        .expect("open account index");
    let mut broker = AuthBroker::with_store_and_index(store, index);
    broker.register_adapter(ProviderId::new(TEST_PROVIDER), Arc::new(TestAuthAdapter));
    broker
        .login(
            &ProviderId::new(TEST_PROVIDER),
            AuthMethod::ApiKey,
            &NoopLoginUi,
        )
        .await
        .expect("test login should always succeed");

    let deps = RuntimeDeps {
        providers: Arc::new(registry),
        auth: Arc::new(broker),
        tools: Arc::new(tools),
        storage,
        config,
    };
    let handle = RuntimeHandle::new(deps, RuntimeConfig::default());
    TestHarness { handle, _dir: dir }
}

/// A test-only tool that always checks `PermissionAction::WriteFile("out.txt")` before "writing"
/// (it never touches the real filesystem) — used to exercise the permission gate (ask/allow/deny,
/// plan-mode block, accept-edits auto-allow) without depending on `xlightcli-tools`'s real
/// built-ins (owned by a different Wave B agent and out of scope to depend on for this).
#[derive(Debug)]
pub struct EchoTool {
    def: xlightcli_protocol::ToolDefinition,
}

impl EchoTool {
    pub fn new() -> Self {
        Self {
            def: xlightcli_protocol::ToolDefinition {
                name: "echo_tool".to_string(),
                description: "Test-only tool: checks a WriteFile permission, then echoes its \
                              `text` input back."
                    .to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"],
                }),
            },
        }
    }
}

impl Default for EchoTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for EchoTool {
    fn definition(&self) -> &xlightcli_protocol::ToolDefinition {
        &self.def
    }

    fn effect(&self, _input: &serde_json::Value) -> ToolEffect {
        ToolEffect::WritesWorkspace
    }

    async fn run(
        &self,
        input: serde_json::Value,
        ctx: ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        ctx.check_permission(PermissionAction::WriteFile(std::path::Path::new("out.txt")))
            .await?;
        let text = input
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        Ok(ToolOutput::Text(format!("echoed: {text}")))
    }
}

pub fn tool_registry_with_echo() -> xlightcli_tools::ToolRegistry {
    let mut registry = xlightcli_tools::ToolRegistry::new();
    registry.register(Arc::new(EchoTool::new()));
    registry
}

/// Builds a scripted `Completed` event — the common case for every script item in these tests.
pub fn completed(message: Message, stop: StopReason) -> Result<AgentEvent, ProviderError> {
    Ok(AgentEvent::Completed {
        message,
        stop,
        usage: Usage::default(),
    })
}

/// A plain assistant text message (no tool calls).
pub fn text_message(text: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
    }
}

/// An assistant message that calls `tool_name` with `input`, optionally preceded by `lead_text`
/// (simulates the model narrating before it calls a tool — used by the "partial output already
/// produced" test).
pub fn tool_use_message(
    lead_text: Option<&str>,
    call_id: &str,
    tool_name: &str,
    input: serde_json::Value,
) -> Message {
    let mut content = Vec::new();
    if let Some(text) = lead_text {
        content.push(ContentBlock::Text {
            text: text.to_string(),
        });
    }
    content.push(ContentBlock::ToolUse {
        id: ToolCallId::new(call_id),
        name: tool_name.to_string(),
        input,
        opaque: None,
    });
    Message {
        role: Role::Assistant,
        content,
    }
}

/// Creates a workspace + session (provider/transport/model = the harness's [`TestTransport`]) and
/// returns its id — the common first step of nearly every test built on [`TestHarness`].
pub async fn new_session(harness: &TestHarness) -> SessionId {
    let workspace = harness
        .handle
        .deps()
        .storage
        .create_workspace(std::env::temp_dir(), None)
        .await
        .expect("create workspace");
    harness
        .handle
        .create_session(
            workspace,
            ProviderId::new(TEST_PROVIDER),
            TransportId::new(TEST_TRANSPORT),
            ModelId::new(TEST_MODEL),
            None,
        )
        .await
        .expect("create session")
}
