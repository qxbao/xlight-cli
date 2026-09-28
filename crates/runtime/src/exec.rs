// SPDX-License-Identifier: GPL-3.0-only

//! Headless `xlightcli exec` (docs/PLAN.md §9.3, §18.3; docs/commands.md §5; D-026).
//!
//! **Status (Phase 1 Wave A): stub.** [`ExecOptions`]/[`ExecOutput`]/[`ExecExitCode`] are the real,
//! serializable contract `app`'s `exec` subcommand and any external script depend on; [`run_exec`]
//! itself — actually driving a turn through [`RuntimeHandle`] and collecting the result — is Wave
//! B (it needs `crate::agent::AgentLoop::run_turn` to exist first).

use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;
use xlightcli_protocol::{ModelId, ProviderId, SessionId, TransportId};
use xlightcli_tools::ExecutionMode;

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

/// Runs one headless turn end to end (docs/PLAN.md §9.3). Wave B.
pub async fn run_exec(
    _handle: &RuntimeHandle,
    _options: ExecOptions,
) -> Result<ExecOutput, RuntimeError> {
    Err(RuntimeError::NotImplemented("exec::run_exec"))
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
}
