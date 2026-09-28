// SPDX-License-Identifier: GPL-3.0-only

//! Core provider traits (docs/PLAN.md §4.1, §5.2): `Provider`, `TransportAdapter`,
//! `ProviderFeaturePack`, `ConfigImporter`, and the command types they use.
//!
//! `ProviderCommand` / `CommandContext` / `RichText` / `TurnRequestPatch` are intentionally
//! minimal placeholders: the full command system (`CommandRegistry`, session-safe context with
//! read APIs, recipes) is Phase 2 (docs/PLAN.md §19). Their *shape* is fixed here so
//! `ProviderFeaturePack` compiles end to end today; Phase 2 is expected to grow, not break, them.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use tokio_util::sync::CancellationToken;
use xlightcli_auth::{AuthAdapter, CredentialHandle};
use xlightcli_protocol::{
    AgentEvent, AuthKind, CapabilityMode, CommandId, ConfigFragment, ModelInfo, ProtocolVersion,
    ProviderCapabilities, ProviderError, ProviderId, QuotaSnapshot, ReasoningEffort, SessionId,
    Stability, TransportId, TurnRequest,
};

/// Pull-based event stream a transport yields for one turn (D-007). Backpressure is natural:
/// nothing is produced faster than the consumer polls.
pub type EventStream = BoxStream<'static, Result<AgentEvent, ProviderError>>;

/// One transport (auth method + wire protocol) for a provider, e.g. `"chatgpt"` or
/// `"anthropic-api"` (docs/PLAN.md §4.1/§4.2).
#[async_trait]
pub trait TransportAdapter: Send + Sync {
    fn id(&self) -> TransportId;
    fn stability(&self) -> Stability;
    fn required_auth(&self) -> AuthKind;
    fn capabilities(&self) -> &ProviderCapabilities;
    /// Wire protocol/schema version this adapter is pinned to (version gating, docs/PLAN.md §15).
    fn protocol_version(&self) -> ProtocolVersion;

    async fn list_models(&self, cred: &CredentialHandle) -> Result<Vec<ModelInfo>, ProviderError>;

    /// Plan/quota snapshot for `/usage` (D-027). `Ok(None)` when upstream exposes nothing.
    async fn quota(&self, cred: &CredentialHandle) -> Result<Option<QuotaSnapshot>, ProviderError>;

    /// Maps the canonical `ReasoningEffort` to wire fields / model id variants (e.g. agy encodes
    /// the tier in the model id rather than a separate field).
    fn apply_effort(&self, req: &mut TurnRequest, effort: ReasoningEffort);

    async fn stream(
        &self,
        req: TurnRequest,
        cred: CredentialHandle,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError>;
}

/// One command a `ProviderFeaturePack` contributes (PATTERNS.md §12).
#[derive(Debug, Clone)]
pub struct CommandDefinition {
    pub id: CommandId,
    pub alias: &'static str,
    pub mode: CapabilityMode,
    /// `None` means "available on every transport of this provider".
    pub requires_transport: Option<TransportId>,
    pub summary: &'static str,
}

/// Parsed invocation of a `ProviderFeaturePack` command. Full argument parsing lands with
/// `runtime::CommandRegistry` in Phase 2; for now the raw text is passed through.
#[derive(Debug, Clone)]
pub struct ProviderCommand {
    pub id: CommandId,
    pub raw_args: String,
}

/// Safe, non-secret context handed to `ProviderFeaturePack::execute` (docs/PLAN.md §5.2:
/// "**no** credential"). Grows in Phase 2 (session history query, workspace info, a way to
/// start a runtime turn).
#[derive(Debug, Clone)]
pub struct CommandContext {
    pub session: SessionId,
}

/// Plain rendered text shown to the user (no markdown/ANSI processing decided here — that's a TUI
/// rendering concern).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RichText(pub String);

/// A partial `TurnRequest` a command can use to kick off a model turn (e.g. `/review`). Full
/// merge semantics with the in-flight `TurnRequest` land with `runtime::recipes` (D-024).
#[derive(Debug, Clone, Default)]
pub struct TurnRequestPatch {
    pub extra_system_text: Option<String>,
    pub extra_user_text: Option<String>,
}

/// Outcome of executing a `ProviderCommand`. `Unavailable` must be used instead of faking
/// behavior when a feature isn't supported (INV-10).
#[derive(Debug, Clone)]
pub enum CommandResult {
    Message(RichText),
    StartTurn(TurnRequestPatch),
    Unavailable { reason: String },
}

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("command failed: {0}")]
    Failed(String),
}

/// Provider-specific slash commands (docs/PLAN.md §5.2). Implemented in `provider-<x>::features`.
#[async_trait]
pub trait ProviderFeaturePack: Send + Sync {
    fn commands(&self) -> Vec<CommandDefinition>;
    async fn execute(
        &self,
        cmd: ProviderCommand,
        ctx: CommandContext,
    ) -> Result<CommandResult, CommandError>;
}

/// Reads another CLI's config/credentials and returns a canonical, provider-agnostic fragment
/// (D-027). Read-only (D-017): never writes back to the source, never spawns the source CLI
/// (INV-1).
#[async_trait]
pub trait ConfigImporter: Send + Sync {
    /// Human-readable name of the CLI this importer reads from (e.g. `"Codex CLI"`).
    fn source_name(&self) -> &'static str;
    async fn import(&self) -> Result<ConfigFragment, ProviderError>;
}

/// `Provider = Auth + Transports + FeaturePack` (docs/PLAN.md §4.1).
pub trait Provider: Send + Sync {
    fn id(&self) -> ProviderId;
    fn display_name(&self) -> &str;
    fn auth(&self) -> &dyn AuthAdapter;
    fn transports(&self) -> &[Arc<dyn TransportAdapter>];
    fn features(&self) -> &dyn ProviderFeaturePack;
    fn importer(&self) -> Option<&dyn ConfigImporter> {
        None
    }

    fn transport(&self, id: &TransportId) -> Option<&Arc<dyn TransportAdapter>> {
        self.transports().iter().find(|t| &t.id() == id)
    }
}
