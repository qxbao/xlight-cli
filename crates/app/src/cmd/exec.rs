// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli exec` (docs/PLAN.md §9.3, §18.3; docs/commands.md §5; D-026).
//!
//! **Status (Phase 1 Wave A):** argument parsing/validation and runtime wiring are real; the turn
//! itself (`xlightcli_runtime::run_exec`) is a Wave B stub, so every *valid* invocation reaches
//! the runtime and then fails with a clear "not implemented yet" error (exit code 1, never a
//! crash, never a fake result — INV-10). An *invalid* invocation (e.g. no prompt) is rejected
//! before ever touching the runtime, with exit code 2.

use xlightcli_protocol::{ModelId, ProviderId, SessionId, TransportId};
use xlightcli_runtime::{ExecOptions, ExecOutputFormat};
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

    let runtime = crate::wiring::build_runtime()
        .await
        .map_err(|e| CliError::other(format!("failed to open storage: {e}")))?;

    match xlightcli_runtime::run_exec(&runtime.handle, options).await {
        Ok(result) => {
            match ExecOutputFormat::from(args.output_format) {
                ExecOutputFormat::Text => output::exec_text(&result.response),
                ExecOutputFormat::Json | ExecOutputFormat::StreamJson => {
                    output::exec_json(&result)?
                }
            }
            Ok(())
        }
        Err(err) => Err(CliError::other(err.to_string())),
    }
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
}
