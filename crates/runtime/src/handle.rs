// SPDX-License-Identifier: GPL-3.0-only

//! `RuntimeHandle` — the **only** API `tui`/`app` use (CODEBASE.md §3): submit user input, run a
//! slash command, respond to a permission request, cancel a turn, switch execution mode,
//! list/resume sessions, subscribe to `UiEvent`s.
//!
//! **Status (Phase 1 Wave A):** the channel plumbing (bounded `UiEvent` stream, session
//! bookkeeping, command resolution) is real; the parts that actually drive a provider turn
//! (`submit_user_input`, `run_command`'s command bodies) delegate to `crate::agent::AgentLoop`
//! and `crate::context::ContextManager`, which are Wave B stubs — see those modules' docs.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;
use xlightcli_protocol::{
    AgentId, ModelId, ModelInfo, ProviderId, SessionId, Stability, StopReason, ToolCallId,
    TransportId, Usage,
};

use crate::agent::{AgentLoop, TurnContext};
use crate::commands::CommandRegistry;
use crate::context::ContextManager;
use crate::error::RuntimeError;
use crate::permission_gate::PendingPermissions;
use crate::session::Session;

#[derive(Debug)]
struct SessionLoginUi {
    ui_tx: mpsc::Sender<UiEvent>,
}

#[async_trait]
impl xlightcli_auth::LoginUi for SessionLoginUi {
    async fn show_browser_url(&self, url: &str) {
        let _ = self
            .ui_tx
            .send(UiEvent::Notice {
                level: NoticeLevel::Info,
                message: format!("Open this URL to log in: {url}"),
            })
            .await;
    }

    async fn show_device_code(&self, verification_uri: &str, user_code: &str) {
        let _ = self
            .ui_tx
            .send(UiEvent::Notice {
                level: NoticeLevel::Info,
                message: format!("Go to {verification_uri} and enter code {user_code}"),
            })
            .await;
    }

    async fn prompt_api_key(
        &self,
        provider: &ProviderId,
    ) -> Result<secrecy::SecretString, xlightcli_auth::AuthError> {
        let name = format!(
            "XLIGHTCLI_API_KEY_{}",
            provider.as_str().to_uppercase().replace(['-', '.'], "_")
        );
        let value = std::env::var(&name).map_err(|_| xlightcli_auth::AuthError::OAuth(format!(
            "set {name} before using /login for an API-key transport, or run `xlightcli auth login {provider}` outside the TUI"
        )))?;
        if value.is_empty() {
            return Err(xlightcli_auth::AuthError::OAuth(format!("{name} is empty")));
        }
        Ok(secrecy::SecretString::from(value))
    }
}

/// Process-wide shared resources a `RuntimeHandle` operates over (CODEBASE.md §5), `Arc`'d by
/// `app::wiring` and never cloned per agent.
///
/// **Scope decision (documented, Phase 1 Wave A):** no `mcp` field yet — `xlightcli-mcp` is still
/// an empty skeleton (Phase 3, CODEBASE.md §2) with no public type to hold a handle to. Add it
/// here (additive, non-breaking) once `McpManager` exists.
#[derive(Clone)]
pub struct RuntimeDeps {
    pub providers: Arc<xlightcli_provider::ProviderRegistry>,
    pub auth: Arc<xlightcli_auth::AuthBroker>,
    pub tools: Arc<xlightcli_tools::ToolRegistry>,
    pub storage: xlightcli_storage::Storage,
    pub config: xlightcli_config::Config,
    pub workspace_trust: Arc<xlightcli_config::TrustStore>,
    /// Process-wide opt-in for `--dangerously-skip-permissions`; project config cannot set it.
    pub allow_dangerous_permissions: bool,
}

impl std::fmt::Debug for RuntimeDeps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeDeps").finish_non_exhaustive()
    }
}

/// Tunables for the runtime itself (as opposed to `xlightcli_config::Config`, which is
/// user-facing persisted config).
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// `UiEvent` channel capacity (PATTERNS.md §3 example: 256, "~1 frame of backlog per agent").
    pub ui_channel_capacity: usize,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            ui_channel_capacity: 256,
        }
    }
}

/// A tool-call summary attached to `UiEvent::ToolCallFinished` (PATTERNS.md §9): the model-facing
/// text plus, when the output was spooled, the artifact it was spooled to. Never the full,
/// unbounded output.
#[derive(Debug, Clone)]
pub struct ToolCallSummary {
    pub name: String,
    pub text_preview: String,
    pub artifact: Option<xlightcli_tools::ArtifactRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Warn,
    Error,
}

/// Events pushed from the runtime to the frontend (`tui` or `exec`). Coalescible variants
/// (`TextDelta`/`ReasoningDelta`) may be merged by a slow receiver; every other variant must never
/// be dropped (PATTERNS.md §3).
#[derive(Debug, Clone)]
pub enum UiEvent {
    TurnStarted {
        session_id: SessionId,
        agent_id: AgentId,
        model: ModelId,
    },
    TextDelta {
        agent_id: AgentId,
        index: u32,
        text: String,
    },
    ReasoningDelta {
        agent_id: AgentId,
        index: u32,
        text: String,
    },
    ToolCallStarted {
        agent_id: AgentId,
        call_id: ToolCallId,
        name: String,
    },
    ToolCallFinished {
        agent_id: AgentId,
        call_id: ToolCallId,
        summary: ToolCallSummary,
    },
    PermissionRequested {
        agent_id: AgentId,
        request: xlightcli_tools::PermissionRequest,
    },
    Usage {
        agent_id: AgentId,
        usage: Usage,
    },
    RateLimit {
        agent_id: AgentId,
        info: xlightcli_protocol::RateLimitInfo,
    },
    TurnCompleted {
        agent_id: AgentId,
        stop: StopReason,
    },
    TurnFailed {
        agent_id: AgentId,
        error: String,
    },
    SessionChanged {
        session_id: SessionId,
    },
    ModeChanged {
        session_id: SessionId,
        mode: xlightcli_tools::ExecutionMode,
    },
    Notice {
        level: NoticeLevel,
        message: String,
    },
}

