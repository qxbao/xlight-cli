// SPDX-License-Identifier: GPL-3.0-only

//! `git_status`, `git_diff` (docs/PLAN.md §7.2, D-013). Wave B implementation notes: both spawn
//! the real `git` CLI via `ctx.launcher.spawn(SpawnSpec { purpose: SpawnPurpose::Git, program:
//! "git", .. })` (D-013: git is the one process-spawning exception — everything else uses a
//! library). `git_diff` renders with `similar` for the TUI's diff view / permission-ask preview
//! when the raw `git diff` output needs a friendlier side-by-side form.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use xlightcli_protocol::ToolDefinition;

use crate::tool::{Tool, ToolContext, ToolEffect, ToolError, ToolOutput};

use super::definition_for;

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

    async fn run(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        Err(ToolError::NotImplemented {
            tool: "git_status",
            detail: "Phase 1 Wave B",
        })
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
                "Show `git diff` for the workspace (including untracked).",
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

    async fn run(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        Err(ToolError::NotImplemented {
            tool: "git_diff",
            detail: "Phase 1 Wave B",
        })
    }
}
