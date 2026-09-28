// SPDX-License-Identifier: GPL-3.0-only

//! Workspace trust (docs/PLAN.md §12.3): the first time a repo with `.xlightcli/` is opened, the
//! user is asked to trust it. While untrusted, sensitive project-layer keys are ignored.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;
use crate::partial::PartialConfig;
use crate::schema::PermissionMode;

/// Persisted trust decisions, keyed by canonicalized repo root (best-effort: falls back to the
/// as-given path if canonicalization fails, e.g. the repo was removed).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct TrustFile {
    #[serde(default)]
    trusted: BTreeSet<String>,
}

/// Tracks which workspace roots the user has explicitly trusted. Persisted as TOML (consistent
/// with the rest of `config`, avoiding a `serde_json` dependency for one small file) at
/// `paths::data_dir()/trust.toml` by convention — callers choose the path explicitly so tests
/// don't touch the real XDG data dir.
#[derive(Debug)]
pub struct TrustStore {
    path: PathBuf,
    trusted: std::sync::Mutex<BTreeSet<String>>,
}

impl TrustStore {
    /// Creates an empty in-memory trust set. Intended for isolated test runtimes that never
    /// persist trust decisions; production wiring should load the configured trust file.
    pub fn empty(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            trusted: std::sync::Mutex::new(BTreeSet::new()),
        }
    }

    /// Loads the trust file at `path`, or starts empty if it doesn't exist yet.
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, ConfigError> {
        let path = path.into();
        let trusted = match std::fs::read_to_string(&path) {
            Ok(text) => {
                toml::from_str::<TrustFile>(&text)
                    .map_err(|source| ConfigError::Toml {
                        path: path.clone(),
                        source: Box::new(source),
                    })?
                    .trusted
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
            Err(source) => return Err(ConfigError::Io { path, source }),
        };
        Ok(Self {
            path,
            trusted: std::sync::Mutex::new(trusted),
        })
    }

    fn key(repo_root: &Path) -> String {
        std::fs::canonicalize(repo_root)
            .unwrap_or_else(|_| repo_root.to_path_buf())
            .to_string_lossy()
            .into_owned()
    }

    /// `true` if `repo_root` has been explicitly trusted.
    pub fn is_trusted(&self, repo_root: &Path) -> bool {
        let key = Self::key(repo_root);
        // A poisoned mutex only means a previous access panicked; the in-memory set is still
        // structurally valid, so recovering it is safe (same rationale as
        // `storage::AccountIndex::with_conn`).
        self.trusted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(&key)
    }

    /// Marks `repo_root` as trusted and persists it.
    pub fn trust(&self, repo_root: &Path) -> Result<(), ConfigError> {
        let key = Self::key(repo_root);
        {
            let mut guard = self
                .trusted
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.insert(key);
        }
        self.persist()
    }

    /// Revokes trust for `repo_root`.
    pub fn revoke(&self, repo_root: &Path) -> Result<(), ConfigError> {
        let key = Self::key(repo_root);
        {
            let mut guard = self
                .trusted
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.remove(&key);
        }
        self.persist()
    }

    fn persist(&self) -> Result<(), ConfigError> {
        let snapshot = self
            .trusted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let file = TrustFile { trusted: snapshot };
        let text = toml::to_string_pretty(&file).map_err(|source| ConfigError::TomlSerialize {
            path: self.path.clone(),
            source: Box::new(source),
        })?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        std::fs::write(&self.path, text).map_err(|source| ConfigError::Io {
            path: self.path.clone(),
            source,
        })
    }
}

/// Keys that a project/project.local layer must never be able to set while the workspace is
/// untrusted (docs/PLAN.md §12.3): hooks, MCP stdio servers, exec allow rules, permission mode
/// above `ask`, enabling experimental transports, env passthrough.
///
/// `[experimental]` is additionally stripped from project layers **unconditionally** (regardless
/// of trust, docs/PLAN.md §12.2 "`[experimental]` is only read from the global layer") — callers
/// should do that separately (`ConfigLoader` does); this function only enforces the *trust* gate.
///
/// Hooks/MCP-stdio config keys aren't part of the Phase 1 [`crate::schema::Config`] schema yet
/// (they land with the `mcp`/`hooks` crates in later phases) — nothing to strip there today; this
/// function strips what Phase 1 *does* have: `env_passthrough` and any `permissions` override that
/// would raise the mode above `Ask` or add an `allow` rule for `command`/`unsandboxed`.
pub fn filter_untrusted(mut partial: PartialConfig) -> PartialConfig {
    if let Some(tools) = partial.tools.as_mut() {
        tools.env_passthrough = None;
    }
    if let Some(permissions) = partial.permissions.as_mut() {
        if let Some(mode) = permissions.mode
            && mode.rank() > PermissionMode::Ask.rank()
        {
            permissions.mode = None;
        }
        if let Some(allow) = permissions.allow.as_mut() {
            allow.retain(|raw| {
                let action = raw.split('(').next().unwrap_or_default().trim();
                action != "command" && action != "unsandboxed"
            });
        }
    }
    partial
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::partial::{PartialPermissionsConfig, PartialToolsConfig};

    #[test]
    fn trust_persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.toml");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();

        {
            let store = TrustStore::load(&path).unwrap();
            assert!(!store.is_trusted(&repo));
            store.trust(&repo).unwrap();
            assert!(store.is_trusted(&repo));
        }
        {
            let store = TrustStore::load(&path).unwrap();
            assert!(store.is_trusted(&repo));
        }
    }

    #[test]
    fn revoke_removes_trust() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.toml");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();

        let store = TrustStore::load(&path).unwrap();
        store.trust(&repo).unwrap();
        store.revoke(&repo).unwrap();
        assert!(!store.is_trusted(&repo));
    }

    #[test]
    fn filter_strips_env_passthrough() {
        let partial = PartialConfig {
            tools: Some(PartialToolsConfig {
                env_passthrough: Some(vec!["PATH".to_string()]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let filtered = filter_untrusted(partial);
        assert_eq!(filtered.tools.unwrap().env_passthrough, None);
    }

    #[test]
    fn filter_strips_mode_above_ask() {
        let partial = PartialConfig {
            permissions: Some(PartialPermissionsConfig {
                mode: Some(PermissionMode::FullAuto),
                ..Default::default()
            }),
            ..Default::default()
        };
        let filtered = filter_untrusted(partial);
        assert_eq!(filtered.permissions.unwrap().mode, None);
    }

    #[test]
    fn filter_keeps_mode_at_or_below_ask() {
        let partial = PartialConfig {
            permissions: Some(PartialPermissionsConfig {
                mode: Some(PermissionMode::ReadOnly),
                ..Default::default()
            }),
            ..Default::default()
        };
        let filtered = filter_untrusted(partial);
        assert_eq!(
            filtered.permissions.unwrap().mode,
            Some(PermissionMode::ReadOnly)
        );
    }

    #[test]
    fn filter_strips_command_allow_rules() {
        let partial = PartialConfig {
            permissions: Some(PartialPermissionsConfig {
                allow: Some(vec!["command(*)".to_string(), "read_file(*)".to_string()]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let filtered = filter_untrusted(partial);
        assert_eq!(
            filtered.permissions.unwrap().allow,
            Some(vec!["read_file(*)".to_string()])
        );
    }
}