/// The user's answer to a `UiEvent::PermissionRequested` (docs/PLAN.md §7.3). Not the same enum
/// as `xlightcli_tools::PermissionDecision`: a user only ever says yes or no to *this one*
/// request — "always allow this pattern" is a config change, made through `/permissions`, not
/// through this response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionResponse {
    Allow,
    AllowAlways,
    Deny,
}

/// The result of `RuntimeHandle::run_command` (docs/PLAN.md §5.2, PATTERNS.md §12). Mirrors
/// `xlightcli_provider::CommandResult`'s shape so a FeaturePack-owned Compatible recipe and a
/// core command look the same to the caller.
#[derive(Debug, Clone)]
pub enum CommandOutcome {
    Message(String),
    /// The command started a new turn; the caller should expect `UiEvent`s to follow.
    TurnStarted,
    Unavailable {
        reason: String,
    },
}

struct RuntimeState {
    deps: RuntimeDeps,
    commands: CommandRegistry,
    ui_tx: mpsc::Sender<UiEvent>,
    ui_rx: Mutex<Option<mpsc::Receiver<UiEvent>>>,
    sessions: Mutex<HashMap<SessionId, Session>>,
    /// One root `AgentId` per session, lazily registered (or recovered via
    /// `Storage::list_agents` on resume, docs/PLAN.md §11.2 "resume after a crash") the first time
    /// a turn runs for that session.
    agents: Mutex<HashMap<SessionId, AgentId>>,
    /// The `CancellationToken` for a session's in-flight turn, if any — `cancel_turn` cancels it;
    /// `AgentLoop::run_turn` removes the entry once the turn ends (success, failure, or
    /// cancellation).
    active_turns: Mutex<HashMap<SessionId, CancellationToken>>,
    /// Shared with every `crate::permission_gate::RuntimePermissionGate` built for this handle's
    /// turns; `respond_to_permission` resolves an entry here.
    pending_permissions: PendingPermissions,
    context: ContextManager,
}

/// The only handle `tui`/`app` (and `exec`) hold. Cheap to clone (`Arc`'d internally).
#[derive(Clone)]
pub struct RuntimeHandle {
    state: Arc<RuntimeState>,
}

impl std::fmt::Debug for RuntimeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeHandle").finish_non_exhaustive()
    }
}

impl RuntimeHandle {
    pub fn new(deps: RuntimeDeps, config: RuntimeConfig) -> Self {
        let (ui_tx, ui_rx) = mpsc::channel(config.ui_channel_capacity);
        let context = ContextManager::from_full_config(&deps.config);
        Self {
            state: Arc::new(RuntimeState {
                deps,
                commands: CommandRegistry::with_core_commands(),
                ui_tx,
                ui_rx: Mutex::new(Some(ui_rx)),
                sessions: Mutex::new(HashMap::new()),
                agents: Mutex::new(HashMap::new()),
                active_turns: Mutex::new(HashMap::new()),
                pending_permissions: Arc::new(Mutex::new(HashMap::new())),
                context,
            }),
        }
    }

    pub fn deps(&self) -> &RuntimeDeps {
        &self.state.deps
    }

    pub fn commands(&self) -> &CommandRegistry {
        &self.state.commands
    }

    /// Provider choices for a new interactive session. Only canonical ids and display labels
    /// cross the frontend boundary; concrete provider types remain in `app`.
    pub fn session_providers(&self) -> Vec<(ProviderId, String)> {
        self.state
            .deps
            .providers
            .iter()
            .map(|provider| (provider.id(), provider.display_name().to_string()))
            .collect()
    }

    /// Transport choices for one provider, including experimental transports so their opt-in
    /// requirement can be shown rather than silently hiding an installed adapter.
    pub fn session_transports(
        &self,
        provider_id: &ProviderId,
    ) -> Result<Vec<(TransportId, Stability)>, RuntimeError> {
        let provider = self.state.deps.providers.get(provider_id).ok_or_else(|| {
            RuntimeError::InvalidRequest(format!("unknown provider {provider_id}"))
        })?;
        Ok(provider
            .transports()
            .iter()
            .map(|transport| (transport.id(), transport.stability()))
            .collect())
    }

    /// Model suggestions for a transport. An empty catalog is valid: the TUI still accepts an
    /// explicitly typed model ID when discovery or credentials are unavailable.
    pub async fn session_models(
        &self,
        provider_id: &ProviderId,
        transport_id: &TransportId,
    ) -> Result<Vec<ModelInfo>, RuntimeError> {
        let provider = self.state.deps.providers.get(provider_id).ok_or_else(|| {
            RuntimeError::InvalidRequest(format!("unknown provider {provider_id}"))
        })?;
        let transport = provider.transport(transport_id).ok_or_else(|| {
            RuntimeError::InvalidRequest(format!(
                "provider {provider_id} has no transport {transport_id}"
            ))
        })?;
        let default_model = self
            .state
            .deps
            .config
            .provider
            .get(provider_id.as_str())
            .and_then(|defaults| defaults.default_model.clone());
        let mut models = match self
            .state
            .deps
            .auth
            .credential(provider_id, transport_id)
            .await
        {
            Ok(credential) => match transport.list_models(&credential).await {
                Ok(models) => models,
                Err(err) => {
                    tracing::warn!(%err, "model catalog unavailable; manual model entry remains available");
                    Vec::new()
                }
            },
            Err(err) => {
                tracing::warn!(%err, "credential unavailable; manual model entry remains available");
                Vec::new()
            }
        };
        if let Some(id) = default_model
            && !models.iter().any(|model| model.id == id)
        {
            models.insert(
                0,
                ModelInfo {
                    display_name: format!("{} (configured)", id),
                    id,
                    context_window: None,
                    max_output_tokens: None,
                    supports_reasoning: false,
                },
            );
        }
        Ok(models)
    }

