// SPDX-License-Identifier: GPL-3.0-only

//! `shell` (docs/PLAN.md §7.2, §7.4, PATTERNS.md §8): spawns `bash -lc "<command>"` via
//! `ctx.launcher` (INV-1 — the *only* spawn point), never `std`/`tokio::process::Command`
//! directly. Output streams through `ctx.open_spool()` (INV-7); on timeout/cancel the whole
//! process group is killed by `SpawnedProcess::pipe_into`.

use std::time::Duration;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use xlightcli_protocol::ToolDefinition;

use crate::launcher::{EnvPolicy, SpawnPurpose, SpawnSpec};
use crate::permission::PermissionAction;
use crate::tool::{Tool, ToolContext, ToolEffect, ToolError, ToolOutput};

use super::definition_for;

/// Fallback default timeout when the caller doesn't set `timeout_secs`, mirroring
/// `xlightcli_config::ToolsConfig::shell_timeout_secs`'s own default (120s). `ToolContext` doesn't
/// carry a `ToolsConfig` reference (Wave A/B scope: it's built from discrete fields, not the whole
/// config), so this is a local constant rather than a value threaded through from config — see the
/// Wave B report for the gap. A caller that wants the *configured* value should pass it explicitly
/// via `timeout_secs`.
const DEFAULT_SHELL_TIMEOUT_SECS: u64 = 120;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShellArgs {
    /// Shell command line, run via `bash -lc`.
    pub command: String,
    /// Override the configured default timeout, in seconds.
    pub timeout_secs: Option<u64>,
}

#[derive(Debug)]
pub struct Shell {
    def: ToolDefinition,
}

impl Shell {
    pub fn new() -> Self {
        Self {
            def: definition_for::<ShellArgs>(
                "shell",
                "Run a shell command in the workspace (subject to permissions).",
            ),
        }
    }
}

impl Default for Shell {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for Shell {
    fn definition(&self) -> &ToolDefinition {
        &self.def
    }

    fn effect(&self, _input: &Value) -> ToolEffect {
        ToolEffect::Executes
    }

    async fn run(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let args: ShellArgs = serde_json::from_value(input).map_err(ToolError::invalid_input)?;
        ctx.check_permission(PermissionAction::Command(&args.command))
            .await?;

        let timeout = Duration::from_secs(args.timeout_secs.unwrap_or(DEFAULT_SHELL_TIMEOUT_SECS));
        let process = ctx
            .launcher
            .spawn(SpawnSpec {
                purpose: SpawnPurpose::ShellTool,
                program: "bash".to_string(),
                args: vec!["-lc".to_string(), args.command.clone()],
                cwd: ctx.workspace.root().to_path_buf(),
                env: EnvPolicy::scrubbed(),
                timeout: Some(timeout),
                cancel: ctx.cancel.clone(),
            })
            .await?;

        let mut spool = ctx.open_spool().await?;
        let status = process.pipe_into(&mut spool).await?;
        let note = match status.code() {
            Some(code) => format!("\n[exit code: {code}]"),
            None => "\n[terminated by signal]".to_string(),
        };
        spool
            .write_chunk(note.as_bytes())
            .await
            .map_err(ToolError::Io)?;
        let summary = spool.finish().await.map_err(ToolError::Io)?;
        Ok(ToolOutput::Spooled(summary))
    }
}
