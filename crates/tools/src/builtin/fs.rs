// SPDX-License-Identifier: GPL-3.0-only

//! `read_file`, `write_file`, `edit_file`, `list_dir`, `glob` (docs/PLAN.md §7.2).
//!
//! Wave B implementation notes: `read_file`/`write_file` go through `ctx.workspace.resolve` +
//! `ctx.check_permission` before touching disk (PATTERNS.md §7); large files stream through
//! `ctx.open_spool()` rather than `read_to_string`. `edit_file` is an exact string replace (per
//! the Wave A brief), not a diff/patch format — render the before/after with `similar` for the
//! permission-ask preview and `core.diff`. `glob` uses the `ignore` + `globset` crates (D-013:
//! never spawn `rg`/`find`).

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use xlightcli_protocol::ToolDefinition;

use crate::tool::{Tool, ToolContext, ToolEffect, ToolError, ToolOutput};

use super::definition_for;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadFileArgs {
    /// Path relative to the workspace root.
    pub path: String,
    /// Optional 1-based inclusive line range `[start, end]`, for reading part of a large file.
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
}

#[derive(Debug)]
pub struct ReadFile {
    def: ToolDefinition,
}

impl ReadFile {
    pub fn new() -> Self {
        Self {
            def: definition_for::<ReadFileArgs>(
                "read_file",
                "Read a file in the workspace, optionally by line range.",
            ),
        }
    }
}

impl Default for ReadFile {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ReadFile {
    fn definition(&self) -> &ToolDefinition {
        &self.def
    }

    fn effect(&self, _input: &Value) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn run(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        Err(ToolError::NotImplemented {
            tool: "read_file",
            detail: "Phase 1 Wave B",
        })
    }
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteFileArgs {
    pub path: String,
    pub content: String,
}

#[derive(Debug)]
pub struct WriteFile {
    def: ToolDefinition,
}

impl WriteFile {
    pub fn new() -> Self {
        Self {
            def: definition_for::<WriteFileArgs>(
                "write_file",
                "Create or overwrite a file in the workspace.",
            ),
        }
    }
}

impl Default for WriteFile {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WriteFile {
    fn definition(&self) -> &ToolDefinition {
        &self.def
    }

    fn effect(&self, _input: &Value) -> ToolEffect {
        ToolEffect::WritesWorkspace
    }

    async fn run(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        Err(ToolError::NotImplemented {
            tool: "write_file",
            detail: "Phase 1 Wave B",
        })
    }
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditFileArgs {
    pub path: String,
    /// The exact string to find (must be unique in the file — Wave B decides the error shape for
    /// "not found" vs. "found more than once").
    pub old_string: String,
    pub new_string: String,
}

#[derive(Debug)]
pub struct EditFile {
    def: ToolDefinition,
}

impl EditFile {
    pub fn new() -> Self {
        Self {
            def: definition_for::<EditFileArgs>(
                "edit_file",
                "Replace an exact, unique string occurrence in a workspace file.",
            ),
        }
    }
}

impl Default for EditFile {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for EditFile {
    fn definition(&self) -> &ToolDefinition {
        &self.def
    }

    fn effect(&self, _input: &Value) -> ToolEffect {
        ToolEffect::WritesWorkspace
    }

    async fn run(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        Err(ToolError::NotImplemented {
            tool: "edit_file",
            detail: "Phase 1 Wave B",
        })
    }
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListDirArgs {
    pub path: String,
}

#[derive(Debug)]
pub struct ListDir {
    def: ToolDefinition,
}

impl ListDir {
    pub fn new() -> Self {
        Self {
            def: definition_for::<ListDirArgs>(
                "list_dir",
                "List the entries of a workspace directory.",
            ),
        }
    }
}

impl Default for ListDir {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ListDir {
    fn definition(&self) -> &ToolDefinition {
        &self.def
    }

    fn effect(&self, _input: &Value) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn run(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        Err(ToolError::NotImplemented {
            tool: "list_dir",
            detail: "Phase 1 Wave B",
        })
    }
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GlobArgs {
    /// Glob pattern, e.g. `"**/*.rs"`.
    pub pattern: String,
}

#[derive(Debug)]
pub struct Glob {
    def: ToolDefinition,
}

impl Glob {
    pub fn new() -> Self {
        Self {
            def: definition_for::<GlobArgs>(
                "glob",
                "Find workspace files matching a glob pattern.",
            ),
        }
    }
}

impl Default for Glob {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for Glob {
    fn definition(&self) -> &ToolDefinition {
        &self.def
    }

    fn effect(&self, _input: &Value) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn run(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        Err(ToolError::NotImplemented {
            tool: "glob",
            detail: "Phase 1 Wave B",
        })
    }
}
