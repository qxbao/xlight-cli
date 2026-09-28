// SPDX-License-Identifier: GPL-3.0-only

//! `AgentLoop` (docs/PLAN.md §9.1): the single, provider-independent agent loop. No `match
//! provider_id` anywhere in this file (INV-2, enforced by `cargo xtask check-deps` refusing this
//! crate any `provider-*` dependency at all).
//!
//! ```text
//! loop {
//!     req    = context_manager.build(session, agent_profile)
//!     stream = transport.stream(req, credential_handle, cancel)
//!     for event in stream { persist (batched) + forward UiEvent }
//!     match stop {
//!         ToolUse   => results = tool_executor.run(calls, permissions).await; append ToolResult; continue
//!         EndTurn   => break
//!         MaxTokens => stop (Wave B policy: don't auto-continue past a token cap)
//!         Cancelled => break
//!     }
//!     budget.check()?
//! }
//! ```
//!
//! **[Wave B signature change, flagged per the brief]:** `AgentLoop::run_turn` no longer takes
//! `(&RuntimeDeps, &Session)` — that shape had no way to reach the `UiEvent` channel, the pending
//! permission map, or a per-turn `CancellationToken`, all of which live on `RuntimeHandle`'s
//! private state, not on `RuntimeDeps` (docs/CONTRACTS.md §10 documents `RuntimeDeps` as the
//! process-wide shared resources only). It now takes a [`TurnContext`] bundling everything one
//! turn needs. `AgentLoop` is only ever called from `RuntimeHandle::submit_user_input` and
//! `crate::exec::run_exec` (both in this crate); `tui`/`app` only ever see `RuntimeHandle`'s own
//! methods, whose signatures are unchanged, so this is not visible outside `runtime`.

use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use xlightcli_protocol::{
    AgentEvent, AgentId, ContentBlock, Message, ProviderError, Role, StopReason, ToolCallId, Usage,
};
use xlightcli_storage::{NewToolCall, NewUsageRow, SessionStatus, ToolCallStatus};
use xlightcli_tools::{AskPolicy, PermissionEngine, ToolOutput};

use crate::context::ContextManager;
use crate::error::RuntimeError;
use crate::handle::{NoticeLevel, RuntimeDeps, ToolCallSummary, UiEvent};
use crate::permission_gate::{PendingPermissions, RuntimePermissionGate};
use crate::session::Session;

pub use crate::permission_gate::HeadlessAskPolicy;

/// Everything one call to [`AgentLoop::run_turn`] needs beyond the user's text, gathered in one
/// place instead of an unwieldy positional argument list.
pub struct TurnContext<'a> {
    pub deps: &'a RuntimeDeps,
    pub session: &'a Session,
    pub agent_id: AgentId,
    pub context: &'a ContextManager,
    pub permission_engine: PermissionEngine,
    pub ui_tx: mpsc::Sender<UiEvent>,
    pub pending_permissions: PendingPermissions,
    pub cancel: CancellationToken,
    /// Turn/tool-loop guard (docs/PLAN.md §10.3 `budget.default.max_turns`): at most this many
    /// provider round trips before the loop gives up rather than looping forever.
    pub max_steps: u32,
    /// `Some(policy)` for headless `exec` (no UI to prompt an `Ask` decision with); `None` for an
    /// interactive frontend subscribed to `UiEvent::PermissionRequested`.
    pub headless_ask_policy: Option<HeadlessAskPolicy>,
}

/// Manual impl (not `#[derive(Debug)]`): `pending_permissions` holds a `oneshot::Sender`, which
/// has no `Debug` impl at all (see `crate::permission_gate::RuntimePermissionGate`'s doc comment
/// for the same note) — a derive here would fail to compile.
impl std::fmt::Debug for TurnContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnContext")
            .field("session", &self.session.id)
            .field("agent_id", &self.agent_id)
            .field("max_steps", &self.max_steps)
            .field("headless_ask_policy", &self.headless_ask_policy)
            .finish_non_exhaustive()
    }
}

/// What one turn produced, once the loop stops (docs/PLAN.md §9.1).
#[derive(Debug, Clone)]
pub struct TurnSummary {
    /// Concatenated `Text` blocks of the final assistant message (what `exec` reports as
    /// `ExecOutput.response`).
    pub response_text: String,
    pub usage: Usage,
    pub stop: StopReason,
}

