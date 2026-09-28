// SPDX-License-Identifier: GPL-3.0-only

//! `grep` (docs/PLAN.md §7.2). Wave B implementation notes: `grep-searcher` + `grep-regex` +
//! `grep-matcher` (already in this crate's `Cargo.toml`) walked via `ignore::WalkBuilder` so
//! `.gitignore`/`.ignore` are respected without spawning `rg` (D-013). Output goes through
//! `ctx.open_spool()` — a repo-wide grep can easily exceed the head/tail window (INV-7).

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use xlightcli_protocol::ToolDefinition;

use crate::tool::{Tool, ToolContext, ToolEffect, ToolError, ToolOutput};

use super::definition_for;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GrepArgs {
    /// Regular expression to search for.
    pub pattern: String,
    /// Restrict the search to files matching this glob (default: the whole workspace).
    pub path_glob: Option<String>,
    pub case_sensitive: Option<bool>,
}

#[derive(Debug)]
pub struct Grep {
    def: ToolDefinition,
}

impl Grep {
    pub fn new() -> Self {
        Self {
            def: definition_for::<GrepArgs>(
                "grep",
                "Search workspace file contents with a regular expression.",
            ),
        }
    }
}

impl Default for Grep {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for Grep {
    fn definition(&self) -> &ToolDefinition {
        &self.def
    }

    fn effect(&self, _input: &Value) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn run(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        Err(ToolError::NotImplemented {
            tool: "grep",
            detail: "Phase 1 Wave B",
        })
    }
}
