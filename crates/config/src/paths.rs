// SPDX-License-Identifier: GPL-3.0-only

//! XDG-style paths (D-011): xlightcli uses `$XDG_CONFIG_HOME` / `$XDG_DATA_HOME` /
//! `$XDG_STATE_HOME` on **both** Linux and macOS (deliberately not the macOS convention of
//! `~/Library/Application Support`, so behavior is predictable across the two supported
//! platforms — D-003).
//!
//! Per the XDG Base Directory spec, an env var that is set but empty is treated as unset.
//!
//! The env/home lookups are split out as pure functions (`resolve_xdg_dir`) so tests don't need
//! to mutate process-wide env vars (which would require `unsafe` under edition 2024 — forbidden
//! by `[workspace.lints]`) or share mutable global state across parallel test threads.

use std::env;
use std::path::{Path, PathBuf};

const APP_NAME: &str = "xlightcli";

fn home_dir() -> PathBuf {
    // `directories::BaseDirs` gives us a correct, tested home-dir lookup on both platforms
    // without pulling in the macOS-specific "Application Support" convention we don't want.
    directories::BaseDirs::new()
        .map(|dirs| dirs.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Pure decision logic: env var wins when set and non-empty, otherwise `home/fallback_relative`.
fn resolve_xdg_dir(env_value: Option<&str>, home: &Path, fallback_relative: &str) -> PathBuf {
    match env_value {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home.join(fallback_relative),
    }
}

fn xdg_dir(env_key: &str, fallback_relative: &str) -> PathBuf {
    resolve_xdg_dir(
        env::var(env_key).ok().as_deref(),
        &home_dir(),
        fallback_relative,
    )
}

/// `$XDG_CONFIG_HOME/xlightcli` (default `~/.config/xlightcli`).
pub fn config_dir() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config").join(APP_NAME)
}

/// `$XDG_DATA_HOME/xlightcli` (default `~/.local/share/xlightcli`).
pub fn data_dir() -> PathBuf {
    xdg_dir("XDG_DATA_HOME", ".local/share").join(APP_NAME)
}

/// `$XDG_STATE_HOME/xlightcli` (default `~/.local/state/xlightcli`).
pub fn state_dir() -> PathBuf {
    xdg_dir("XDG_STATE_HOME", ".local/state").join(APP_NAME)
}

/// Global config file path (`config_dir()/config.toml`).
pub fn global_config_file() -> PathBuf {
    config_dir().join("config.toml")
}

/// SQLite database path (CODEBASE.md §6); `-wal`/`-shm` siblings are managed by rusqlite.
pub fn database_path() -> PathBuf {
    data_dir().join("xlightcli.db")
}

/// Root of the per-session tool-output artifact store.
pub fn artifacts_dir() -> PathBuf {
    data_dir().join("artifacts")
}

/// Root of agent git worktrees (D-012): kept outside any repo on purpose.
pub fn worktrees_dir() -> PathBuf {
    data_dir().join("worktrees")
}

/// Redacted, rotated log directory.
pub fn logs_dir() -> PathBuf {
    state_dir().join("logs")
}

/// Project config dir for a given repo root: `<repo>/.xlightcli`.
pub fn project_config_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(".xlightcli")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn env_var_wins_when_set_and_non_empty() {
        let home = Path::new("/home/u");
        let resolved = resolve_xdg_dir(Some("/custom/config"), home, ".config");
        assert_eq!(resolved, PathBuf::from("/custom/config"));
    }

    #[test]
    fn empty_env_var_falls_back_to_home_relative() {
        let home = Path::new("/home/u");
        let resolved = resolve_xdg_dir(Some(""), home, ".config");
        assert_eq!(resolved, PathBuf::from("/home/u/.config"));
    }

    #[test]
    fn unset_env_var_falls_back_to_home_relative() {
        let home = Path::new("/home/u");
        let resolved = resolve_xdg_dir(None, home, ".local/state");
        assert_eq!(resolved, PathBuf::from("/home/u/.local/state"));
    }

    #[test]
    fn derived_paths_are_rooted_under_data_or_state_dir() {
        // These only exercise real env/home resolution (no assumption about its value), to
        // catch accidental typos in the join() chains.
        assert!(database_path().starts_with(data_dir()));
        assert!(artifacts_dir().starts_with(data_dir()));
        assert!(worktrees_dir().starts_with(data_dir()));
        assert!(logs_dir().starts_with(state_dir()));
        assert!(global_config_file().starts_with(config_dir()));
    }

    #[test]
    fn project_config_dir_is_dot_xlightcli() {
        let repo = Path::new("/repo");
        assert_eq!(project_config_dir(repo), PathBuf::from("/repo/.xlightcli"));
    }
}
