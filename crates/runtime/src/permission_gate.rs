// SPDX-License-Identifier: GPL-3.0-only

//! `RuntimePermissionGate` — the runtime's `xlightcli_tools::PermissionGate` implementation
//! (docs/PLAN.md §7.3, this Wave B brief's "the `PermissionGate` implemented by the runtime").
//!
//! One instance is built per tool call (it captures that call's `ToolCallId`, matching how
//! `xlightcli_tools::ToolContext` is itself constructed per call). It layers three things, in
//! order:
//!
//! 1. `xlightcli_tools::PermissionEngine::evaluate` (deny/ask/allow rules + `PermissionMode`
//!    default) — a matching `deny` rule always wins, before mode/execution-mode logic runs.
//! 2. The session's `ExecutionMode` (D-025): `Plan` blocks every non-read-only action outright
//!    (never asks); `AcceptEdits` auto-allows `WriteFile` actions the engine didn't already deny.
//! 3. Whatever is left as `Ask` is either routed to the UI (`UiEvent::PermissionRequested`,
//!    interactive) or resolved immediately per a fixed [`AskPolicy`] (headless `exec`, docs/PLAN.md
//!    §9.3 — there is no UI to prompt).

use async_trait::async_trait;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use xlightcli_protocol::{AgentId, ToolCallId};
use xlightcli_tools::{
    AskPolicy, ExecutionMode, PermissionAction, PermissionDecision, PermissionEngine,
    PermissionGate, PermissionRequest, ToolError,
};

use crate::handle::{PermissionResponse, UiEvent};

/// Shared map from an in-flight tool call to the oneshot sender that resolves its pending
/// `PermissionRequested` UI event. `RuntimeHandle::respond_to_permission` looks a call up here and
/// sends the user's answer; the [`RuntimePermissionGate`] built for that call is the one awaiting
/// the other end.
pub type PendingPermissions = std::sync::Arc<
    Mutex<std::collections::HashMap<ToolCallId, oneshot::Sender<PermissionResponse>>>,
>;

/// How [`RuntimePermissionGate::check`] resolves an `Ask` decision when there is no UI to prompt
/// (headless `xlightcli exec`, docs/commands.md §5: "ask ⇒ deny unless
/// `--dangerously-skip-permissions`").
pub type HeadlessAskPolicy = AskPolicy;

/// **Not** `#[derive(Debug)]`: `pending` holds a `tokio::sync::oneshot::Sender`, which doesn't
/// implement `Debug` at all (checked against the pinned tokio version — no `impl Debug for
/// Sender<T>` exists, even a conditional one), so a derive would fail to compile. Manual impl
/// below satisfies `[workspace.lints.rust] missing_debug_implementations` without printing any of
/// the channel/permission internals.
pub struct RuntimePermissionGate {
    engine: PermissionEngine,
    mode: ExecutionMode,
    agent_id: AgentId,
    call_id: ToolCallId,
    ui_tx: mpsc::Sender<UiEvent>,
    pending: PendingPermissions,
    cancel: CancellationToken,
    /// `Some(policy)` in headless `exec` (no UI to prompt, resolve immediately); `None` when an
    /// interactive frontend is subscribed and can answer `UiEvent::PermissionRequested`.
    headless: Option<HeadlessAskPolicy>,
}

impl RuntimePermissionGate {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        engine: PermissionEngine,
        mode: ExecutionMode,
        agent_id: AgentId,
        call_id: ToolCallId,
        ui_tx: mpsc::Sender<UiEvent>,
        pending: PendingPermissions,
        cancel: CancellationToken,
        headless: Option<HeadlessAskPolicy>,
    ) -> Self {
        Self {
            engine,
            mode,
            agent_id,
            call_id,
            ui_tx,
            pending,
            cancel,
            headless,
        }
    }

    async fn ask(&self, action: &PermissionAction<'_>, reason: String) -> Result<(), ToolError> {
        if let Some(policy) = self.headless {
            return match policy {
                AskPolicy::AutoAllow => Ok(()),
                AskPolicy::AutoDeny => Err(ToolError::PermissionDenied(reason)),
            };
        }

        let request = PermissionRequest {
            action: action.action_name().to_string(),
            target: action.target().to_string(),
            tool_call_id: Some(self.call_id.clone()),
            reason,
        };
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(self.call_id.clone(), tx);
        let _ = self
            .ui_tx
            .send(UiEvent::PermissionRequested {
                agent_id: self.agent_id,
                request,
            })
            .await;

        tokio::select! {
            biased;
            _ = self.cancel.cancelled() => {
                self.pending.lock().await.remove(&self.call_id);
                Err(ToolError::Cancelled)
            }
            resp = rx => {
                match resp {
                    Ok(PermissionResponse::Allow) => Ok(()),
                    Ok(PermissionResponse::Deny) | Err(_) => {
                        Err(ToolError::PermissionDenied("denied by the user".to_string()))
                    }
                }
            }
        }
    }
}

impl std::fmt::Debug for RuntimePermissionGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimePermissionGate")
            .field("mode", &self.mode)
            .field("agent_id", &self.agent_id)
            .field("call_id", &self.call_id)
            .field("headless", &self.headless)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl PermissionGate for RuntimePermissionGate {
    async fn check(&self, action: PermissionAction<'_>) -> Result<(), ToolError> {
        let decision = self.engine.evaluate(&action);
        if let PermissionDecision::Deny { reason } = decision {
            return Err(ToolError::PermissionDenied(reason));
        }

        // Execution-mode overrides (D-025), applied after any explicit `deny` rule but before
        // whatever the engine/mode-default would otherwise decide.
        if self.mode == ExecutionMode::Plan && !action.is_read_only() {
            return Err(ToolError::PermissionDenied(format!(
                "{} is blocked: execution mode is plan (read-only tools only)",
                action.action_name()
            )));
        }
        if self.mode == ExecutionMode::AcceptEdits
            && matches!(action, PermissionAction::WriteFile(_))
        {
            return Ok(());
        }

        match decision {
            PermissionDecision::Allow => Ok(()),
            PermissionDecision::Ask { reason } => self.ask(&action, reason).await,
            PermissionDecision::Deny { .. } => unreachable!("handled above"),
        }
    }
}
