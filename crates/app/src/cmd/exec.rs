// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli exec` (docs/PLAN.md §9.3, §18.3; docs/commands.md §5; D-026).
//!
//! **Status (Phase 1 Wave B):** argument parsing/validation, runtime wiring, and the turn itself
//! (`xlightcli_runtime::run_exec`, now real) all work end to end. Exit-code mapping (D-026,
//! docs/commands.md §5) is this module's job, `run_exec` only reports success/failure of *driving*
//! the turn:
//! - An invalid invocation (bad `--mode`, bad `--resume` id, no prompt) never reaches the runtime:
//!   exit `2`.
//! - `RuntimeError::InvalidRequest` (a well-formed CLI invocation the runtime still can't act on —
//!   unknown provider/transport, no `--provider` and no `default_provider`, `--continue` with no
//!   session to continue) also maps to exit `2`.
//! - Every other `RuntimeError` (storage/tool/provider/auth failures, `NotImplemented`) maps to
//!   exit `1`.
//! - `Ok(ExecOutput)` with `status == "ok"` is exit `0`.
//! - `Ok(ExecOutput)` with a different `status` (`"cancelled"`, a `StopReason::Other(..)`, ...)
//!   *and* a non-empty `response` is exit `3` ("error after output was already produced",
//!   docs/commands.md §5) — the response is still printed first, same as `dev probe`'s `Partial`.
//!   The same status with an empty response has nothing to show for the failure, so it's exit `1`
//!   instead.
//!
//! `--output-format stream-json` additionally subscribes to `RuntimeHandle::subscribe` *before*
//! calling `run_exec`, so every `UiEvent` the turn produces is printed as its own JSON line
//! (`output::stream_json_event`) while the turn is still running, followed by the final
//! `{conversation_id, status, response, usage}` result line `run_exec` returns.

use xlightcli_protocol::{ModelId, ProviderId, SessionId, TransportId};
use xlightcli_runtime::{ExecOptions, ExecOutput, ExecOutputFormat, RuntimeError, RuntimeHandle};
use xlightcli_tools::ExecutionMode;

use crate::cli::ExecArgs;
use crate::cmd::error::CliError;
use crate::output;

fn parse_mode(raw: &str) -> Result<ExecutionMode, CliError> {
    match raw {
        "default" => Ok(ExecutionMode::Default),
        "accept-edits" | "auto-edit" => Ok(ExecutionMode::AcceptEdits),
        "plan" => Ok(ExecutionMode::Plan),
        other => Err(CliError::invalid_input(format!(
            "unknown --mode {other:?} (expected default|accept-edits|plan)"
        ))),
    }
}

fn parse_session_id(raw: &str) -> Result<SessionId, CliError> {
    uuid::Uuid::parse_str(raw)
        .map(SessionId::from_uuid)
        .map_err(|e| CliError::invalid_input(format!("invalid --resume session id {raw:?}: {e}")))
}

fn build_options(args: &ExecArgs) -> Result<ExecOptions, CliError> {
    let prompt = args
        .print
        .clone()
        .or_else(|| args.prompt.clone())
        .ok_or_else(|| {
            CliError::invalid_input("a prompt is required: pass it positionally or via -p/--print")
        })?;

    let mut options = ExecOptions::new(prompt);
    options.output_format = args.output_format.into();
    options.model = args.model.clone().map(ModelId::new);
    options.provider = args.provider.clone().map(ProviderId::new);
    options.transport = args.transport.clone().map(TransportId::new);
    options.mode = args.mode.as_deref().map(parse_mode).transpose()?;
    options.continue_session = args.continue_session;
    options.resume_session = args.resume.as_deref().map(parse_session_id).transpose()?;
    options.dangerously_skip_permissions = args.dangerously_skip_permissions;
    options.add_dir.clone_from(&args.add_dir);
    if let Some(secs) = args.print_timeout {
        options.print_timeout = std::time::Duration::from_secs(secs);
    }
    Ok(options)
}

pub async fn dispatch(args: &ExecArgs) -> Result<(), CliError> {
    let options = build_options(args)?;
    let format = ExecOutputFormat::from(args.output_format);

    let runtime = crate::wiring::build_runtime()
        .await
        .map_err(|e| CliError::other(format!("failed to initialize runtime: {e}")))?;

    let result = if format == ExecOutputFormat::StreamJson {
        run_streaming(&runtime.handle, options).await
    } else {
        xlightcli_runtime::run_exec(&runtime.handle, options).await
    };

    match result {
        Ok(output) => {
            if format == ExecOutputFormat::Text {
                output::exec_text(&output.response);
            } else {
                output::exec_json(&output)?;
            }
            exit_result_for(&output)
        }
        Err(RuntimeError::InvalidRequest(msg)) => Err(CliError::invalid_input(msg)),
        Err(err) => Err(CliError::other(err.to_string())),
    }
}