    /// Creates a session rooted at the frontend's current workspace after the picker resolves a
    /// provider, transport, and model. `Storage` remains the owner of workspace/session records.
    pub async fn start_session(
        &self,
        provider_id: ProviderId,
        transport_id: TransportId,
        model_id: ModelId,
    ) -> Result<SessionId, RuntimeError> {
        self.session_transports(&provider_id)?
            .into_iter()
            .find(|(id, _)| *id == transport_id)
            .ok_or_else(|| {
                RuntimeError::InvalidRequest(format!(
                    "provider {provider_id} has no transport {transport_id}"
                ))
            })?;
        if model_id.as_str().is_empty() || model_id.as_str().chars().any(char::is_whitespace) {
            return Err(RuntimeError::InvalidRequest(
                "model id must be non-empty and contain no whitespace".to_string(),
            ));
        }
        let root = std::env::current_dir().map_err(|err| {
            RuntimeError::InvalidRequest(format!("cannot locate current workspace: {err}"))
        })?;
        let workspace_id = self.state.deps.storage.create_workspace(root, None).await?;
        self.create_session(workspace_id, provider_id, transport_id, model_id, None)
            .await
    }

    /// Takes ownership of the single `UiEvent` receiver. Errors on a second call — there is
    /// exactly one frontend (`tui` or `exec`) per process (CODEBASE.md §5); a second subscriber
    /// would silently only see events after it subscribed, which is more likely a bug than intent.
    pub async fn subscribe(&self) -> Result<mpsc::Receiver<UiEvent>, RuntimeError> {
        self.state
            .ui_rx
            .lock()
            .await
            .take()
            .ok_or(RuntimeError::AlreadySubscribed)
    }

    /// Emits a `UiEvent`. Only `crate::agent`/internal callers need this once the real agent loop
    /// exists; exposed `pub(crate)` for that reason, not part of the frontend-facing API.
    pub(crate) async fn emit(&self, event: UiEvent) {
        // A full channel means the frontend is falling behind; dropping a non-coalescible event
        // would violate PATTERNS.md §3, so this blocks briefly rather than using `try_send`. Wave
        // B's real agent loop should still coalesce `TextDelta`/`ReasoningDelta` before calling
        // this, per the module doc.
        let _ = self.state.ui_tx.send(event).await;
    }

    /// `pub(crate)` (not just private): `crate::exec::run_exec` needs to fetch the same `Session`
    /// snapshot `submit_user_input` uses, without duplicating `RuntimeHandle`'s session-tracking
    /// state in `exec.rs`.
    pub(crate) async fn get_session(&self, session_id: SessionId) -> Result<Session, RuntimeError> {
        self.state
            .sessions
            .lock()
            .await
            .get(&session_id)
            .cloned()
            .ok_or(RuntimeError::UnknownSession(session_id))
    }

    /// Returns the session's root `AgentId`, registering one via `Storage::register_agent` the
    /// first time this is called for a session — or, if the process restarted (crash recovery,
    /// docs/PLAN.md §11.2), recovering the one a previous run already registered via
    /// `Storage::list_agents` instead of creating a duplicate.
    pub(crate) async fn ensure_agent(&self, session: &Session) -> Result<AgentId, RuntimeError> {
        if let Some(id) = self.state.agents.lock().await.get(&session.id).copied() {
            return Ok(id);
        }
        let existing = self.state.deps.storage.list_agents(session.id).await?;
        let agent_id = match existing.into_iter().find(|a| a.parent_id.is_none()) {
            Some(record) => record.id,
            None => {
                self.state
                    .deps
                    .storage
                    .register_agent(
                        session.id,
                        None,
                        serde_json::json!({
                            "provider": session.provider,
                            "transport": session.transport,
                        }),
                    )
                    .await?
            }
        };
        self.state.agents.lock().await.insert(session.id, agent_id);
        Ok(agent_id)
    }

    /// Creates a new session (persisted via `Storage`) and tracks it in-memory.
    pub async fn create_session(
        &self,
        workspace_id: xlightcli_protocol::WorkspaceId,
        provider: ProviderId,
        transport: TransportId,
        model: ModelId,
        title: Option<String>,
    ) -> Result<SessionId, RuntimeError> {
        let id = self
            .state
            .deps
            .storage
            .create_session(workspace_id, provider, transport, model, title)
            .await?;
        let record = self
            .state
            .deps
            .storage
            .get_session(id)
            .await?
            .ok_or(RuntimeError::UnknownSession(id))?;
        let session = Session::from_record(record);
        self.state.sessions.lock().await.insert(id, session);
        self.emit(UiEvent::SessionChanged { session_id: id }).await;
        Ok(id)
    }

    /// Lists persisted sessions (`core.resume`, docs/commands.md §2).
    pub async fn list_sessions(
        &self,
    ) -> Result<Vec<xlightcli_storage::SessionRecord>, RuntimeError> {
        Ok(self.state.deps.storage.list_sessions(None).await?)
    }

    /// Loads a persisted session into memory so it becomes the active target of
    /// `submit_user_input`/`run_command` (`core.resume`).
    pub async fn resume_session(&self, session_id: SessionId) -> Result<(), RuntimeError> {
        let record = self
            .state
            .deps
            .storage
            .get_session(session_id)
            .await?
            .ok_or(RuntimeError::UnknownSession(session_id))?;
        let session = Session::from_record(record);
        self.state.sessions.lock().await.insert(session_id, session);
        self.emit(UiEvent::SessionChanged { session_id }).await;
        Ok(())
    }

    /// Submits the user's next turn: drives `crate::agent::AgentLoop::run_turn` and streams the
    /// resulting `UiEvent`s to whoever called [`Self::subscribe`].
    pub async fn submit_user_input(
        &self,
        session_id: SessionId,
        text: String,
    ) -> Result<(), RuntimeError> {
        let session = self.get_session(session_id).await?;
        let agent_id = self.ensure_agent(&session).await?;
        self.run_turn_for(&session, agent_id, text, None).await?;
        Ok(())
    }

