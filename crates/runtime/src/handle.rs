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

use tokio::sync::{Mutex, mpsc};
use xlightcli_protocol::{
    AgentId, ModelId, ProviderId, SessionId, StopReason, ToolCallId, TransportId, Usage,
};

use crate::commands::CommandRegistry;
use crate::error::RuntimeError;
use crate::session::Session;

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
        Self {
            state: Arc::new(RuntimeState {
                deps,
                commands: CommandRegistry::with_core_commands(),
                ui_tx,
                ui_rx: Mutex::new(Some(ui_rx)),
                sessions: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn deps(&self) -> &RuntimeDeps {
        &self.state.deps
    }

    pub fn commands(&self) -> &CommandRegistry {
        &self.state.commands
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

    async fn get_session(&self, session_id: SessionId) -> Result<Session, RuntimeError> {
        self.state
            .sessions
            .lock()
            .await
            .get(&session_id)
            .cloned()
            .ok_or(RuntimeError::UnknownSession(session_id))
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

    /// Submits the user's next turn. Wave B: drives `crate::agent::AgentLoop::run_turn` and
    /// streams the resulting `UiEvent`s; for now, validates the session exists and reports
    /// `NotImplemented`.
    pub async fn submit_user_input(
        &self,
        session_id: SessionId,
        _text: String,
    ) -> Result<(), RuntimeError> {
        let session = self.get_session(session_id).await?;
        crate::agent::AgentLoop::run_turn(&self.state.deps, &session).await
    }

    /// Runs a slash command (docs/PLAN.md §5, PATTERNS.md §12). Resolution order: core, then the
    /// active provider's `FeaturePack` (Phase 2) — only core commands exist in Phase 1.
    pub async fn run_command(
        &self,
        session_id: SessionId,
        raw: &str,
    ) -> Result<CommandOutcome, RuntimeError> {
        let _session = self.get_session(session_id).await?;
        let (alias, _rest) = raw
            .trim_start_matches('/')
            .split_once(' ')
            .unwrap_or((raw.trim_start_matches('/'), ""));
        match self.state.commands.resolve(alias) {
            Some(def) => Ok(CommandOutcome::Unavailable {
                reason: format!("{} not implemented yet (Wave B)", def.id),
            }),
            None => Err(RuntimeError::UnknownCommand(alias.to_string())),
        }
    }

    /// Answers a pending `UiEvent::PermissionRequested`. Wave B: correlates `tool_call_id` with
    /// the tool executor's waiting future; for now, always reports no pending request (there is no
    /// real tool executor yet to have created one).
    pub async fn respond_to_permission(
        &self,
        tool_call_id: ToolCallId,
        _response: PermissionResponse,
    ) -> Result<(), RuntimeError> {
        Err(RuntimeError::NoPendingPermission(tool_call_id))
    }

    /// Cancels the in-flight turn for `session_id`, if any. Wave B: cancels the session's
    /// `CancellationToken`; for now, always reports no active turn.
    pub async fn cancel_turn(&self, session_id: SessionId) -> Result<(), RuntimeError> {
        self.get_session(session_id).await?;
        Err(RuntimeError::NoActiveTurn(session_id))
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

    async fn test_handle() -> (RuntimeHandle, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let storage = xlightcli_storage::Storage::open(dir.path().join("test.db"))
            .await
            .unwrap();
        let deps = RuntimeDeps {
            providers: Arc::new(xlightcli_provider::ProviderRegistry::new()),
            auth: Arc::new(xlightcli_auth::AuthBroker::new()),
            tools: Arc::new(xlightcli_tools::ToolRegistry::with_builtins()),
            storage,
            config: xlightcli_config::Config::default(),
        };
        (RuntimeHandle::new(deps, RuntimeConfig::default()), dir)
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
    async fn run_command_resolves_core_commands_as_unavailable() {
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
        assert!(matches!(outcome, CommandOutcome::Unavailable { .. }));
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
}
