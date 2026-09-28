// SPDX-License-Identifier: GPL-3.0-only

//! `git_status`, `git_diff` (docs/PLAN.md §7.2, D-013). Both spawn the real `git` CLI via
//! `ctx.launcher.spawn(SpawnSpec { purpose: SpawnPurpose::Git, program: "git", .. })` (D-013:
//! `git` is the one process-spawning exception — everything else uses a library; INV-1 only
//! forbids `codex`/`claude`/`agy`/`antigravity`, never `git`).

use std::time::Duration;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use xlightcli_protocol::ToolDefinition;

use crate::launcher::{EnvPolicy, SpawnPurpose, SpawnSpec};
use crate::permission::PermissionAction;
use crate::spool::SpooledOutput;
use crate::tool::{Tool, ToolContext, ToolEffect, ToolError, ToolOutput};

use super::definition_for;

const GIT_TIMEOUT_SECS: u64 = 30;

/// Spawns `git <args>` in the workspace root via `ctx.launcher` and spools combined
/// stdout+stderr (INV-7 — `git diff` on a large changeset can be sizeable). Fails the tool call
/// with the captured output if `git` exits non-zero (e.g. "not a git repository").
async fn run_git(ctx: &ToolContext, args: &[&str]) -> Result<SpooledOutput, ToolError> {
    ctx.check_permission(PermissionAction::ReadFile(ctx.workspace.root()))
        .await?;

    let process = ctx
        .launcher
        .spawn(SpawnSpec {
            purpose: SpawnPurpose::Git,
            program: "git".to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd: ctx.workspace.root().to_path_buf(),
            env: EnvPolicy::scrubbed(),
            timeout: Some(Duration::from_secs(GIT_TIMEOUT_SECS)),
            cancel: ctx.cancel.clone(),
        })
        .await?;

    let mut spool = ctx.open_spool().await?;
    let status = process.pipe_into(&mut spool).await?;
    let summary = spool.finish().await.map_err(ToolError::Io)?;
    if !status.success() {
        return Err(ToolError::InvalidInput(format!(
            "git {} failed (exit {:?}): {}",
            args.join(" "),
            status.code(),
            summary.head_text().trim()
        )));
    }
    Ok(summary)
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GitStatusArgs {}

#[derive(Debug)]
pub struct GitStatus {
    def: ToolDefinition,
}

impl GitStatus {
    pub fn new() -> Self {
        Self {
            def: definition_for::<GitStatusArgs>(
                "git_status",
                "Show `git status` for the workspace.",
            ),
        }
    }
}

impl Default for GitStatus {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for GitStatus {
    fn definition(&self) -> &ToolDefinition {
        &self.def
    }

    fn effect(&self, _input: &Value) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn run(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let _args: GitStatusArgs =
            serde_json::from_value(input).map_err(ToolError::invalid_input)?;
        let summary = run_git(&ctx, &["status", "--porcelain=v1", "--branch"]).await?;
        Ok(ToolOutput::Spooled(summary))
    }
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GitDiffArgs {
    /// Restrict the diff to this path (default: the whole working tree).
    pub path: Option<String>,
    /// Include staged changes only.
    pub staged: Option<bool>,
}

#[derive(Debug)]
pub struct GitDiff {
    def: ToolDefinition,
}

impl GitDiff {
    pub fn new() -> Self {
        Self {
            def: definition_for::<GitDiffArgs>(
                "git_diff",
                "Show `git diff` for the workspace (tracked changes, staged or not; use \
                 git_status to see untracked files).",
            ),
        }
    }
}

impl Default for GitDiff {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for GitDiff {
    fn definition(&self) -> &ToolDefinition {
        &self.def
    }

    fn effect(&self, _input: &Value) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn run(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let args: GitDiffArgs = serde_json::from_value(input).map_err(ToolError::invalid_input)?;

        // Untracked files never show up in plain `git diff` (only `git status`); Wave B keeps
        // `git_diff` scoped to what `git diff` itself reports (tracked changes, staged or not) and
        // leaves untracked-file discovery to `git_status`, per the tool's own docs/PLAN.md scope.
        let mut git_args: Vec<String> = vec!["diff".to_string(), "--no-color".to_string()];
        if args.staged.unwrap_or(false) {
            git_args.push("--cached".to_string());
        }
        if let Some(path) = &args.path {
            let resolved = ctx.workspace.resolve(path)?;
            let relative = resolved
                .strip_prefix(ctx.workspace.root())
                .unwrap_or(&resolved)
                .to_string_lossy()
                .into_owned();
            git_args.push("--".to_string());
            git_args.push(relative);
        }
        let arg_refs: Vec<&str> = git_args.iter().map(String::as_str).collect();

        let summary = run_git(&ctx, &arg_refs).await?;
        Ok(ToolOutput::Spooled(summary))
    }
}