fn add_usage(a: Usage, b: Usage) -> Usage {
    Usage {
        input_tokens: a.input_tokens + b.input_tokens,
        output_tokens: a.output_tokens + b.output_tokens,
        cached_input_tokens: a.cached_input_tokens + b.cached_input_tokens,
        reasoning_tokens: a.reasoning_tokens + b.reasoning_tokens,
    }
}

fn message_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Decides how a mid-turn provider failure is reported (D-026 exit-code philosophy: exit `1` —
/// "general error" — when nothing was produced yet vs. exit `3` — "error after output was
/// already produced" — when it was; `exec::run_exec`/`app::cmd::exec` map an `Ok(TurnSummary)`
/// vs. `Err(RuntimeError)` return here to those two cases). Total failure (nothing streamed yet,
/// e.g. the very first step of the turn) still surfaces as `Err` so a caller with no output to
/// show can't mistake it for success.
fn provider_error_outcome(
    err: ProviderError,
    response_text: String,
    usage: Usage,
) -> Result<TurnSummary, RuntimeError> {
    if response_text.is_empty() {
        Err(RuntimeError::Provider(err))
    } else {
        Ok(TurnSummary {
            response_text,
            usage,
            stop: StopReason::Other(format!("provider_error_after_partial_output: {err}")),
        })
    }
}

fn truncate_preview(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars).collect();
    format!("{truncated}…")
}

/// Namespace for the agent loop entry point.
#[derive(Debug)]
pub struct AgentLoop;

