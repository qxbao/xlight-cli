// SPDX-License-Identifier: GPL-3.0-only

//! The Phase-1 built-in tools (docs/PLAN.md §7.2): `read_file`, `write_file`, `edit_file`,
//! `list_dir`, `glob`, `grep`, `shell`, `git_status`, `git_diff`.
//!
//! **Status (Phase 1 Wave A): declared, not implemented.** Every struct here has a real
//! [`xlightcli_protocol::ToolDefinition`] (name/description/JSON schema generated from its args
//! struct via `schemars`, PATTERNS.md §7) and registers correctly in [`crate::registry::ToolRegistry`],
//! but [`crate::tool::Tool::run`] always returns [`crate::tool::ToolError::NotImplemented`] — Wave B
//! fills in the body of each. Grep for `NotImplemented` to find every stub.
//!
//! Each module still names the dependency it will need in Wave B in its own doc comment
//! (`ignore`/`grep-searcher`/`globset`/`similar`, all already in this crate's `Cargo.toml` per the
//! Wave A brief) so Wave B doesn't have to touch `Cargo.toml` at all.

mod fs;
mod git;
mod grep;
mod shell;

pub use fs::{EditFile, Glob, ListDir, ReadFile, WriteFile};
pub use git::{GitDiff, GitStatus};
pub use grep::Grep;
pub use shell::Shell;

use std::sync::Arc;

use xlightcli_protocol::ToolDefinition;

use crate::registry::ToolRegistry;

/// Builds a [`ToolDefinition`] whose `input_schema` is generated from `Args` (PATTERNS.md §7:
/// "the JSON schema for the model is generated from the same struct" the tool parses its input
/// with).
fn definition_for<Args: schemars::JsonSchema>(name: &str, description: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: description.to_string(),
        input_schema: schemars::schema_for!(Args).to_value(),
    }
}

/// Registers every built-in tool into `registry` (called by
/// [`crate::registry::ToolRegistry::with_builtins`]).
pub fn register_all(registry: &mut ToolRegistry) {
    registry.register(Arc::new(ReadFile::new()));
    registry.register(Arc::new(WriteFile::new()));
    registry.register(Arc::new(EditFile::new()));
    registry.register(Arc::new(ListDir::new()));
    registry.register(Arc::new(Glob::new()));
    registry.register(Arc::new(Grep::new()));
    registry.register(Arc::new(Shell::new()));
    registry.register(Arc::new(GitStatus::new()));
    registry.register(Arc::new(GitDiff::new()));
}