/// Maps a successfully-returned `ExecOutput` to `Ok(())` or the exit-code-3 `Partial`/exit-code-1
/// `Other` error the module doc describes, based on `status`/whether anything was printed.
fn exit_result_for(output: &ExecOutput) -> Result<(), CliError> {
    if output.status == "ok" {
        return Ok(());
    }
    let msg = format!(
        "turn ended with status {:?} (see output above)",
        output.status
    );
    if output.response.is_empty() {
        Err(CliError::other(msg))
    } else {
        Err(CliError::partial(msg))
    }
}

/// `--output-format stream-json`: subscribes before starting the turn so every `UiEvent` prints as
/// its own JSON line while the turn runs, concurrently with `run_exec` itself. `RuntimeHandle`
/// keeps its `UiEvent` sender alive for the whole process (CODEBASE.md §5), so the channel never
/// closes on its own — the `select!` loop instead stops as soon as `run_exec`'s future resolves,
/// then does one final non-blocking drain for anything already buffered at that instant.
async fn run_streaming(
    handle: &RuntimeHandle,
    options: ExecOptions,
) -> Result<ExecOutput, RuntimeError> {
    let mut ui_events = match handle.subscribe().await {
        Ok(rx) => rx,
        Err(err) => {
            // Already subscribed (shouldn't happen: `exec` is a fresh `RuntimeHandle` per
            // process) — fall back to a non-streaming run rather than failing the whole command
            // over a UI-only nicety.
            tracing::warn!(%err, "stream-json: could not subscribe to UiEvents, falling back to a single final line");
            return xlightcli_runtime::run_exec(handle, options).await;
        }
    };

    let exec_fut = xlightcli_runtime::run_exec(handle, options);
    tokio::pin!(exec_fut);

    let outcome = loop {
        tokio::select! {
            biased;
            Some(event) = ui_events.recv() => {
                if let Some(line) = output::stream_json_event(&event) {
                    output::exec_stream_json_line(&line);
                }
            }
            result = &mut exec_fut => {
                break result;
            }
        }
    };
    while let Ok(event) = ui_events.try_recv() {
        if let Some(line) = output::stream_json_event(&event) {
            output::exec_stream_json_line(&line);
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::cli::ExecOutputFormatArg;

    fn args(print: Option<&str>, prompt: Option<&str>) -> ExecArgs {
        ExecArgs {
            print: print.map(str::to_string),
            prompt: prompt.map(str::to_string),
            output_format: ExecOutputFormatArg::Text,
            model: None,
            provider: None,
            transport: None,
            mode: None,
            continue_session: false,
            resume: None,
            dangerously_skip_permissions: false,
            add_dir: Vec::new(),
            print_timeout: None,
        }
    }

    #[test]
    fn missing_prompt_is_invalid_input() {
        let err = build_options(&args(None, None)).unwrap_err();
        assert_eq!(err.exit_code_number(), 2);
    }

    #[test]
    fn print_flag_takes_precedence_shape_is_accepted() {
        let options = build_options(&args(Some("via -p"), None)).unwrap();
        assert_eq!(options.prompt, "via -p");
    }

    #[test]
    fn positional_prompt_is_accepted() {
        let options = build_options(&args(None, Some("via positional"))).unwrap();
        assert_eq!(options.prompt, "via positional");
    }

    #[test]
    fn unknown_mode_is_invalid_input() {
        let mut a = args(None, Some("hi"));
        a.mode = Some("not-a-mode".to_string());
        let err = build_options(&a).unwrap_err();
        assert_eq!(err.exit_code_number(), 2);
    }

    #[test]
    fn valid_mode_is_parsed() {
        let mut a = args(None, Some("hi"));
        a.mode = Some("plan".to_string());
        let options = build_options(&a).unwrap();
        assert_eq!(options.mode, Some(ExecutionMode::Plan));
    }

    #[test]
    fn invalid_resume_id_is_invalid_input() {
        let mut a = args(None, Some("hi"));
        a.resume = Some("not-a-uuid".to_string());
        let err = build_options(&a).unwrap_err();
        assert_eq!(err.exit_code_number(), 2);
    }

    #[test]
    fn exit_result_for_ok_status_returns_ok() {
        let output = xlightcli_runtime::ExecOutput {
            conversation_id: SessionId::new(),
            status: "ok".to_string(),
            response: "Done.".to_string(),
            usage: Default::default(),
        };
        assert!(exit_result_for(&output).is_ok());
    }

    #[test]
    fn exit_result_for_error_without_output_is_exit_1() {
        let output = xlightcli_runtime::ExecOutput {
            conversation_id: SessionId::new(),
            status: "error".to_string(),
            response: String::new(),
            usage: Default::default(),
        };
        let err = exit_result_for(&output).unwrap_err();
        assert_eq!(err.exit_code_number(), 1);
    }

    #[test]
    fn exit_result_for_error_with_partial_output_is_exit_3() {
        let output = xlightcli_runtime::ExecOutput {
            conversation_id: SessionId::new(),
            status: "other(\"provider_error_after_partial_output: upstream 500\")".to_string(),
            response: "Here is partial code...".to_string(),
            usage: Default::default(),
        };
        let err = exit_result_for(&output).unwrap_err();
        assert_eq!(err.exit_code_number(), 3);
    }
}