impl AgentLoop {
    /// Runs one full turn for `ctx.session` (docs/PLAN.md §9.1). Persists the user's message,
    /// then loops: build the request from storage, stream it through the session's provider
    /// transport, execute any tool calls the model asked for, and repeat until the model stops,
    /// the loop is cancelled, or `ctx.max_steps` is exhausted.
    pub async fn run_turn(
        ctx: TurnContext<'_>,
        user_text: String,
    ) -> Result<TurnSummary, RuntimeError> {
        let TurnContext {
            deps,
            session,
            agent_id,
            context,
            permission_engine,
            ui_tx,
            pending_permissions,
            cancel,
            max_steps,
            headless_ask_policy,
        } = ctx;

        let provider = deps.providers.get(&session.provider).ok_or_else(|| {
            RuntimeError::InvalidRequest(format!("unknown provider {}", session.provider))
        })?;
        let transport = provider.transport(&session.transport).ok_or_else(|| {
            RuntimeError::InvalidRequest(format!(
                "provider {} has no transport {}",
                session.provider, session.transport
            ))
        })?;
        let cred = deps
            .auth
            .credential(&session.provider, &session.transport)
            .await?;

        let history = deps
            .storage
            .load_messages(session.id, xlightcli_storage::MessagePage::default())
            .await?;
        let mut turn = history.last().map(|m| m.turn + 1).unwrap_or(1);
        deps.storage
            .append_message(
                session.id,
                agent_id,
                turn,
                Role::User,
                vec![ContentBlock::Text { text: user_text }],
            )
            .await?;

        let _ = ui_tx
            .send(UiEvent::TurnStarted {
                session_id: session.id,
                agent_id,
                model: session.model.clone(),
            })
            .await;

        let mut total_usage = Usage::default();
        let mut response_text = String::new();
        // Deliberately uninitialized rather than defaulted to e.g. `Cancelled`: every `break
        // 'turn` below assigns this before breaking (the loop has no other exit), so rustc's
        // definite-assignment check proves it's always set by the time it's read after the loop —
        // a stale/default value can never leak out as the wrong `StopReason`.
        let final_stop: StopReason;
        let mut steps = 0u32;

        'turn: loop {
            if cancel.is_cancelled() {
                final_stop = StopReason::Cancelled;
                break 'turn;
            }
            if steps >= max_steps {
                final_stop = StopReason::Other("max_steps_exceeded".to_string());
                break 'turn;
            }
            steps += 1;

            let mut request = context
                .build_turn_request(&deps.storage, &deps.tools, session)
                .await?;
            if let Some(window) = transport.capabilities().context_window {
                let used = context.estimate_request_tokens(&request);
                if context.should_compact(used, u64::from(window)) {
                    request.messages = context.compact_messages(request.messages, session.id);
                    let _ = ui_tx
                        .send(UiEvent::Notice {
                            level: NoticeLevel::Info,
                            message: "Context compacted for this request (Wave B: a deterministic \
                                      placeholder, not persisted as a summary)."
                                .to_string(),
                        })
                        .await;
                }
            }

            let mut stream = match transport
                .stream(request, cred.clone(), cancel.clone())
                .await
            {
                Ok(stream) => stream,
                Err(err) => {
                    let _ = ui_tx
                        .send(UiEvent::TurnFailed {
                            agent_id,
                            error: err.to_string(),
                        })
                        .await;
                    let _ = deps
                        .storage
                        .update_session_status(session.id, SessionStatus::Failed)
                        .await;
                    return provider_error_outcome(err, response_text, total_usage);
                }
            };

            let mut completed: Option<(Message, StopReason, Usage)> = None;
            let mut stream_cancelled = false;
            'stream: loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        stream_cancelled = true;
                        break 'stream;
                    }
                    next = stream.next() => {
                        match next {
                            Some(Ok(AgentEvent::TurnStarted { .. })) => {}
                            Some(Ok(AgentEvent::TextDelta { index, text })) => {
                                let _ = ui_tx.send(UiEvent::TextDelta { agent_id, index, text }).await;
                            }
                            Some(Ok(AgentEvent::ReasoningDelta { index, text })) => {
                                let _ = ui_tx
                                    .send(UiEvent::ReasoningDelta { agent_id, index, text })
                                    .await;
                            }
                            Some(Ok(AgentEvent::ToolCallStarted { id, name, .. })) => {
                                let _ = ui_tx
                                    .send(UiEvent::ToolCallStarted { agent_id, call_id: id, name })
                                    .await;
                            }
                            Some(Ok(AgentEvent::Usage(usage))) => {
                                total_usage = add_usage(total_usage, usage);
                                let _ = ui_tx.send(UiEvent::Usage { agent_id, usage }).await;
                            }
                            Some(Ok(AgentEvent::RateLimit(info))) => {
                                let _ = ui_tx.send(UiEvent::RateLimit { agent_id, info }).await;
                            }
                            Some(Ok(AgentEvent::Completed { message, stop, usage })) => {
                                completed = Some((message, stop, usage));
                                break 'stream;
                            }
                            Some(Err(err)) => {
                                let _ = ui_tx
                                    .send(UiEvent::TurnFailed { agent_id, error: err.to_string() })
                                    .await;
                                let _ = deps
                                    .storage
                                    .update_session_status(session.id, SessionStatus::Failed)
                                    .await;
                                return provider_error_outcome(err, response_text, total_usage);
                            }
                            None => break 'stream,
                        }
                    }
                }
            }

            if stream_cancelled {
                final_stop = StopReason::Cancelled;
                break 'turn;
            }

            let Some((message, stop, usage)) = completed else {
                return Err(RuntimeError::Provider(ProviderError::ProtocolMismatch {
                    expected: xlightcli_protocol::ProtocolVersion(1),
                    detail: "transport stream ended without a Completed event".to_string(),
                }));
            };
            total_usage = add_usage(total_usage, usage);
            response_text.push_str(&message_text(&message));

            deps.storage
                .append_message(
                    session.id,
                    agent_id,
                    turn,
                    Role::Assistant,
                    message.content.clone(),
                )
                .await?;
            deps.storage
                .record_usage(NewUsageRow {
                    session_id: session.id,
                    agent_id,
                    transport: session.transport.clone(),
                    model: session.model.clone(),
                    usage,
                })
                .await?;

            match stop {
                StopReason::ToolUse => {
                    turn += 1;
                    let tool_uses: Vec<_> = message
                        .content
                        .into_iter()
                        .filter_map(|block| match block {
                            ContentBlock::ToolUse {
                                id, name, input, ..
                            } => Some((id, name, input)),
                            _ => None,
                        })
                        .collect();

                    let mut result_blocks = Vec::with_capacity(tool_uses.len());
                    for (call_id, name, input) in tool_uses {
                        let _ = ui_tx
                            .send(UiEvent::ToolCallStarted {
                                agent_id,
                                call_id: call_id.clone(),
                                name: name.clone(),
                            })
                            .await;
                        let stored_id = deps
                            .storage
                            .record_tool_call(NewToolCall {
                                agent_id,
                                call_id: call_id.clone(),
                                name: name.clone(),
                                input: input.clone(),
                            })
                            .await?;

                        let outcome = execute_tool(
                            deps,
                            session,
                            &permission_engine,
                            &ui_tx,
                            &pending_permissions,
                            cancel.clone(),
                            agent_id,
                            call_id.clone(),
                            &name,
                            input,
                            headless_ask_policy,
                        )
                        .await;

                        let (status, text, is_error) = match outcome {
                            Ok(output) => {
                                (ToolCallStatus::Succeeded, output.model_facing_text(), false)
                            }
                            Err(tool_err) => (ToolCallStatus::Failed, tool_err.to_string(), true),
                        };
                        let _ = deps.storage.finish_tool_call(stored_id, status, None).await;
                        let _ = ui_tx
                            .send(UiEvent::ToolCallFinished {
                                agent_id,
                                call_id: call_id.clone(),
                                summary: ToolCallSummary {
                                    name: name.clone(),
                                    text_preview: truncate_preview(&text, 500),
                                    artifact: None,
                                },
                            })
                            .await;
                        result_blocks.push(ContentBlock::ToolResult {
                            call_id,
                            content: vec![xlightcli_protocol::ToolResultPart::Text { text }],
                            is_error,
                        });
                    }

                    deps.storage
                        .append_message(session.id, agent_id, turn, Role::User, result_blocks)
                        .await?;
                    continue 'turn;
                }
                other => {
                    final_stop = other;
                    break 'turn;
                }
            }
        }

        let _ = ui_tx
            .send(UiEvent::TurnCompleted {
                agent_id,
                stop: final_stop.clone(),
            })
            .await;
        let session_status = if final_stop == StopReason::Cancelled {
            SessionStatus::Interrupted
        } else {
            SessionStatus::Active
        };
        let _ = deps
            .storage
            .update_session_status(session.id, session_status)
            .await;

        Ok(TurnSummary {
            response_text,
            usage: total_usage,
            stop: final_stop,
        })
    }
}

