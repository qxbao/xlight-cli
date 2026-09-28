// SPDX-License-Identifier: GPL-3.0-only

//! `read_file`, `write_file`, `edit_file`, `list_dir`, `glob` (docs/PLAN.md §7.2).
//!
//! `read_file`/`write_file` go through `ctx.workspace.resolve` + `ctx.check_permission` before
//! touching disk (PATTERNS.md §7); a whole-file read larger than [`READ_INLINE_LIMIT`] streams
//! through `ctx.open_spool()` (INV-7) rather than being loaded into a `String`. `edit_file` is an
//! exact string replace (per the Wave A brief), not a diff/patch format — the before/after is
//! rendered as a unified diff via `similar` for the TUI diff view. `glob`/`grep` use the `ignore` +
//! `globset` crates so `.gitignore`/`.ignore` are respected without spawning `rg`/`find` (D-013).

use std::path::Path;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt};
use xlightcli_protocol::ToolDefinition;

use crate::permission::PermissionAction;
use crate::tool::{Tool, ToolContext, ToolEffect, ToolError, ToolOutput};

use super::definition_for;

/// Whole-file reads at or below this size are loaded into a `String` directly; above it, the file
/// is streamed through `ctx.open_spool()` instead (INV-7 — never buffer an unbounded read).
const READ_INLINE_LIMIT: u64 = 256 * 1024;

/// How many leading bytes are sniffed to decide whether a file looks binary (a `\0` byte anywhere
/// in the sample is treated as binary — the same heuristic `git`/`grep -I` use).
const BINARY_SNIFF_LEN: usize = 8192;

/// Cap on the number of entries `list_dir` returns, so a directory with millions of entries can't
/// make the tool call unbounded.
const LIST_DIR_MAX_ENTRIES: usize = 2000;

/// Cap on the number of paths `glob` returns.
const GLOB_MAX_RESULTS: usize = 5000;

