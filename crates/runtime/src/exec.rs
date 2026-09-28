// SPDX-License-Identifier: GPL-3.0-only

//! Headless `xlightcli exec` (docs/PLAN.md §9.3, §18.3; docs/commands.md §5; D-026).
//!
//! **Status (Phase 1 Wave B):** [`ExecOptions`]/[`ExecOutput`]/[`ExecExitCode`] are the same real,
//! serializable contract from Wave A. [`run_exec`] now drives a real turn through
//! [`RuntimeHandle::run_turn_for`] (`crate::agent::AgentLoop`) and reports the result.
//!
//! `--dangerously-skip-permissions` requires the persisted session workspace to be trusted and
//! the process-wide `XLIGHTCLI_ALLOW_DANGEROUS_SKIP_PERMISSIONS=1` opt-in before it changes the
//! headless `Ask` policy to `AutoAllow` (docs/commands.md §5).

use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;
use xlightcli_protocol::{ModelId, ProviderId, SessionId, StopReason, TransportId, WorkspaceId};
use xlightcli_tools::{AskPolicy, ExecutionMode};

use crate::error::RuntimeError;
use crate::handle::RuntimeHandle;

/// `--output-format` (docs/commands.md §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecOutputFormat {
    #[default]
    Text,
    Json,
    StreamJson,
}

/// Flags `xlightcli exec` accepts (D-026, docs/commands.md §5 — kept compatible with agy's/Claude
/// Code's print-mode flags so scripts migrate unmodified).
#[derive(Debug, Clone)]
pub struct ExecOptions {
    pub prompt: String,
    pub output_format: ExecOutputFormat,
    pub model: Option<ModelId>,
    pub provider: Option<ProviderId>,
    pub transport: Option<TransportId>,
    pub mode: Option<ExecutionMode>,
    /// `-c` / `--continue`: continue the most recent session in this workspace.
    pub continue_session: bool,
    /// `--resume <id>`.
    pub resume_session: Option<SessionId>,
    /// Requires workspace trust + a global opt-in (docs/commands.md §5); enforcing that is Wave B.
    pub dangerously_skip_permissions: bool,
    /// `--print-timeout` (default 5 minutes, docs/commands.md §5).
    pub print_timeout: Duration,
    /// `--add-dir`, repeatable.
    pub add_dir: Vec<PathBuf>,
}

impl ExecOptions {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            output_format: ExecOutputFormat::default(),
            model: None,
            provider: None,
            transport: None,
            mode: None,
            continue_session: false,
            resume_session: None,
            dangerously_skip_permissions: false,
            print_timeout: Duration::from_secs(5 * 60),
            add_dir: Vec::new(),
        }
    }
}

/// Token usage in the shape `docs/commands.md §5` documents for JSON output:
/// `{input, output, thinking, cache_read, total}_tokens`.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct ExecUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub thinking_tokens: u64,
    pub cache_read_tokens: u64,
    pub total_tokens: u64,
}

impl From<xlightcli_protocol::Usage> for ExecUsage {
    fn from(usage: xlightcli_protocol::Usage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            thinking_tokens: usage.reasoning_tokens,
            cache_read_tokens: usage.cached_input_tokens,
            total_tokens: usage.input_tokens + usage.output_tokens + usage.reasoning_tokens,
        }
    }
}

/// `xlightcli exec`'s JSON output shape (docs/commands.md §5:
/// `{conversation_id, status, response, usage}`).
#[derive(Debug, Clone, Serialize)]
pub struct ExecOutput {
    pub conversation_id: SessionId,
    pub status: String,
    pub response: String,
    pub usage: ExecUsage,
}

/// Process exit codes (D-026, docs/commands.md §5): `0` ok, `1` general error, `2` bad input, `3`
/// model/agent error after output was already produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecExitCode {
    Ok = 0,
    Error = 1,
    InvalidInput = 2,
    PartialError = 3,
}

impl ExecExitCode {
    pub fn code(self) -> i32 {
        self as i32
    }
}