    /// Shared by [`Self::submit_user_input`] and `crate::exec::run_exec`: registers a
    /// per-session `CancellationToken` (so [`Self::cancel_turn`] has something to cancel) for the
    /// duration of the turn, then delegates to `AgentLoop::run_turn`. `headless_ask_policy`
    /// distinguishes the two callers: `None` for the interactive path (an `Ask` permission
    /// decision is routed to the UI); `Some(policy)` for headless `exec`, which has no UI to
    /// prompt (docs/commands.md §5).
    pub(crate) async fn run_turn_for(
        &self,
        session: &Session,
        agent_id: AgentId,
        text: String,
        headless_ask_policy: Option<xlightcli_tools::AskPolicy>,
    ) -> Result<crate::agent::TurnSummary, RuntimeError> {
        let cancel = CancellationToken::new();
        self.state
            .active_turns
            .lock()
            .await
            .insert(session.id, cancel.clone());

        let permission_engine =
            xlightcli_tools::PermissionEngine::from_config(&self.state.deps.config.permissions);
        let max_steps = self.state.deps.config.budget.default.max_turns.max(1);

        let turn_ctx = TurnContext {
            deps: &self.state.deps,
            session,
            agent_id,
            context: &self.state.context,
            permission_engine,
            ui_tx: self.state.ui_tx.clone(),
            pending_permissions: Arc::clone(&self.state.pending_permissions),
            cancel: cancel.clone(),
            max_steps,
            headless_ask_policy,
        };
        let result = AgentLoop::run_turn(turn_ctx, text).await;
        self.state.active_turns.lock().await.remove(&session.id);
        result
    }

    /// Runs a slash command (docs/PLAN.md §5, PATTERNS.md §12). Resolution order: core, then the
    /// active provider's `FeaturePack` (Phase 2) — only core commands exist in Phase 1, so a
    /// resolved command outside the `core.*` namespace still reports `Unavailable` rather than
    /// panicking on an unreachable match arm.
    pub async fn run_command(
        &self,
        session_id: SessionId,
        raw: &str,
    ) -> Result<CommandOutcome, RuntimeError> {
        let session = self.get_session(session_id).await?;
        let trimmed = raw.trim_start_matches('/');
        let (alias, rest) = trimmed.split_once(' ').unwrap_or((trimmed, ""));
        let rest = rest.trim();
        let def = self
            .state
            .commands
            .resolve(alias)
            .ok_or_else(|| RuntimeError::UnknownCommand(alias.to_string()))?;

        match def.id.as_str() {
            "core.help" => Ok(CommandOutcome::Message(self.render_help())),
            "core.exit" => Ok(CommandOutcome::Message(
                "Exiting xlightcli. (Confirming whether an agent is still running is the \
                 frontend's job — it should call `cancel_turn` first if so.)"
                    .to_string(),
            )),
            "core.clear" => self.run_clear(&session).await,
            "core.resume" => self.run_resume(&session).await,
            "core.model" => {
                if rest.is_empty() {
                    Ok(CommandOutcome::Message(format!(
                        "Current model: {}",
                        session.model
                    )))
                } else {
                    Ok(CommandOutcome::Unavailable {
                        reason: "switching the model of an existing session isn't implemented \
                                 yet (Wave C) — start a new session instead"
                            .to_string(),
                    })
                }
            }
            "core.context" => self.run_context(&session).await,
            "core.compact" => self.run_compact(&session, rest).await,
            "core.diff" => self.run_diff().await,
            "core.permissions" => Ok(CommandOutcome::Message(self.render_permissions())),
            "core.config" => Ok(CommandOutcome::Message(self.render_config())),
            "core.status" => Ok(CommandOutcome::Message(self.render_status(&session))),
            "core.login" => self.run_login(&session).await,
            "core.logout" => self.run_logout(&session).await,
            "core.mode" => self.run_mode(&session, rest).await,
            _ => Ok(CommandOutcome::Unavailable {
                reason: format!("{} not implemented yet (Wave B)", def.id),
            }),
        }
    }

    async fn run_login(&self, session: &Session) -> Result<CommandOutcome, RuntimeError> {
        let provider = self
            .state
            .deps
            .providers
            .get(&session.provider)
            .ok_or_else(|| {
                RuntimeError::InvalidRequest(format!("unknown provider {}", session.provider))
            })?;
        let transport = provider.transport(&session.transport).ok_or_else(|| {
            RuntimeError::InvalidRequest(format!("unknown transport {}", session.transport))
        })?;
        let method = match transport.required_auth() {
            xlightcli_protocol::AuthKind::Subscription => xlightcli_auth::AuthMethod::BrowserOAuth,
            xlightcli_protocol::AuthKind::ApiKey => xlightcli_auth::AuthMethod::ApiKey,
        };
        let ui = SessionLoginUi {
            ui_tx: self.state.ui_tx.clone(),
        };
        let account = self
            .state
            .deps
            .auth
            .login(&session.provider, method, &ui)
            .await?;
        Ok(CommandOutcome::Message(format!(
            "Logged in to {}/{} as {}",
            account.provider, account.transport, account.account_id
        )))
    }

    async fn run_logout(&self, session: &Session) -> Result<CommandOutcome, RuntimeError> {
        let accounts = self
            .state
            .deps
            .auth
            .accounts(Some(session.provider.clone()))
            .await?;
        let account = accounts
            .into_iter()
            .find(|account| account.transport == session.transport)
            .ok_or_else(|| {
                RuntimeError::InvalidRequest(format!(
                    "no account is logged in for {}/{}",
                    session.provider, session.transport
                ))
            })?;
        self.state
            .deps
            .auth
            .logout(&session.provider, &session.transport, &account.account_id)
            .await?;
        Ok(CommandOutcome::Message(format!(
            "Logged out of {}/{}",
            session.provider, session.transport
        )))
    }

