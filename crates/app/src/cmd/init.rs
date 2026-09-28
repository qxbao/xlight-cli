// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli init` (docs/PLAN.md §18.1): provider selection, auth (reuse/browser/API key),
//! import prompts, writes `.xlightcli/config.toml` and/or `~/.config/xlightcli/config.toml`.
//!
//! **Status (Phase 1 Wave B): minimal real behavior**, not the full interactive wizard. The full
//! wizard (provider picker, auth method, import prompts) needs an interactive prompt UI that
//! doesn't exist yet (that's still a real gap, tracked below) — but doing *nothing* useful in the
//! meantime isn't better than a small honest step forward (INV-10: never fake a result, but also
//! don't let "not fully done" become "not started at all"). This writes a project-level
//! `.xlightcli/config.toml` skeleton (`xlightcli_config::paths::project_config_dir`) and,
//! optionally, an `AGENTS.md` stub — both non-destructively: an existing file is never overwritten
//! without an explicit `y`/`yes` confirmation (`crate::output::confirm`).
//!
//! **Known gap (Wave B):** no provider/auth/import wizard yet — `xlightcli auth login <provider>`
//! remains the way to authenticate until Phase 1's interactive prompt UI exists.

use std::path::{Path, PathBuf};

use crate::cmd::error::CliError;

const CONFIG_SKELETON: &str = r#"# xlightcli project configuration (docs/PLAN.md §12.4).
#
# Everything below is commented out: an empty file is already a valid config (every key falls
# back to the global config at ~/.config/xlightcli/config.toml, then to the built-in default).
# Uncomment and edit only the sections you want this project to override.

# default_provider = "codex"

# [permissions]
# mode = "ask"            # read-only | strict | ask | auto-edit | full-auto
# allow = []
# ask = []
# deny = []

# [tools]
# shell_timeout_secs = 120
"#;

const AGENTS_STUB: &str = r#"# AGENTS.md

Guidance for coding agents (Claude Code, Codex, xlightcli, ...) working in this repository.

- Describe how to build/test this project.
- Describe any conventions agents should follow.
"#;

/// Writes `path` with `contents`, asking for confirmation first if it already exists. Returns
/// whether the file was (re)written.
async fn write_with_confirmation(
    path: &Path,
    contents: &str,
    description: &str,
) -> Result<bool, CliError> {
    if tokio::fs::try_exists(path)
        .await
        .map_err(|e| CliError::other(format!("failed to check {}: {e}", path.display())))?
    {
        let prompt = format!(
            "{} already exists at {}; overwrite?",
            description,
            path.display()
        );
        if !crate::output::confirm(&prompt)? {
            crate::output::info(&format!("kept existing {}", path.display()));
            return Ok(false);
        }
    }
    tokio::fs::write(path, contents)
        .await
        .map_err(|e| CliError::other(format!("failed to write {}: {e}", path.display())))?;
    crate::output::info(&format!("wrote {}", path.display()));
    Ok(true)
}

/// The actual logic, parameterized over `repo_root` so tests don't need to mutate the process-wide
/// current directory (racy under parallel `cargo test`, PATTERNS.md §13 concerns) — `dispatch`
/// passes `std::env::current_dir()`.
async fn dispatch_in(repo_root: &Path) -> Result<(), CliError> {
    let project_dir = xlightcli_config::paths::project_config_dir(repo_root);
    tokio::fs::create_dir_all(&project_dir)
        .await
        .map_err(|e| CliError::other(format!("failed to create {}: {e}", project_dir.display())))?;

    write_with_confirmation(
        &project_dir.join("config.toml"),
        CONFIG_SKELETON,
        "project config",
    )
    .await?;

    let agents_path = repo_root.join("AGENTS.md");
    write_with_confirmation(&agents_path, AGENTS_STUB, "AGENTS.md").await?;

    crate::output::info(
        "note: the interactive provider/auth wizard isn't implemented yet; run \
         `xlightcli auth login <provider>` to authenticate in the meantime",
    );
    Ok(())
}

pub async fn dispatch() -> Result<(), CliError> {
    let repo_root: PathBuf = std::env::current_dir()
        .map_err(|e| CliError::other(format!("failed to read the current directory: {e}")))?;
    dispatch_in(&repo_root).await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// `crate::output::confirm` reads real stdin; a fresh tempdir never has anything to confirm
    /// overwriting, so this exercises the whole non-interactive "everything created fresh" path
    /// without needing to feed stdin.
    #[tokio::test]
    async fn fresh_directory_writes_project_config_without_prompting() {
        let dir = tempfile::tempdir().unwrap();
        dispatch_in(dir.path()).await.unwrap();

        let config_path =
            xlightcli_config::paths::project_config_dir(dir.path()).join("config.toml");
        assert!(config_path.exists());
        let contents = tokio::fs::read_to_string(&config_path).await.unwrap();
        assert!(contents.contains("xlightcli project configuration"));
    }

    #[tokio::test]
    async fn project_config_dir_is_dot_xlightcli_under_the_repo_root() {
        let dir = tempfile::tempdir().unwrap();
        dispatch_in(dir.path()).await.unwrap();
        assert!(dir.path().join(".xlightcli").join("config.toml").exists());
    }
}