/// Resolves which session `run_exec` should drive: `--resume <id>` (must already exist),
/// `--continue` (the most recently updated session in the current workspace), or a brand-new session for
/// `--provider`/`--transport`/`--model` (falling back to `Config`'s `[provider.<id>]` defaults,
/// then `default_provider`) rooted at the current working directory.
async fn resolve_session(
    handle: &RuntimeHandle,
    options: &ExecOptions,
) -> Result<SessionId, RuntimeError> {
    if let Some(id) = options.resume_session {
        handle.resume_session(id).await?;
        return Ok(id);
    }
    if options.continue_session {
        let current_root = std::env::current_dir().map_err(|err| {
            RuntimeError::InvalidRequest(format!("cannot locate current workspace: {err}"))
        })?;
        let current_root = std::fs::canonicalize(&current_root).unwrap_or(current_root);
        let mut latest = None;
        for record in handle.list_sessions().await? {
            let Some(workspace) = handle
                .deps()
                .storage
                .get_workspace(record.workspace_id)
                .await?
            else {
                continue;
            };
            if workspace.root == current_root
                && latest
                    .as_ref()
                    .is_none_or(|previous: &xlightcli_storage::SessionRecord| {
                        record.updated_at > previous.updated_at
                    })
            {
                latest = Some(record);
            }
        }
        let Some(record) = latest else {
            return Err(RuntimeError::InvalidRequest(
                "--continue was given but no session exists in the current workspace".to_string(),
            ));
        };
        handle.resume_session(record.id).await?;
        return Ok(record.id);
    }

    let deps = handle.deps();
    let provider_id = options
        .provider
        .clone()
        .or_else(|| deps.config.default_provider.clone())
        .ok_or_else(|| {
            RuntimeError::InvalidRequest(
                "no --provider given and no default_provider configured".to_string(),
            )
        })?;
    let provider = deps
        .providers
        .get(&provider_id)
        .ok_or_else(|| RuntimeError::InvalidRequest(format!("unknown provider {provider_id}")))?;
    let provider_defaults = deps.config.provider.get(provider_id.as_str());

    let transport_id = options
        .transport
        .clone()
        .or_else(|| provider_defaults.and_then(|d| d.transport.clone()))
        .ok_or_else(|| {
            RuntimeError::InvalidRequest(format!(
                "no --transport given and no default transport configured for provider {provider_id}"
            ))
        })?;
    if provider.transport(&transport_id).is_none() {
        return Err(RuntimeError::InvalidRequest(format!(
            "provider {provider_id} has no transport {transport_id}"
        )));
    }

    let model = options
        .model
        .clone()
        .or_else(|| provider_defaults.and_then(|d| d.default_model.clone()))
        .ok_or_else(|| {
            RuntimeError::InvalidRequest(format!(
                "no --model given and no default_model configured for provider {provider_id}"
            ))
        })?;

    let workspace_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let workspace_id: WorkspaceId = deps.storage.create_workspace(workspace_root, None).await?;
    handle
        .create_session(workspace_id, provider_id, transport_id, model, None)
        .await
}