async fn looks_binary(path: &Path) -> Result<bool, ToolError> {
    let mut file = tokio::fs::File::open(path).await.map_err(ToolError::Io)?;
    let mut buf = vec![0_u8; BINARY_SNIFF_LEN];
    let n = file.read(&mut buf).await.map_err(ToolError::Io)?;
    Ok(buf[..n].contains(&0))
}

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

    async fn run(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let args: ReadFileArgs = serde_json::from_value(input).map_err(ToolError::invalid_input)?;
        let path = ctx.workspace.resolve(&args.path)?;
        ctx.check_permission(PermissionAction::ReadFile(&path))
            .await?;

        let metadata = tokio::fs::metadata(&path).await.map_err(ToolError::Io)?;
        if metadata.is_dir() {
            return Err(ToolError::InvalidInput(format!(
                "{} is a directory, not a file",
                args.path
            )));
        }

        if args.start_line.is_some() || args.end_line.is_some() {
            let start = args.start_line.unwrap_or(1);
            if start == 0 {
                return Err(ToolError::InvalidInput(
                    "start_line is 1-based; 0 is not a valid line number".to_string(),
                ));
            }
            let end = args.end_line.unwrap_or(u32::MAX);
            if end < start {
                return Err(ToolError::InvalidInput(format!(
                    "end_line ({end}) is before start_line ({start})"
                )));
            }

            let file = tokio::fs::File::open(&path).await.map_err(ToolError::Io)?;
            let mut lines = tokio::io::BufReader::new(file).lines();
            let mut collected = Vec::new();
            let mut line_no: u32 = 0;
            while let Some(line) = lines.next_line().await.map_err(ToolError::Io)? {
                line_no += 1;
                if line_no < start {
                    continue;
                }
                if line_no > end {
                    break;
                }
                collected.push(line);
            }
            if collected.is_empty() && line_no < start {
                return Err(ToolError::InvalidInput(format!(
                    "start_line {start} is past the end of {} ({line_no} lines)",
                    args.path
                )));
            }
            return Ok(ToolOutput::Text(collected.join("\n")));
        }

        if looks_binary(&path).await? {
            return Err(ToolError::InvalidInput(format!(
                "{} appears to be a binary file; read_file only supports text",
                args.path
            )));
        }

        if metadata.len() > READ_INLINE_LIMIT {
            let mut spool = ctx.open_spool().await?;
            let mut file = tokio::fs::File::open(&path).await.map_err(ToolError::Io)?;
            let mut buf = [0_u8; 64 * 1024];
            loop {
                let n = file.read(&mut buf).await.map_err(ToolError::Io)?;
                if n == 0 {
                    break;
                }
                spool.write_chunk(&buf[..n]).await.map_err(ToolError::Io)?;
            }
            let summary = spool.finish().await.map_err(ToolError::Io)?;
            return Ok(ToolOutput::Spooled(summary));
        }

        let bytes = tokio::fs::read(&path).await.map_err(ToolError::Io)?;
        let text = String::from_utf8(bytes)
            .map_err(|_| ToolError::InvalidInput(format!("{} is not valid UTF-8", args.path)))?;
        Ok(ToolOutput::Text(text))
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

    async fn run(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let args: WriteFileArgs =
            serde_json::from_value(input).map_err(ToolError::invalid_input)?;
        let path = ctx.workspace.resolve(&args.path)?;
        ctx.check_permission(PermissionAction::WriteFile(&path))
            .await?;

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(ToolError::Io)?;
        }
        tokio::fs::write(&path, args.content.as_bytes())
            .await
            .map_err(ToolError::Io)?;
        Ok(ToolOutput::Text(format!(
            "wrote {} bytes to {}",
            args.content.len(),
            args.path
        )))
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

    async fn run(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let args: EditFileArgs = serde_json::from_value(input).map_err(ToolError::invalid_input)?;
        if args.old_string.is_empty() {
            return Err(ToolError::InvalidInput(
                "old_string must not be empty".to_string(),
            ));
        }
        let path = ctx.workspace.resolve(&args.path)?;
        ctx.check_permission(PermissionAction::WriteFile(&path))
            .await?;

        let original = tokio::fs::read_to_string(&path)
            .await
            .map_err(ToolError::Io)?;
        let occurrences = original.matches(args.old_string.as_str()).count();
        if occurrences == 0 {
            return Err(ToolError::InvalidInput(format!(
                "old_string not found in {}",
                args.path
            )));
        }
        if occurrences > 1 {
            return Err(ToolError::InvalidInput(format!(
                "old_string matches {occurrences} times in {} — it must be unique",
                args.path
            )));
        }

        let updated = original.replacen(args.old_string.as_str(), &args.new_string, 1);
        tokio::fs::write(&path, updated.as_bytes())
            .await
            .map_err(ToolError::Io)?;

        let diff = similar::TextDiff::from_lines(&original, &updated)
            .unified_diff()
            .context_radius(3)
            .header(&args.path, &args.path)
            .to_string();
        Ok(ToolOutput::Text(diff))
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

    async fn run(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let args: ListDirArgs = serde_json::from_value(input).map_err(ToolError::invalid_input)?;
        let path = ctx.workspace.resolve(&args.path)?;
        ctx.check_permission(PermissionAction::ReadFile(&path))
            .await?;

        let metadata = tokio::fs::metadata(&path).await.map_err(ToolError::Io)?;
        if !metadata.is_dir() {
            return Err(ToolError::InvalidInput(format!(
                "{} is not a directory",
                args.path
            )));
        }

        let mut read_dir = tokio::fs::read_dir(&path).await.map_err(ToolError::Io)?;
        let mut entries = Vec::new();
        let mut truncated = false;
        while let Some(entry) = read_dir.next_entry().await.map_err(ToolError::Io)? {
            if entries.len() >= LIST_DIR_MAX_ENTRIES {
                truncated = true;
                break;
            }
            let file_type = entry.file_type().await.map_err(ToolError::Io)?;
            let kind = if file_type.is_symlink() {
                "symlink"
            } else if file_type.is_dir() {
                "dir"
            } else {
                "file"
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            entries.push(serde_json::json!({ "name": name, "kind": kind }));
        }
        entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));

        Ok(ToolOutput::Structured(serde_json::json!({
            "path": args.path,
            "entries": entries,
            "truncated": truncated,
        })))
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

    async fn run(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let args: GlobArgs = serde_json::from_value(input).map_err(ToolError::invalid_input)?;
        ctx.check_permission(PermissionAction::ReadFile(ctx.workspace.root()))
            .await?;

        let root = ctx.workspace.root().to_path_buf();
        let pattern = args.pattern.clone();
        let (matches, truncated) = tokio::task::spawn_blocking(move || run_glob(&root, &pattern))
            .await
            .map_err(|e| ToolError::Io(std::io::Error::other(e)))??;

        Ok(ToolOutput::Structured(serde_json::json!({
            "pattern": args.pattern,
            "matches": matches,
            "truncated": truncated,
        })))
    }
}

/// Blocking implementation of `glob`'s file walk (runs inside `spawn_blocking`, PATTERNS.md §3:
/// never block inside async). Respects `.gitignore`/`.ignore` via `ignore::WalkBuilder`.
fn run_glob(root: &Path, pattern: &str) -> Result<(Vec<String>, bool), ToolError> {
    let matcher = globset::Glob::new(pattern)
        .map_err(|e| ToolError::InvalidInput(format!("invalid glob pattern: {e}")))?
        .compile_matcher();

    let mut results = Vec::new();
    let mut truncated = false;
    // `.gitignore` should be honored even when the workspace root isn't (yet) an actual git
    // repository — `require_git(false)` makes `ignore` apply `.gitignore`/`.ignore` files it finds
    // by walking, rather than only inside a real `.git` checkout.
    for entry in ignore::WalkBuilder::new(root).require_git(false).build() {
        let Ok(entry) = entry else { continue };
        if entry.file_type().is_none_or(|t| t.is_dir()) {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        if !matcher.is_match(rel) {
            continue;
        }
        if results.len() >= GLOB_MAX_RESULTS {
            truncated = true;
            break;
        }
        results.push(rel.to_string_lossy().into_owned());
    }
    results.sort();
    Ok((results, truncated))
}
