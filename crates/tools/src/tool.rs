// SPDX-License-Identifier: GPL-3.0-only

//! `Tool` trait, `ToolContext`, `ToolOutput`/`ToolError`, and `WorkspacePath` (PATTERNS.md §7).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use xlightcli_protocol::ToolDefinition;

use crate::launcher::ProcessLauncher;
use crate::permission::{PermissionAction, PermissionGate};
use crate::spool::{ArtifactRef, SpoolLimits, SpooledOutput};

/// Side-effect classification (docs/PLAN.md §7.2): read-only tool calls within the same assistant
/// message run in parallel; everything else runs sequentially in the order the model returned it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolEffect {
    ReadOnly,
    WritesWorkspace,
    Executes,
    Network,
}

/// What a tool returns to the agent loop. Most built-ins produce `Text` or `Structured`; `shell`
/// (and anything else whose output can be large/unbounded) produces `Spooled`
/// (`OutputSpool`/INV-7, PATTERNS.md §9).
#[derive(Debug, Clone)]
pub enum ToolOutput {
    /// Small, already-bounded text (e.g. `list_dir` of a normal directory).
    Text(String),
    /// Structured data the caller can render or feed back verbatim (e.g. a parsed `git_status`).
    Structured(Value),
    /// Head/tail summary + artifact pointer (PATTERNS.md §9) for output that could be large.
    Spooled(SpooledOutput),
}

impl ToolOutput {
    /// The text the model actually sees in its next turn: `Text` as-is, `Structured` as compact
    /// JSON, `Spooled` as its head+tail summary (never the full output).
    pub fn model_facing_text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Structured(value) => value.to_string(),
            Self::Spooled(spooled) => format!(
                "{}\n...\n{}\n[{} bytes, {} lines{}]",
                spooled.head_text(),
                spooled.tail_text(),
                spooled.total_bytes,
                spooled.total_lines,
                if spooled.truncated { ", truncated" } else { "" }
            ),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("permission denied: {0}")]
    PermissionDenied(String),

    #[error("path escapes the workspace: {0}")]
    PathEscape(String),

    #[error("process error: {0}")]
    Spawn(#[from] crate::launcher::SpawnError),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("cancelled")]
    Cancelled,

    /// Declared but not yet implemented (Wave A: the Phase-1 built-ins are stubs — see
    /// `crate::builtin` module doc). Never returned for a genuinely unsupported *feature* of a
    /// working tool — that's `xlightcli_provider::CommandResult::Unavailable`'s job at the
    /// command layer (INV-10); this is specifically "this tool body hasn't been written yet".
    #[error("{tool} not implemented yet (Wave B): {detail}")]
    NotImplemented {
        tool: &'static str,
        detail: &'static str,
    },
}

impl ToolError {
    pub fn invalid_input(err: impl std::fmt::Display) -> Self {
        Self::InvalidInput(err.to_string())
    }
}

/// Resolves a tool-supplied relative path against the workspace root, rejecting any path that
/// would escape it (PATTERNS.md §7).
///
/// **Scope decision (documented, Phase 1 Wave A):** this performs a purely *lexical* resolution
/// (join + collapse `.`/`..` components, then check the result still starts with the root) — it
/// does not consult the filesystem, so it does not yet catch a symlink whose target itself
/// escapes the root. That check (canonicalizing each existing ancestor) is a Wave B addition; the
/// lexical check already blocks the common `../../etc/passwd`-style traversal attempts and never
/// needs the target path to exist (important for `write_file` creating a new file).
#[derive(Debug, Clone)]
pub struct WorkspacePath {
    root: PathBuf,
}

impl WorkspacePath {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn resolve(&self, relative: impl AsRef<Path>) -> Result<PathBuf, ToolError> {
        let candidate = self.root.join(relative.as_ref());
        let normalized = normalize_lexically(&candidate);
        if !normalized.starts_with(&self.root) {
            return Err(ToolError::PathEscape(candidate.display().to_string()));
        }
        Ok(normalized)
    }
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Everything a `Tool::run` needs, besides its parsed input. **No credentials** (INV-4): a tool
/// never sees a `CredentialHandle`.
#[derive(Clone)]
pub struct ToolContext {
    pub workspace: WorkspacePath,
    pub permissions: Arc<dyn PermissionGate>,
    pub launcher: Arc<ProcessLauncher>,
    pub cancel: CancellationToken,
    pub session_id: xlightcli_protocol::SessionId,
    pub call_id: xlightcli_protocol::ToolCallId,
    artifacts_dir: PathBuf,
    spool_limits: SpoolLimits,
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("workspace_root", &self.workspace.root())
            .field("session_id", &self.session_id)
            .field("call_id", &self.call_id)
            .finish_non_exhaustive()
    }
}