/// Runs one headless turn end to end (docs/PLAN.md §9.3).
pub async fn run_exec(
    handle: &RuntimeHandle,
    options: ExecOptions,
) -> Result<ExecOutput, RuntimeError> {
    if options.dangerously_skip_permissions && !handle.deps().allow_dangerous_permissions {
        return Err(RuntimeError::InvalidRequest(
            "--dangerously-skip-permissions requires global opt-in: set XLIGHTCLI_ALLOW_DANGEROUS_SKIP_PERMISSIONS=1".to_string(),
        ));
    }
    let session_id = resolve_session(handle, &options).await?;
    if let Some(mode) = options.mode {
        handle.set_execution_mode(session_id, mode).await?;
    }
    let session = handle.get_session(session_id).await?;
    if options.dangerously_skip_permissions {
        let workspace = handle
            .deps()
            .storage
            .get_workspace(session.workspace_id)
            .await?
            .ok_or_else(|| {
                RuntimeError::InvalidRequest(format!(
                    "session {session_id} has no persisted workspace"
                ))
            })?;
        if !handle.deps().workspace_trust.is_trusted(&workspace.root) {
            return Err(RuntimeError::InvalidRequest(format!(
                "--dangerously-skip-permissions requires a trusted workspace: {}",
                workspace.root.display()
            )));
        }
    }
    let agent_id = handle.ensure_agent(&session).await?;

    let ask_policy = if options.dangerously_skip_permissions {
        AskPolicy::AutoAllow
    } else {
        AskPolicy::AutoDeny
    };

    let turn = handle.run_turn_for(&session, agent_id, options.prompt, Some(ask_policy));
    let summary = match tokio::time::timeout(options.print_timeout, turn).await {
        Ok(result) => result?,
        Err(_) => {
            return Err(RuntimeError::InvalidRequest(format!(
                "exec timed out after {:?} (--print-timeout)",
                options.print_timeout
            )));
        }
    };

    let status = match &summary.stop {
        StopReason::EndTurn => "ok".to_string(),
        StopReason::Cancelled => "cancelled".to_string(),
        other => format!("{other:?}").to_ascii_lowercase(),
    };

    Ok(ExecOutput {
        conversation_id: session_id,
        status,
        response: summary.response_text,
        usage: summary.usage.into(),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn exit_codes_match_d_026() {
        assert_eq!(ExecExitCode::Ok.code(), 0);
        assert_eq!(ExecExitCode::Error.code(), 1);
        assert_eq!(ExecExitCode::InvalidInput.code(), 2);
        assert_eq!(ExecExitCode::PartialError.code(), 3);
    }

    #[test]
    fn usage_conversion_sums_total_tokens() {
        let usage = xlightcli_protocol::Usage {
            input_tokens: 10,
            output_tokens: 5,
            cached_input_tokens: 2,
            reasoning_tokens: 3,
        };
        let exec_usage: ExecUsage = usage.into();
        assert_eq!(exec_usage.total_tokens, 18);
        assert_eq!(exec_usage.cache_read_tokens, 2);
    }

    #[test]
    fn default_output_format_is_text() {
        assert_eq!(ExecOptions::new("hi").output_format, ExecOutputFormat::Text);
    }

    /// `docs/commands.md §5` JSON output shape: `{conversation_id, status, response, usage{...
    /// _tokens}}`. `conversation_id` uses a fixed nil UUID so the snapshot is stable — a real
    /// `SessionId` is random per run.
    #[test]
    fn exec_output_json_shape_matches_docs_commands_md() {
        let output = ExecOutput {
            conversation_id: SessionId::from_uuid(uuid::Uuid::nil()),
            status: "ok".to_string(),
            response: "Hello, world!".to_string(),
            usage: ExecUsage {
                input_tokens: 100,
                output_tokens: 20,
                thinking_tokens: 5,
                cache_read_tokens: 10,
                total_tokens: 125,
            },
        };
        insta::assert_json_snapshot!(output);
    }

    async fn harness_with_prompt_response(
        scripts: Vec<
            Vec<Result<xlightcli_protocol::AgentEvent, xlightcli_protocol::ProviderError>>,
        >,
        tools: xlightcli_tools::ToolRegistry,
    ) -> crate::test_support::TestHarness {
        crate::test_support::build_harness(
            scripts,
            Duration::ZERO,
            None,
            tools,
            xlightcli_config::Config::default(),
        )
        .await
    }

    fn base_options() -> ExecOptions {
        let mut options = ExecOptions::new("hello");
        options.provider = Some(ProviderId::new(crate::test_support::TEST_PROVIDER));
        options.transport = Some(TransportId::new(crate::test_support::TEST_TRANSPORT));
        options.model = Some(ModelId::new(crate::test_support::TEST_MODEL));
        options
    }

    #[tokio::test]
    async fn run_exec_completes_a_new_session_end_to_end() {
        let scripts = vec![vec![crate::test_support::completed(
            crate::test_support::text_message("Hello, world!"),
            StopReason::EndTurn,
        )]];
        let harness =
            harness_with_prompt_response(scripts, xlightcli_tools::ToolRegistry::new()).await;

        let output = run_exec(&harness.handle, base_options()).await.unwrap();
        assert_eq!(output.status, "ok");
        assert_eq!(output.response, "Hello, world!");
    }

    #[tokio::test]
    async fn run_exec_errors_clearly_with_no_provider_configured() {
        let harness =
            harness_with_prompt_response(Vec::new(), xlightcli_tools::ToolRegistry::new()).await;
        let err = run_exec(&harness.handle, ExecOptions::new("hi"))
            .await
            .unwrap_err();
        assert!(matches!(err, RuntimeError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn run_exec_dangerously_skip_permissions_allows_an_otherwise_asked_write() {
        let scripts = vec![
            vec![crate::test_support::completed(
                crate::test_support::tool_use_message(
                    None,
                    "call-1",
                    "echo_tool",
                    serde_json::json!({"text": "hi"}),
                ),
                StopReason::ToolUse,
            )],
            vec![crate::test_support::completed(
                crate::test_support::text_message("done"),
                StopReason::EndTurn,
            )],
        ];
        let harness =
            harness_with_prompt_response(scripts, crate::test_support::tool_registry_with_echo())
                .await;
        let mut deps = harness.handle.deps().clone();
        deps.allow_dangerous_permissions = true;
        deps.workspace_trust
            .trust(&std::env::current_dir().unwrap())
            .unwrap();
        let handle =
            crate::handle::RuntimeHandle::new(deps, crate::handle::RuntimeConfig::default());

        let mut options = base_options();
        options.dangerously_skip_permissions = true;
        let output = run_exec(&handle, options).await.unwrap();
        assert_eq!(output.status, "ok");

        // Find the session `run_exec` created and confirm the tool actually ran and succeeded
        // (not silently skipped) — `--dangerously-skip-permissions` bypasses the `Ask` decision,
        // it doesn't fake a result (INV-10).
        let sessions = handle.list_sessions().await.unwrap();
        let session_id = sessions[0].id;
        let session = handle.get_session(session_id).await.unwrap();
        let agent_id = handle.ensure_agent(&session).await.unwrap();
        let calls = handle
            .deps()
            .storage
            .list_tool_calls(agent_id)
            .await
            .unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].status,
            xlightcli_storage::ToolCallStatus::Succeeded
        );
    }

    #[tokio::test]
    async fn dangerous_skip_requires_global_opt_in_and_workspace_trust() {
        let harness =
            harness_with_prompt_response(Vec::new(), xlightcli_tools::ToolRegistry::new()).await;
        let mut options = base_options();
        options.dangerously_skip_permissions = true;
        let err = run_exec(&harness.handle, options.clone())
            .await
            .unwrap_err();
        assert!(
            matches!(err, RuntimeError::InvalidRequest(message) if message.contains("global opt-in"))
        );

        let mut deps = harness.handle.deps().clone();
        deps.allow_dangerous_permissions = true;
        let handle =
            crate::handle::RuntimeHandle::new(deps, crate::handle::RuntimeConfig::default());
        let err = run_exec(&handle, options).await.unwrap_err();
        assert!(
            matches!(err, RuntimeError::InvalidRequest(message) if message.contains("trusted workspace"))
        );
    }

    #[tokio::test]
    async fn run_exec_without_the_skip_flag_denies_an_ask_decision() {
        let scripts = vec![
            vec![crate::test_support::completed(
                crate::test_support::tool_use_message(
                    None,
                    "call-1",
                    "echo_tool",
                    serde_json::json!({"text": "hi"}),
                ),
                StopReason::ToolUse,
            )],
            vec![crate::test_support::completed(
                crate::test_support::text_message("done"),
                StopReason::EndTurn,
            )],
        ];
        let harness =
            harness_with_prompt_response(scripts, crate::test_support::tool_registry_with_echo())
                .await;

        let output = run_exec(&harness.handle, base_options()).await.unwrap();
        assert_eq!(output.status, "ok");

        let sessions = harness.handle.list_sessions().await.unwrap();
        let session_id = sessions[0].id;
        let session = harness.handle.get_session(session_id).await.unwrap();
        let agent_id = harness.handle.ensure_agent(&session).await.unwrap();
        let calls = harness
            .handle
            .deps()
            .storage
            .list_tool_calls(agent_id)
            .await
            .unwrap();
        assert_eq!(calls[0].status, xlightcli_storage::ToolCallStatus::Failed);
    }

    #[tokio::test]
    async fn run_exec_continue_resumes_the_most_recently_updated_session() {
        let harness =
            harness_with_prompt_response(Vec::new(), xlightcli_tools::ToolRegistry::new()).await;
        let workspace = harness
            .handle
            .deps()
            .storage
            .create_workspace(std::env::current_dir().unwrap(), None)
            .await
            .unwrap();
        let create = || {
            harness.handle.create_session(
                workspace,
                ProviderId::new(crate::test_support::TEST_PROVIDER),
                TransportId::new(crate::test_support::TEST_TRANSPORT),
                ModelId::new(crate::test_support::TEST_MODEL),
                None,
            )
        };
        let first = create().await.unwrap();
        // Ensure a strictly later `created_at`/`updated_at` than `first` even on a fast
        // filesystem/clock.
        tokio::time::sleep(Duration::from_millis(5)).await;
        let second = create().await.unwrap();

        let mut options = ExecOptions::new("hi");
        options.continue_session = true;
        let resolved = resolve_session(&harness.handle, &options).await.unwrap();
        assert_eq!(resolved, second);
        assert_ne!(resolved, first);
    }
}