    async fn run_compact(
        &self,
        session: &Session,
        instructions: &str,
    ) -> Result<CommandOutcome, RuntimeError> {
        let provider = self
            .state
            .deps
            .providers
            .get(&session.provider)
            .ok_or_else(|| {
                RuntimeError::InvalidRequest(format!("unknown provider {}", session.provider))
            })?;
        let transport = provider.transport(&session.transport).ok_or_else(|| {
            RuntimeError::InvalidRequest(format!(
                "provider {} has no transport {}",
                session.provider, session.transport
            ))
        })?;
        let credential = self
            .state
            .deps
            .auth
            .credential(&session.provider, &session.transport)
            .await?;
        let agent_id = self.ensure_agent(session).await?;
        let mut request = self
            .state
            .context
            .build_turn_request(&self.state.deps.storage, &self.state.deps.tools, session)
            .await?;
        if request.messages.is_empty() {
            return Ok(CommandOutcome::Message(
                "Nothing to compact yet.".to_string(),
            ));
        }
        request.tools.clear();
        request.system = xlightcli_protocol::SystemPrompt::new(
            "Summarize this coding conversation for future continuation. Preserve goals, decisions, relevant file paths, pending work, and important tool results. Do not claim unverified work is complete.",
        );
        request
            .messages
            .push(xlightcli_protocol::Message::user_text(format!(
                "Produce a concise factual continuation summary. {instructions}"
            )));
        let mut stream = transport
            .stream(request, credential, CancellationToken::new())
            .await?;
        let mut summary = None;
        while let Some(event) = stream.next().await {
            if let xlightcli_protocol::AgentEvent::Completed { message, stop, .. } = event? {
                if stop != StopReason::EndTurn {
                    return Err(RuntimeError::InvalidRequest(format!(
                        "compaction did not complete: {stop:?}"
                    )));
                }
                let text = message
                    .content
                    .into_iter()
                    .filter_map(|block| match block {
                        xlightcli_protocol::ContentBlock::Text { text } => Some(text),
                        _ => None,
                    })
                    .collect::<String>();
                if !text.trim().is_empty() {
                    summary = Some(text);
                }
                break;
            }
        }
        let text = summary.ok_or_else(|| {
            RuntimeError::InvalidRequest("compaction produced no summary".to_string())
        })?;
        let seq = self.state.deps.storage.latest_event_seq(session.id).await?;
        self.state
            .deps
            .storage
            .save_summary(session.id, agent_id, seq, text)
            .await?;
        Ok(CommandOutcome::Message(
            "Context summary saved for this session.".to_string(),
        ))
    }

    fn render_help(&self) -> String {
        let mut lines = vec!["Core commands:".to_string()];
        for def in self.state.commands.core_commands() {
            lines.push(format!("  /{:<12} {}", def.alias, def.summary));
        }
        lines.join("\n")
    }

    fn render_permissions(&self) -> String {
        let cfg = &self.state.deps.config.permissions;
        let mut lines = vec![format!("mode: {:?}", cfg.mode)];
        for rule in cfg.rules() {
            lines.push(format!("  {:?} {}", rule.effect, rule.to_raw()));
        }
        lines.join("\n")
    }

    fn render_config(&self) -> String {
        serde_json::to_string_pretty(&self.state.deps.config)
            .unwrap_or_else(|_| "<config failed to serialize>".to_string())
    }

    fn render_status(&self, session: &Session) -> String {
        format!(
            "xlightcli {}\nsession: {}\nprovider: {} ({})\nmodel: {}\nexecution mode: {:?}",
            env!("CARGO_PKG_VERSION"),
            session.id,
            session.provider,
            session.transport,
            session.model,
            session.mode
        )
    }

    async fn run_clear(&self, session: &Session) -> Result<CommandOutcome, RuntimeError> {
        let new_id = self
            .create_session(
                session.workspace_id,
                session.provider.clone(),
                session.transport.clone(),
                session.model.clone(),
                None,
            )
            .await?;
        Ok(CommandOutcome::Message(format!(
            "Started a new session {new_id} — the previous session {} can still be resumed via \
             /resume.",
            session.id
        )))
    }

    async fn run_resume(&self, session: &Session) -> Result<CommandOutcome, RuntimeError> {
        let mut sessions = self
            .state
            .deps
            .storage
            .list_sessions(Some(session.workspace_id))
            .await?;
        if sessions.is_empty() {
            return Ok(CommandOutcome::Message(
                "No sessions found for this workspace.".to_string(),
            ));
        }
        sessions.sort_by_key(|record| std::cmp::Reverse(record.updated_at));
        let mut lines = vec!["Sessions in this workspace (most recent first):".to_string()];
        for record in sessions {
            lines.push(format!(
                "  {} [{:?}] {}",
                record.id,
                record.status,
                record.title.as_deref().unwrap_or("(untitled)")
            ));
        }
        Ok(CommandOutcome::Message(lines.join("\n")))
    }

    async fn run_context(&self, session: &Session) -> Result<CommandOutcome, RuntimeError> {
        let request = self
            .state
            .context
            .build_turn_request(&self.state.deps.storage, &self.state.deps.tools, session)
            .await?;
        let system_tokens = crate::context::estimate_tokens(&request.system.text);
        let history_tokens: u64 = request
            .messages
            .iter()
            .map(crate::context::estimate_message_tokens)
            .sum();
        let tools_tokens: u64 = request
            .tools
            .iter()
            .map(|def| {
                crate::context::estimate_tokens(&def.description)
                    + crate::context::estimate_tokens(&def.input_schema.to_string())
            })
            .sum();
        Ok(CommandOutcome::Message(format!(
            "Context breakdown (chars/4 heuristic, docs/PLAN.md §9.2):\n  \
             system + rules: ~{system_tokens} tokens\n  \
             history ({} messages): ~{history_tokens} tokens\n  \
             tools ({}): ~{tools_tokens} tokens\n  \
             total: ~{} tokens",
            request.messages.len(),
            request.tools.len(),
            system_tokens + history_tokens + tools_tokens
        )))
    }

