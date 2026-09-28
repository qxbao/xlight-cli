// SPDX-License-Identifier: GPL-3.0-only

//! `shell` (docs/PLAN.md §7.2, §7.4, PATTERNS.md §8). Wave B implementation notes: spawn via
//! `ctx.launcher.spawn(SpawnSpec { purpose: SpawnPurpose::ShellTool, program: "bash", args:
//! vec!["-lc", command], ... })` (PATTERNS.md §8 example), timeout defaults to
//! `xlightcli_config::ToolsConfig::shell_timeout_secs`, output via `ctx.open_spool()`.
//! `ctx.check_permission(PermissionAction::Command(&command))` **before** spawning.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use xlightcli_protocol::ToolDefinition;

use crate::tool::{Tool, ToolContext, ToolEffect, ToolError, ToolOutput};

use super::definition_for;

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

    async fn run(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        Err(ToolError::NotImplemented {
            tool: "shell",
            detail: "Phase 1 Wave B",
        })
    }
}