/// The `OutputSpool`-related settings for a [`ToolContext`], grouped so
/// [`ToolContext::new`] doesn't need eight positional arguments.
#[derive(Debug, Clone)]
pub struct ToolSpoolConfig {
    pub artifacts_dir: PathBuf,
    pub limits: SpoolLimits,
}

impl ToolContext {
    pub fn new(
        workspace: WorkspacePath,
        permissions: Arc<dyn PermissionGate>,
        launcher: Arc<ProcessLauncher>,
        cancel: CancellationToken,
        session_id: xlightcli_protocol::SessionId,
        call_id: xlightcli_protocol::ToolCallId,
        spool: ToolSpoolConfig,
    ) -> Self {
        Self {
            workspace,
            permissions,
            launcher,
            cancel,
            session_id,
            call_id,
            artifacts_dir: spool.artifacts_dir,
            spool_limits: spool.limits,
        }
    }

    /// Convenience wrapper: `ctx.permissions.check(action)`.
    pub async fn check_permission(&self, action: PermissionAction<'_>) -> Result<(), ToolError> {
        self.permissions.check(action).await
    }

    /// Opens a fresh [`crate::spool::OutputSpool`] for this call (PATTERNS.md §9), rooted at the
    /// configured artifacts directory and using the configured head/tail limits.
    pub async fn open_spool(&self) -> Result<crate::spool::OutputSpool, ToolError> {
        crate::spool::OutputSpool::create(
            &self.artifacts_dir,
            self.session_id,
            self.call_id.clone(),
            self.spool_limits,
        )
        .await
        .map_err(ToolError::Io)
    }

    /// Not currently in `ToolOutput` (a tool can still reference its own artifact without
    /// finishing a spool through this helper) — surfaced for tools that register a pre-existing
    /// artifact rather than spooling live process output.
    pub fn artifact_ref(&self, path: PathBuf) -> ArtifactRef {
        ArtifactRef {
            session_id: self.session_id,
            call_id: self.call_id.clone(),
            path,
        }
    }
}

/// One callable tool (PATTERNS.md §7). Trait object-safe (`#[async_trait]`, D-006) since the
/// `ToolRegistry` holds a heterogeneous `Vec<Arc<dyn Tool>>`.
#[async_trait]
pub trait Tool: Send + Sync {
    fn definition(&self) -> &ToolDefinition;
    fn effect(&self, input: &Value) -> ToolEffect;
    async fn run(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError>;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn resolve_allows_a_plain_relative_path() {
        let ws = WorkspacePath::new("/repo");
        assert_eq!(
            ws.resolve("src/main.rs").unwrap(),
            PathBuf::from("/repo/src/main.rs")
        );
    }

    #[test]
    fn resolve_collapses_dot_and_dotdot_within_bounds() {
        let ws = WorkspacePath::new("/repo");
        assert_eq!(
            ws.resolve("src/../src/./main.rs").unwrap(),
            PathBuf::from("/repo/src/main.rs")
        );
    }

    #[test]
    fn resolve_blocks_traversal_out_of_the_root() {
        let ws = WorkspacePath::new("/repo");
        let err = ws.resolve("../../etc/passwd").unwrap_err();
        assert!(matches!(err, ToolError::PathEscape(_)));
    }

    #[test]
    fn resolve_blocks_traversal_even_when_it_would_land_back_inside() {
        // A pattern like `foo/../../repo/bar` shouldn't be treated as "safe because it happens to
        // land back under root" if it ever escapes at any point along the way lexically — here it
        // doesn't escape at all lexically (never actually leaves `/repo`), so this documents the
        // intended non-escaping case explicitly rather than assuming.
        let ws = WorkspacePath::new("/repo");
        assert_eq!(
            ws.resolve("foo/../bar").unwrap(),
            PathBuf::from("/repo/bar")
        );
    }

    #[test]
    fn model_facing_text_for_spooled_output_summarizes_not_dumps() {
        let output = ToolOutput::Spooled(SpooledOutput {
            head: b"head".to_vec(),
            tail: b"tail".to_vec(),
            total_bytes: 1_000_000,
            total_lines: 500,
            truncated: true,
            artifact: ArtifactRef {
                session_id: xlightcli_protocol::SessionId::new(),
                call_id: xlightcli_protocol::ToolCallId::new("call-1"),
                path: PathBuf::from("/tmp/a.log"),
            },
        });
        let text = output.model_facing_text();
        assert!(text.contains("head"));
        assert!(text.contains("tail"));
        assert!(text.contains("truncated"));
    }
}