    async fn run_mode(
        &self,
        session: &Session,
        rest: &str,
    ) -> Result<CommandOutcome, RuntimeError> {
        let new_mode = if rest.is_empty() {
            session.mode.next()
        } else {
            match rest {
                "default" => xlightcli_tools::ExecutionMode::Default,
                "accept-edits" | "auto-edit" => xlightcli_tools::ExecutionMode::AcceptEdits,
                "plan" => xlightcli_tools::ExecutionMode::Plan,
                other => {
                    return Ok(CommandOutcome::Unavailable {
                        reason: format!(
                            "unknown mode {other:?} (expected default|accept-edits|plan)"
                        ),
                    });
                }
            }
        };
        self.set_execution_mode(session.id, new_mode).await?;
        Ok(CommandOutcome::Message(format!(
            "Execution mode: {new_mode:?}"
        )))
    }

    /// `/diff` (docs/commands.md §2 `core.diff`): shells out to `git diff` via `ProcessLauncher`
    /// (INV-1) and reports the result through `OutputSpool` (INV-7) rather than buffering the
    /// whole thing. A `git` failure (not a repo, `git` missing, timeout) is reported as
    /// `Unavailable`, never a crash (INV-11).
    async fn run_diff(&self) -> Result<CommandOutcome, RuntimeError> {
        let launcher = xlightcli_tools::ProcessLauncher::new();
        let cancel = CancellationToken::new();
        let spec = xlightcli_tools::SpawnSpec {
            purpose: xlightcli_tools::SpawnPurpose::Git,
            program: "git".to_string(),
            args: vec!["diff".to_string(), "--no-color".to_string()],
            cwd: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            env: xlightcli_tools::EnvPolicy::scrubbed(),
            timeout: Some(std::time::Duration::from_secs(30)),
            cancel: cancel.clone(),
        };
        let process = match launcher.spawn(spec).await {
            Ok(process) => process,
            Err(err) => {
                return Ok(CommandOutcome::Unavailable {
                    reason: format!("could not run `git diff`: {err}"),
                });
            }
        };
        let mut spool = match xlightcli_tools::OutputSpool::create(
            &xlightcli_config::paths::artifacts_dir(),
            SessionId::new(),
            ToolCallId::new("core.diff"),
            xlightcli_tools::SpoolLimits::from_config(&self.state.deps.config.tools),
        )
        .await
        {
            Ok(spool) => spool,
            Err(err) => {
                return Ok(CommandOutcome::Unavailable {
                    reason: format!("could not open a spool for `git diff`'s output: {err}"),
                });
            }
        };
        let status = process.pipe_into(&mut spool).await;
        let Ok(summary) = spool.finish().await else {
            return Ok(CommandOutcome::Unavailable {
                reason: "could not finish the `git diff` output spool".to_string(),
            });
        };
        match status {
            Ok(exit) if exit.success() => {
                if summary.total_bytes == 0 {
                    Ok(CommandOutcome::Message(
                        "No changes (working tree clean).".to_string(),
                    ))
                } else {
                    Ok(CommandOutcome::Message(format!(
                        "{}\n...\n{}",
                        summary.head_text(),
                        summary.tail_text()
                    )))
                }
            }
            _ => Ok(CommandOutcome::Unavailable {
                reason: "`git diff` failed or timed out — is this a git repository?".to_string(),
            }),
        }
    }

    /// Answers a pending `UiEvent::PermissionRequested`: correlates `tool_call_id` with the
    /// `crate::permission_gate::RuntimePermissionGate` awaiting it and resolves its oneshot.
    pub async fn respond_to_permission(
        &self,
        tool_call_id: ToolCallId,
        response: PermissionResponse,
    ) -> Result<(), RuntimeError> {
        let sender = self
            .state
            .pending_permissions
            .lock()
            .await
            .remove(&tool_call_id);
        match sender {
            Some(tx) => {
                // The gate may have already given up (e.g. the turn was cancelled while this
                // answer was in flight); a dropped receiver here is not an error for the caller.
                let _ = tx.send(response);
                Ok(())
            }
            None => Err(RuntimeError::NoPendingPermission(tool_call_id)),
        }
    }

    /// Cancels the in-flight turn for `session_id`, if any.
    pub async fn cancel_turn(&self, session_id: SessionId) -> Result<(), RuntimeError> {
        self.get_session(session_id).await?;
        match self.state.active_turns.lock().await.get(&session_id) {
            Some(token) => {
                token.cancel();
                Ok(())
            }
            None => Err(RuntimeError::NoActiveTurn(session_id)),
        }
    }

