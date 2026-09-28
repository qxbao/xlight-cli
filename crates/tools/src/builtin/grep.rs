// SPDX-License-Identifier: GPL-3.0-only

//! `grep` (docs/PLAN.md §7.2): `grep-searcher` + `grep-regex` + `grep-matcher` walked via
//! `ignore::WalkBuilder` so `.gitignore`/`.ignore` are respected without spawning `rg` (D-013).
//! Output goes through `ctx.open_spool()` — a repo-wide grep can easily exceed the head/tail
//! window (INV-7).

use std::path::Path;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use xlightcli_protocol::ToolDefinition;

use crate::permission::PermissionAction;
use crate::tool::{Tool, ToolContext, ToolEffect, ToolError, ToolOutput};

use super::definition_for;

/// Stop walking further files once this many matches have been found — bounds the work done for
/// a pattern that matches almost everything in a large repo (the spool artifact still captures
/// everything found up to the cap; INV-7's RAM bound applies independently on top of this).
const GREP_MAX_MATCHES: usize = 5000;

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

    async fn run(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let args: GrepArgs = serde_json::from_value(input).map_err(ToolError::invalid_input)?;
        ctx.check_permission(PermissionAction::ReadFile(ctx.workspace.root()))
            .await?;

        let matcher = grep_regex::RegexMatcherBuilder::new()
            .case_insensitive(!args.case_sensitive.unwrap_or(true))
            .build(&args.pattern)
            .map_err(|e| ToolError::InvalidInput(format!("invalid regex: {e}")))?;
        let path_matcher = match &args.path_glob {
            Some(glob) => Some(
                globset::Glob::new(glob)
                    .map_err(|e| ToolError::InvalidInput(format!("invalid path_glob: {e}")))?
                    .compile_matcher(),
            ),
            None => None,
        };

        let root = ctx.workspace.root().to_path_buf();
        let lines = tokio::task::spawn_blocking(move || run_grep(&root, &matcher, path_matcher))
            .await
            .map_err(|e| ToolError::Io(std::io::Error::other(e)))?
            .map_err(ToolError::Io)?;

        let mut spool = ctx.open_spool().await?;
        if lines.is_empty() {
            spool
                .write_chunk(b"no matches\n")
                .await
                .map_err(ToolError::Io)?;
        }
        for line in &lines {
            spool
                .write_chunk(line.as_bytes())
                .await
                .map_err(ToolError::Io)?;
            spool.write_chunk(b"\n").await.map_err(ToolError::Io)?;
        }
        let summary = spool.finish().await.map_err(ToolError::Io)?;
        Ok(ToolOutput::Spooled(summary))
    }
}

/// Blocking implementation of the walk + search (runs inside `spawn_blocking`, PATTERNS.md §3).
fn run_grep(
    root: &Path,
    matcher: &grep_regex::RegexMatcher,
    path_matcher: Option<globset::GlobMatcher>,
) -> Result<Vec<String>, std::io::Error> {
    let mut searcher = grep_searcher::SearcherBuilder::new()
        .line_number(true)
        .build();
    let mut out = Vec::new();
    let mut match_count = 0_usize;

    // See `builtin::fs::run_glob`'s comment: `.gitignore` should apply even outside a real `.git`
    // checkout.
    for entry in ignore::WalkBuilder::new(root).require_git(false).build() {
        if match_count >= GREP_MAX_MATCHES {
            break;
        }
        let Ok(entry) = entry else { continue };
        if entry.file_type().is_none_or(|t| !t.is_file()) {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        if let Some(pm) = &path_matcher
            && !pm.is_match(rel)
        {
            continue;
        }
        let rel_display = rel.to_string_lossy().into_owned();
        // A search error (e.g. the file is binary) is treated like `grep -I`: skip the file
        // rather than failing the whole tool call.
        let _ = searcher.search_path(
            matcher,
            entry.path(),
            grep_searcher::sinks::UTF8(|line_number, line| {
                out.push(format!(
                    "{}:{}:{}",
                    rel_display,
                    line_number,
                    line.trim_end_matches('\n')
                ));
                match_count += 1;
                Ok(match_count < GREP_MAX_MATCHES)
            }),
        );
    }
    Ok(out)
}