/// Builds a per-call `ToolContext` (workspace, permission gate, launcher, spool config) and runs
/// `name`'s `Tool::run`. Returns `Err` for an unknown tool name rather than panicking (INV-11).
#[allow(clippy::too_many_arguments)]
async fn execute_tool(
    deps: &RuntimeDeps,
    session: &Session,
    permission_engine: &PermissionEngine,
    ui_tx: &mpsc::Sender<UiEvent>,
    pending_permissions: &PendingPermissions,
    cancel: CancellationToken,
    agent_id: AgentId,
    call_id: ToolCallId,
    name: &str,
    input: serde_json::Value,
    headless_ask_policy: Option<AskPolicy>,
) -> Result<ToolOutput, xlightcli_tools::ToolError> {
    let Some(tool) = deps.tools.get(name) else {
        return Err(xlightcli_tools::ToolError::InvalidInput(format!(
            "unknown tool {name:?}"
        )));
    };

    let gate: Arc<dyn xlightcli_tools::PermissionGate> = Arc::new(RuntimePermissionGate::new(
        permission_engine.clone(),
        session.mode,
        agent_id,
        call_id.clone(),
        ui_tx.clone(),
        Arc::clone(pending_permissions),
        cancel.clone(),
        headless_ask_policy,
    ));
    let workspace_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let ctx = xlightcli_tools::ToolContext::new(
        xlightcli_tools::WorkspacePath::new(workspace_root),
        gate,
        Arc::new(xlightcli_tools::ProcessLauncher::new()),
        cancel,
        session.id,
        call_id,
        xlightcli_tools::ToolSpoolConfig {
            artifacts_dir: xlightcli_config::paths::artifacts_dir(),
            limits: xlightcli_tools::SpoolLimits::from_config(&deps.config.tools),
        },
    );
    tool.run(input, ctx).await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::time::Duration;

    use pretty_assertions::assert_eq;
    use xlightcli_storage::{MessagePage, SessionStatus, ToolCallStatus};
    use xlightcli_tools::{ExecutionMode, PermissionMode};

    use super::*;
    use crate::handle::PermissionResponse;
    use crate::test_support::{
        build_harness, completed, new_session, text_message, tool_registry_with_echo,
        tool_use_message,
    };

    #[tokio::test]
    async fn basic_turn_completes_and_persists_messages() {
        let scripts = vec![vec![
            Ok(AgentEvent::TurnStarted {
                model: xlightcli_protocol::ModelId::new("test-model"),
            }),
            Ok(AgentEvent::TextDelta {
                index: 0,
                text: "Hello".to_string(),
            }),
            completed(text_message("Hello, world!"), StopReason::EndTurn),
        ]];
        let harness = build_harness(
            scripts,
            Duration::ZERO,
            None,
            xlightcli_tools::ToolRegistry::new(),
            xlightcli_config::Config::default(),
        )
        .await;
        let session_id = new_session(&harness).await;

        harness
            .handle
            .submit_user_input(session_id, "hi".to_string())
            .await
            .unwrap();

        let record = harness
            .handle
            .deps()
            .storage
            .get_session(session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.status, SessionStatus::Active);

        let messages = harness
            .handle
            .deps()
            .storage
            .load_messages(session_id, MessagePage::default())
            .await
            .unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[1].role, Role::Assistant);
    }

    #[tokio::test]
    async fn multi_step_tool_loop_executes_the_tool_and_continues() {
        let mut config = xlightcli_config::Config::default();
        config.permissions.mode = PermissionMode::FullAuto;
        let scripts = vec![
            vec![completed(
                tool_use_message(
                    None,
                    "call-1",
                    "echo_tool",
                    serde_json::json!({"text": "hi"}),
                ),
                StopReason::ToolUse,
            )],
            vec![completed(text_message("done"), StopReason::EndTurn)],
        ];
        let harness = build_harness(
            scripts,
            Duration::ZERO,
            None,
            tool_registry_with_echo(),
            config,
        )
        .await;
        let session_id = new_session(&harness).await;
        let session = harness.handle.get_session(session_id).await.unwrap();
        let agent_id = harness.handle.ensure_agent(&session).await.unwrap();

        let summary = harness
            .handle
            .run_turn_for(&session, agent_id, "do it".to_string(), None)
            .await
            .unwrap();
        assert_eq!(summary.stop, StopReason::EndTurn);
        assert_eq!(summary.response_text, "done");

        let calls = harness
            .handle
            .deps()
            .storage
            .list_tool_calls(agent_id)
            .await
            .unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].status, ToolCallStatus::Succeeded);
        assert_eq!(calls[0].name, "echo_tool");
    }

    #[tokio::test]
    async fn permission_ask_allow_round_trip() {
        let config = xlightcli_config::Config::default(); // PermissionMode::Ask (default): WriteFile asks.
        let scripts = vec![
            vec![completed(
                tool_use_message(
                    None,
                    "call-1",
                    "echo_tool",
                    serde_json::json!({"text": "hi"}),
                ),
                StopReason::ToolUse,
            )],
            vec![completed(text_message("done"), StopReason::EndTurn)],
        ];
        let harness = build_harness(
            scripts,
            Duration::ZERO,
            None,
            tool_registry_with_echo(),
            config,
        )
        .await;
        let session_id = new_session(&harness).await;
        let mut ui = harness.handle.subscribe().await.unwrap();
        let session = harness.handle.get_session(session_id).await.unwrap();
        let agent_id = harness.handle.ensure_agent(&session).await.unwrap();

        let handle = harness.handle.clone();
        let session_clone = session.clone();
        let run = tokio::spawn(async move {
            handle
                .run_turn_for(&session_clone, agent_id, "do it".to_string(), None)
                .await
        });

        let tool_call_id = loop {
            match ui.recv().await.expect("ui channel closed unexpectedly") {
                UiEvent::PermissionRequested { request, .. } => {
                    break request
                        .tool_call_id
                        .expect("permission request needs a call id");
                }
                _ => continue,
            }
        };
        harness
            .handle
            .respond_to_permission(tool_call_id, PermissionResponse::Allow)
            .await
            .unwrap();

        let summary = run.await.unwrap().unwrap();
        assert_eq!(summary.stop, StopReason::EndTurn);

        let calls = harness
            .handle
            .deps()
            .storage
            .list_tool_calls(agent_id)
            .await
            .unwrap();
        assert_eq!(calls[0].status, ToolCallStatus::Succeeded);
    }

    #[tokio::test]
    async fn permission_ask_deny_round_trip() {
        let config = xlightcli_config::Config::default();
        let scripts = vec![
            vec![completed(
                tool_use_message(
                    None,
                    "call-1",
                    "echo_tool",
                    serde_json::json!({"text": "hi"}),
                ),
                StopReason::ToolUse,
            )],
            vec![completed(text_message("done"), StopReason::EndTurn)],
        ];
        let harness = build_harness(
            scripts,
            Duration::ZERO,
            None,
            tool_registry_with_echo(),
            config,
        )
        .await;
        let session_id = new_session(&harness).await;
        let mut ui = harness.handle.subscribe().await.unwrap();
        let session = harness.handle.get_session(session_id).await.unwrap();
        let agent_id = harness.handle.ensure_agent(&session).await.unwrap();

        let handle = harness.handle.clone();
        let session_clone = session.clone();
        let run = tokio::spawn(async move {
            handle
                .run_turn_for(&session_clone, agent_id, "do it".to_string(), None)
                .await
        });

        let tool_call_id = loop {
            match ui.recv().await.expect("ui channel closed unexpectedly") {
                UiEvent::PermissionRequested { request, .. } => {
                    break request
                        .tool_call_id
                        .expect("permission request needs a call id");
                }
                _ => continue,
            }
        };
        harness
            .handle
            .respond_to_permission(tool_call_id, PermissionResponse::Deny)
            .await
            .unwrap();

        // A denied tool doesn't fail the whole turn — the denial is reported back to the model as
        // an error tool result, and our scripted second call still ends the turn normally.
        let summary = run.await.unwrap().unwrap();
        assert_eq!(summary.stop, StopReason::EndTurn);

        let calls = harness
            .handle
            .deps()
            .storage
            .list_tool_calls(agent_id)
            .await
            .unwrap();
        assert_eq!(calls[0].status, ToolCallStatus::Failed);
    }

    #[tokio::test]
    async fn plan_mode_blocks_tool_writes_without_ever_asking() {
        let config = xlightcli_config::Config::default();
        let scripts = vec![
            vec![completed(
                tool_use_message(
                    None,
                    "call-1",
                    "echo_tool",
                    serde_json::json!({"text": "hi"}),
                ),
                StopReason::ToolUse,
            )],
            vec![completed(text_message("done"), StopReason::EndTurn)],
        ];
        let harness = build_harness(
            scripts,
            Duration::ZERO,
            None,
            tool_registry_with_echo(),
            config,
        )
        .await;
        let session_id = new_session(&harness).await;
        harness
            .handle
            .set_execution_mode(session_id, ExecutionMode::Plan)
            .await
            .unwrap();
        let mut ui = harness.handle.subscribe().await.unwrap();
        let session = harness.handle.get_session(session_id).await.unwrap();
        assert_eq!(session.mode, ExecutionMode::Plan);
        let agent_id = harness.handle.ensure_agent(&session).await.unwrap();

        let summary = harness
            .handle
            .run_turn_for(&session, agent_id, "do it".to_string(), None)
            .await
            .unwrap();
        assert_eq!(summary.stop, StopReason::EndTurn);

        while let Ok(event) = ui.try_recv() {
            assert!(
                !matches!(event, UiEvent::PermissionRequested { .. }),
                "plan mode must deny outright, never ask"
            );
        }

        let calls = harness
            .handle
            .deps()
            .storage
            .list_tool_calls(agent_id)
            .await
            .unwrap();
        assert_eq!(calls[0].status, ToolCallStatus::Failed);
    }

    #[tokio::test]
    async fn cancel_mid_stream_stops_the_turn() {
        let scripts = vec![vec![
            Ok(AgentEvent::TextDelta {
                index: 0,
                text: "a".to_string(),
            }),
            Ok(AgentEvent::TextDelta {
                index: 0,
                text: "b".to_string(),
            }),
            completed(text_message("never gets here"), StopReason::EndTurn),
        ]];
        let harness = build_harness(
            scripts,
            Duration::from_millis(200),
            None,
            xlightcli_tools::ToolRegistry::new(),
            xlightcli_config::Config::default(),
        )
        .await;
        let session_id = new_session(&harness).await;
        let session = harness.handle.get_session(session_id).await.unwrap();
        let agent_id = harness.handle.ensure_agent(&session).await.unwrap();

        let handle = harness.handle.clone();
        let session_clone = session.clone();
        let run = tokio::spawn(async move {
            handle
                .run_turn_for(&session_clone, agent_id, "hi".to_string(), None)
                .await
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        harness.handle.cancel_turn(session_id).await.unwrap();

        let summary = run.await.unwrap().unwrap();
        assert_eq!(summary.stop, StopReason::Cancelled);
    }

    #[tokio::test]
    async fn provider_error_before_any_output_is_a_hard_error() {
        let scripts = vec![vec![Err(ProviderError::Upstream {
            status: 500,
            body_excerpt: "boom".to_string(),
        })]];
        let harness = build_harness(
            scripts,
            Duration::ZERO,
            None,
            xlightcli_tools::ToolRegistry::new(),
            xlightcli_config::Config::default(),
        )
        .await;
        let session_id = new_session(&harness).await;
        let session = harness.handle.get_session(session_id).await.unwrap();
        let agent_id = harness.handle.ensure_agent(&session).await.unwrap();

        let err = harness
            .handle
            .run_turn_for(&session, agent_id, "hi".to_string(), None)
            .await
            .unwrap_err();
        assert!(matches!(err, RuntimeError::Provider(_)));

        let record = harness
            .handle
            .deps()
            .storage
            .get_session(session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.status, SessionStatus::Failed);
    }

    #[tokio::test]
    async fn provider_error_after_partial_output_reports_ok_with_the_partial_response() {
        let mut config = xlightcli_config::Config::default();
        config.permissions.mode = PermissionMode::FullAuto;
        let first_message = tool_use_message(
            Some("Let me check that."),
            "call-1",
            "echo_tool",
            serde_json::json!({"text": "hi"}),
        );
        let scripts = vec![
            vec![completed(first_message, StopReason::ToolUse)],
            vec![Err(ProviderError::Upstream {
                status: 500,
                body_excerpt: "boom".to_string(),
            })],
        ];
        let harness = build_harness(
            scripts,
            Duration::ZERO,
            None,
            tool_registry_with_echo(),
            config,
        )
        .await;
        let session_id = new_session(&harness).await;
        let session = harness.handle.get_session(session_id).await.unwrap();
        let agent_id = harness.handle.ensure_agent(&session).await.unwrap();

        let summary = harness
            .handle
            .run_turn_for(&session, agent_id, "hi".to_string(), None)
            .await
            .unwrap();
        assert_eq!(summary.response_text, "Let me check that.");
        assert!(matches!(summary.stop, StopReason::Other(_)));
    }

    #[tokio::test]
    async fn compaction_trigger_emits_a_notice_when_over_the_context_window() {
        let scripts = vec![vec![completed(text_message("ok"), StopReason::EndTurn)]];
        let harness = build_harness(
            scripts,
            Duration::ZERO,
            Some(10),
            xlightcli_tools::ToolRegistry::new(),
            xlightcli_config::Config::default(),
        )
        .await;
        let session_id = new_session(&harness).await;
        let session = harness.handle.get_session(session_id).await.unwrap();
        let agent_id = harness.handle.ensure_agent(&session).await.unwrap();

        for i in 0..20i64 {
            harness
                .handle
                .deps()
                .storage
                .append_message(
                    session_id,
                    agent_id,
                    i,
                    Role::User,
                    vec![ContentBlock::Text {
                        text: format!(
                            "padding message number {i} with extra words to inflate the token \
                             estimate well past a tiny context window"
                        ),
                    }],
                )
                .await
                .unwrap();
        }

        let mut ui = harness.handle.subscribe().await.unwrap();
        harness
            .handle
            .run_turn_for(&session, agent_id, "hi".to_string(), None)
            .await
            .unwrap();

        let mut saw_notice = false;
        while let Ok(event) = ui.try_recv() {
            if let UiEvent::Notice { message, .. } = event
                && message.contains("compacted")
            {
                saw_notice = true;
            }
        }
        assert!(saw_notice, "expected a context-compacted Notice event");
    }
}