    /// Cycles or sets the execution mode (Shift+Tab, D-025) for `session_id`.
    pub async fn set_execution_mode(
        &self,
        session_id: SessionId,
        mode: xlightcli_tools::ExecutionMode,
    ) -> Result<(), RuntimeError> {
        let mut sessions = self.state.sessions.lock().await;
        let session = sessions
            .get_mut(&session_id)
            .ok_or(RuntimeError::UnknownSession(session_id))?;
        session.mode = mode;
        drop(sessions);
        self.emit(UiEvent::ModeChanged { session_id, mode }).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// Opens (creating if needed) a `RuntimeHandle` backed by real storage at `path` — shared by
    /// [`test_handle`] (fresh tempdir) and the crash-recovery test below (same path, reopened).
    async fn test_handle_at(path: std::path::PathBuf) -> RuntimeHandle {
        let storage = xlightcli_storage::Storage::open(&path).await.unwrap();
        let deps = RuntimeDeps {
            providers: Arc::new(xlightcli_provider::ProviderRegistry::new()),
            auth: Arc::new(xlightcli_auth::AuthBroker::new()),
            tools: Arc::new(xlightcli_tools::ToolRegistry::with_builtins()),
            storage,
            config: xlightcli_config::Config::default(),
            workspace_trust: Arc::new(
                xlightcli_config::TrustStore::load(path.with_extension("trust.toml")).unwrap(),
            ),
            allow_dangerous_permissions: false,
        };
        RuntimeHandle::new(deps, RuntimeConfig::default())
    }

    async fn test_handle() -> (RuntimeHandle, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let handle = test_handle_at(dir.path().join("test.db")).await;
        (handle, dir)
    }

    #[tokio::test]
    async fn interactive_picker_can_create_a_persisted_session() {
        let mut config = xlightcli_config::Config::default();
        config.provider.insert(
            crate::test_support::TEST_PROVIDER.to_string(),
            xlightcli_config::ProviderDefaults {
                transport: Some(TransportId::new(crate::test_support::TEST_TRANSPORT)),
                default_model: Some(ModelId::new(crate::test_support::TEST_MODEL)),
            },
        );
        let harness = crate::test_support::build_harness(
            Vec::new(),
            std::time::Duration::ZERO,
            None,
            xlightcli_tools::ToolRegistry::new(),
            config,
        )
        .await;
        let handle = &harness.handle;
        let provider = ProviderId::new(crate::test_support::TEST_PROVIDER);
        let transport = TransportId::new(crate::test_support::TEST_TRANSPORT);
        assert_eq!(handle.session_providers().len(), 1);
        assert_eq!(handle.session_transports(&provider).unwrap().len(), 1);
        let models = handle.session_models(&provider, &transport).await.unwrap();
        assert_eq!(models[0].id.as_str(), crate::test_support::TEST_MODEL);

        let session_id = handle
            .start_session(provider.clone(), transport.clone(), models[0].id.clone())
            .await
            .unwrap();
        let session = handle
            .deps()
            .storage
            .get_session(session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.provider, provider);
        assert_eq!(session.transport, transport);
        assert_eq!(session.model, models[0].id);
    }

    #[tokio::test]
    async fn interactive_picker_accepts_a_custom_model_when_catalog_is_empty() {
        let harness = crate::test_support::build_harness(
            Vec::new(),
            std::time::Duration::ZERO,
            None,
            xlightcli_tools::ToolRegistry::new(),
            xlightcli_config::Config::default(),
        )
        .await;
        let provider = ProviderId::new(crate::test_support::TEST_PROVIDER);
        let transport = TransportId::new(crate::test_support::TEST_TRANSPORT);
        assert!(
            harness
                .handle
                .session_models(&provider, &transport)
                .await
                .unwrap()
                .is_empty()
        );
        let model = ModelId::new("gpt-6-luna");
        let session_id = harness
            .handle
            .start_session(provider, transport, model.clone())
            .await
            .unwrap();
        assert_eq!(
            harness.handle.get_session(session_id).await.unwrap().model,
            model
        );
    }

    #[tokio::test]
    async fn subscribe_can_only_be_called_once() {
        let (handle, _dir) = test_handle().await;
        assert!(handle.subscribe().await.is_ok());
        assert!(matches!(
            handle.subscribe().await,
            Err(RuntimeError::AlreadySubscribed)
        ));
    }

    #[tokio::test]
    async fn create_and_resume_session_round_trip_emits_session_changed() {
        let (handle, _dir) = test_handle().await;
        let mut ui = handle.subscribe().await.unwrap();

        let workspace = handle
            .state
            .deps
            .storage
            .create_workspace(std::path::PathBuf::from("/repo"), None)
            .await
            .unwrap();
        let session_id = handle
            .create_session(
                workspace,
                ProviderId::new("codex"),
                TransportId::new("chatgpt"),
                ModelId::new("gpt-5"),
                None,
            )
            .await
            .unwrap();
        assert!(matches!(
            ui.recv().await,
            Some(UiEvent::SessionChanged { .. })
        ));

        handle.resume_session(session_id).await.unwrap();
        assert!(matches!(
            ui.recv().await,
            Some(UiEvent::SessionChanged { .. })
        ));
    }

    #[tokio::test]
    async fn unknown_session_is_reported() {
        let (handle, _dir) = test_handle().await;
        let err = handle.cancel_turn(SessionId::new()).await.unwrap_err();
        assert!(matches!(err, RuntimeError::UnknownSession(_)));
    }

    #[tokio::test]
    async fn set_execution_mode_updates_session_and_emits_event() {
        let (handle, _dir) = test_handle().await;
        let mut ui = handle.subscribe().await.unwrap();
        let workspace = handle
            .state
            .deps
            .storage
            .create_workspace(std::path::PathBuf::from("/repo"), None)
            .await
            .unwrap();
        let session_id = handle
            .create_session(
                workspace,
                ProviderId::new("codex"),
                TransportId::new("chatgpt"),
                ModelId::new("gpt-5"),
                None,
            )
            .await
            .unwrap();
        let _ = ui.recv().await; // SessionChanged from create_session

        handle
            .set_execution_mode(session_id, xlightcli_tools::ExecutionMode::Plan)
            .await
            .unwrap();
        assert!(matches!(
            ui.recv().await,
            Some(UiEvent::ModeChanged {
                mode: xlightcli_tools::ExecutionMode::Plan,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn run_command_help_returns_a_real_message() {
        let (handle, _dir) = test_handle().await;
        let workspace = handle
            .state
            .deps
            .storage
            .create_workspace(std::path::PathBuf::from("/repo"), None)
            .await
            .unwrap();
        let session_id = handle
            .create_session(
                workspace,
                ProviderId::new("codex"),
                TransportId::new("chatgpt"),
                ModelId::new("gpt-5"),
                None,
            )
            .await
            .unwrap();

        let outcome = handle.run_command(session_id, "/help").await.unwrap();
        match outcome {
            CommandOutcome::Message(text) => assert!(text.contains("/help")),
            other => panic!("expected a Message, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_command_status_reports_session_metadata() {
        let (handle, _dir) = test_handle().await;
        let workspace = handle
            .state
            .deps
            .storage
            .create_workspace(std::path::PathBuf::from("/repo"), None)
            .await
            .unwrap();
        let session_id = handle
            .create_session(
                workspace,
                ProviderId::new("codex"),
                TransportId::new("chatgpt"),
                ModelId::new("gpt-5"),
                None,
            )
            .await
            .unwrap();

        let outcome = handle.run_command(session_id, "/status").await.unwrap();
        match outcome {
            CommandOutcome::Message(text) => {
                assert!(text.contains("codex"));
                assert!(text.contains("gpt-5"));
            }
            other => panic!("expected a Message, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_command_mode_cycles_execution_mode() {
        let (handle, _dir) = test_handle().await;
        let workspace = handle
            .state
            .deps
            .storage
            .create_workspace(std::path::PathBuf::from("/repo"), None)
            .await
            .unwrap();
        let session_id = handle
            .create_session(
                workspace,
                ProviderId::new("codex"),
                TransportId::new("chatgpt"),
                ModelId::new("gpt-5"),
                None,
            )
            .await
            .unwrap();

        handle.run_command(session_id, "/mode").await.unwrap();
        let session = handle.get_session(session_id).await.unwrap();
        assert_eq!(session.mode, xlightcli_tools::ExecutionMode::AcceptEdits);
    }

    #[tokio::test]
    async fn run_command_compact_saves_a_model_summary() {
        let harness = crate::test_support::build_harness(
            vec![vec![crate::test_support::completed(
                crate::test_support::text_message("Keep the Rust tests green."),
                StopReason::EndTurn,
            )]],
            std::time::Duration::ZERO,
            None,
            xlightcli_tools::ToolRegistry::new(),
            xlightcli_config::Config::default(),
        )
        .await;
        let handle = &harness.handle;
        let session_id = crate::test_support::new_session(&harness).await;
        let session = handle.get_session(session_id).await.unwrap();
        let agent_id = handle.ensure_agent(&session).await.unwrap();
        handle
            .deps()
            .storage
            .append_message(
                session_id,
                agent_id,
                1,
                xlightcli_protocol::Role::User,
                vec![xlightcli_protocol::ContentBlock::Text {
                    text: "Please help with Rust tests".to_string(),
                }],
            )
            .await
            .unwrap();

        let outcome = handle.run_command(session_id, "/compact").await.unwrap();
        assert!(matches!(outcome, CommandOutcome::Message(_)));
        let summary = handle
            .deps()
            .storage
            .latest_summary(session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(summary.0, "Keep the Rust tests green.");
    }

    #[tokio::test]
    async fn slash_logout_then_login_updates_the_auth_broker() {
        let harness = crate::test_support::build_harness(
            Vec::new(),
            std::time::Duration::ZERO,
            None,
            xlightcli_tools::ToolRegistry::new(),
            xlightcli_config::Config::default(),
        )
        .await;
        let handle = &harness.handle;
        let session_id = crate::test_support::new_session(&harness).await;
        assert!(matches!(
            handle.run_command(session_id, "/logout").await.unwrap(),
            CommandOutcome::Message(_)
        ));
        assert!(handle.deps().auth.accounts(None).await.unwrap().is_empty());
        assert!(matches!(
            handle.run_command(session_id, "/login").await.unwrap(),
            CommandOutcome::Message(_)
        ));
        assert_eq!(handle.deps().auth.accounts(None).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn run_command_rejects_unknown_alias() {
        let (handle, _dir) = test_handle().await;
        let workspace = handle
            .state
            .deps
            .storage
            .create_workspace(std::path::PathBuf::from("/repo"), None)
            .await
            .unwrap();
        let session_id = handle
            .create_session(
                workspace,
                ProviderId::new("codex"),
                TransportId::new("chatgpt"),
                ModelId::new("gpt-5"),
                None,
            )
            .await
            .unwrap();

        let err = handle
            .run_command(session_id, "/not-a-real-command")
            .await
            .unwrap_err();
        assert!(matches!(err, RuntimeError::UnknownCommand(_)));
    }

    /// docs/PLAN.md §11.2 "an in-progress turn is marked Interrupted" + this Wave B brief's
    /// "resume after simulated crash": a session left `active` (no `update_session_status` call —
    /// simulating a process that died mid-turn) must (1) come back as `Interrupted` the next time
    /// `Storage::open` runs against the same file (it does this automatically), and (2)
    /// `ensure_agent` must recover the previously-registered root agent instead of registering a
    /// second one.
    #[tokio::test]
    async fn resume_after_simulated_crash_recovers_status_and_agent() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        let session_id;
        let agent_id_before_crash;
        {
            let handle = test_handle_at(db_path.clone()).await;
            let workspace = handle
                .state
                .deps
                .storage
                .create_workspace(std::path::PathBuf::from("/repo"), None)
                .await
                .unwrap();
            session_id = handle
                .create_session(
                    workspace,
                    ProviderId::new("codex"),
                    TransportId::new("chatgpt"),
                    ModelId::new("gpt-5"),
                    None,
                )
                .await
                .unwrap();
            let session = handle.get_session(session_id).await.unwrap();
            agent_id_before_crash = handle.ensure_agent(&session).await.unwrap();
            // No `update_session_status` call and `handle` is dropped here — simulating a crash
            // mid-turn: the session is left `active` and only ever has its root agent registered.
        }

        // "Restart": reopen storage at the same path. `Storage::open` calls
        // `mark_interrupted_on_open` automatically.
        let handle = test_handle_at(db_path).await;
        let record = handle
            .state
            .deps
            .storage
            .get_session(session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.status, xlightcli_storage::SessionStatus::Interrupted);

        handle.resume_session(session_id).await.unwrap();
        let session = handle.get_session(session_id).await.unwrap();
        let agent_id_after_resume = handle.ensure_agent(&session).await.unwrap();
        assert_eq!(
            agent_id_before_crash, agent_id_after_resume,
            "ensure_agent must recover the previously-registered root agent, not duplicate it"
        );

        let agents = handle
            .state
            .deps
            .storage
            .list_agents(session_id)
            .await
            .unwrap();
        assert_eq!(agents.len(), 1);
    }
}
