// SPDX-License-Identifier: GPL-3.0-only

//! `ConfigLoader` — orchestrates the layer order from docs/PLAN.md §12.2:
//!
//! ```text
//! built-in defaults
//!   -> ~/.config/xlightcli/config.toml           (global)
//!   -> <repo>/.xlightcli/config.toml              (project)
//!   -> <repo>/.xlightcli/config.local.toml        (project.local)
//!   -> env XLIGHTCLI_*
//!   -> CLI flags / session override
//! ```
//!
//! `[experimental]` is stripped from the project/project.local layers unconditionally
//! (docs/PLAN.md §12.2); while the workspace isn't trusted, `crate::trust::filter_untrusted` also
//! strips the other sensitive keys from those two layers (docs/PLAN.md §12.3).

use std::path::{Path, PathBuf};

use crate::error::ConfigError;
use crate::merge::{self, resolve};
use crate::origin::{Origin, OriginMap, SECTION_NAMES};
use crate::partial::PartialConfig;
use crate::schema::Config;
use crate::trust::{TrustStore, filter_untrusted};

/// Result of a full layered load: the resolved config plus where each section came from.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub config: Config,
    pub origins: OriginMap,
}

/// Reads a TOML file if it exists; `Ok(None)` (not an error) if it's simply absent.
fn read_partial_file(path: &Path) -> Result<Option<PartialConfig>, ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ConfigError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let partial = PartialConfig::from_toml_str(&text).map_err(|source| ConfigError::Toml {
        path: path.to_path_buf(),
        source: Box::new(source),
    })?;
    Ok(Some(partial))
}

/// Records an origin for every top-level section the layer actually set (`Some` in its partial).
fn record_origins(accumulated: &mut OriginMap, layer: &PartialConfig, origin: Origin) {
    macro_rules! record_if_some {
        ($field:ident) => {
            if layer.$field.is_some() {
                accumulated.record(stringify!($field), origin);
            }
        };
    }
    record_if_some!(default_provider);
    record_if_some!(provider);
    record_if_some!(experimental);
    record_if_some!(disabled_transports);
    record_if_some!(permissions);
    record_if_some!(agents);
    record_if_some!(concurrency);
    record_if_some!(budget);
    record_if_some!(context);
    record_if_some!(tools);
    record_if_some!(auth);
    record_if_some!(rules);
    record_if_some!(ui);
    debug_assert_eq!(
        SECTION_NAMES.len(),
        13,
        "update record_origins if a section is added"
    );
}

/// Strips `experimental` unconditionally (docs/PLAN.md §12.2: only the global layer may set it).
fn strip_experimental(mut partial: PartialConfig) -> PartialConfig {
    partial.experimental = None;
    partial
}

/// Loads and merges every layer. `project_dir` is `<repo>/.xlightcli` (absent when not running
/// inside a workspace, e.g. `xlightcli auth login`). `env_vars` is injected (rather than read
/// directly from `std::env`) so tests don't need to mutate process-wide env state; production
/// callers pass `std::env::vars()`.
#[derive(Debug)]
pub struct ConfigLoader {
    global_path: PathBuf,
    project_dir: Option<PathBuf>,
}

impl ConfigLoader {
    /// `global_path` is typically `xlightcli_config::paths::global_config_file()`.
    pub fn new(global_path: impl Into<PathBuf>) -> Self {
        Self {
            global_path: global_path.into(),
            project_dir: None,
        }
    }

    /// `dir` is `<repo>/.xlightcli` (`xlightcli_config::paths::project_config_dir`).
    pub fn with_project_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.project_dir = Some(dir.into());
        self
    }

    /// Runs the full layer pipeline and resolves the final [`Config`].
    ///
    /// - `trust`/`repo_root`: workspace-trust gate for the project/project.local layers
    ///   (docs/PLAN.md §12.3). `repo_root` is only meaningful when `project_dir` was set.
    /// - `env_vars`: the minimal recognized `XLIGHTCLI_*` subset (see
    ///   [`crate::env::partial_from_env`]) — already collected by the caller.
    /// - `cli_overrides`: a [`PartialConfig`] built by the CLI layer from flags/session state.
    pub fn load(
        &self,
        trust: &TrustStore,
        repo_root: Option<&Path>,
        env_vars: impl Iterator<Item = (String, String)>,
        cli_overrides: PartialConfig,
    ) -> Result<LoadedConfig, ConfigError> {
        let mut accumulated = PartialConfig::empty();
        let mut origins = OriginMap::new();

        if let Some(global) = read_partial_file(&self.global_path)? {
            record_origins(&mut origins, &global, Origin::Global);
            accumulated = merge::merge(accumulated, global);
        }

        if let Some(project_dir) = &self.project_dir {
            let trusted = repo_root.is_some_and(|root| trust.is_trusted(root));

            if let Some(project) = read_partial_file(&project_dir.join("config.toml"))? {
                let project = strip_experimental(project);
                let project = if trusted {
                    project
                } else {
                    filter_untrusted(project)
                };
                record_origins(&mut origins, &project, Origin::Project);
                accumulated = merge::merge(accumulated, project);
            }

            if let Some(local) = read_partial_file(&project_dir.join("config.local.toml"))? {
                let local = strip_experimental(local);
                let local = if trusted {
                    local
                } else {
                    filter_untrusted(local)
                };
                record_origins(&mut origins, &local, Origin::ProjectLocal);
                accumulated = merge::merge(accumulated, local);
            }
        }

        let env_partial = crate::env::partial_from_env(env_vars);
        record_origins(&mut origins, &env_partial, Origin::Env);
        accumulated = merge::merge(accumulated, env_partial);

        record_origins(&mut origins, &cli_overrides, Origin::Cli);
        accumulated = merge::merge(accumulated, cli_overrides);

        Ok(LoadedConfig {
            config: resolve(accumulated),
            origins,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::schema::PermissionMode;

    fn write(path: &Path, text: &str) {
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn missing_files_resolve_to_hardcoded_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let trust = TrustStore::load(dir.path().join("trust.toml")).unwrap();
        let loader = ConfigLoader::new(dir.path().join("config.toml"));
        let loaded = loader
            .load(&trust, None, std::iter::empty(), PartialConfig::empty())
            .unwrap();
        assert_eq!(loaded.config.permissions.mode, PermissionMode::Ask);
        assert_eq!(loaded.origins.get("permissions"), Origin::Default);
    }

    #[test]
    fn global_layer_is_recorded_and_applied() {
        let dir = tempfile::tempdir().unwrap();
        let global_path = dir.path().join("config.toml");
        write(&global_path, "default_provider = \"codex\"\n");
        let trust = TrustStore::load(dir.path().join("trust.toml")).unwrap();
        let loader = ConfigLoader::new(&global_path);
        let loaded = loader
            .load(&trust, None, std::iter::empty(), PartialConfig::empty())
            .unwrap();
        assert_eq!(
            loaded.config.default_provider,
            Some(xlightcli_protocol::ProviderId::new("codex"))
        );
        assert_eq!(loaded.origins.get("default_provider"), Origin::Global);
    }

    #[test]
    fn untrusted_project_cannot_raise_permission_mode() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path().join(".xlightcli");
        std::fs::create_dir_all(&project_dir).unwrap();
        write(
            &project_dir.join("config.toml"),
            "[permissions]\nmode = \"full-auto\"\n",
        );
        let repo_root = dir.path().join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        let trust = TrustStore::load(dir.path().join("trust.toml")).unwrap();

        let loader =
            ConfigLoader::new(dir.path().join("config.toml")).with_project_dir(&project_dir);
        let loaded = loader
            .load(
                &trust,
                Some(&repo_root),
                std::iter::empty(),
                PartialConfig::empty(),
            )
            .unwrap();
        assert_eq!(loaded.config.permissions.mode, PermissionMode::Ask);
    }

    #[test]
    fn trusted_project_can_raise_permission_mode() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path().join(".xlightcli");
        std::fs::create_dir_all(&project_dir).unwrap();
        write(
            &project_dir.join("config.toml"),
            "[permissions]\nmode = \"full-auto\"\n",
        );
        let repo_root = dir.path().join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        let trust = TrustStore::load(dir.path().join("trust.toml")).unwrap();
        trust.trust(&repo_root).unwrap();

        let loader =
            ConfigLoader::new(dir.path().join("config.toml")).with_project_dir(&project_dir);
        let loaded = loader
            .load(
                &trust,
                Some(&repo_root),
                std::iter::empty(),
                PartialConfig::empty(),
            )
            .unwrap();
        assert_eq!(loaded.config.permissions.mode, PermissionMode::FullAuto);
    }

    #[test]
    fn experimental_is_never_read_from_project_layer_even_when_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path().join(".xlightcli");
        std::fs::create_dir_all(&project_dir).unwrap();
        write(
            &project_dir.join("config.toml"),
            "[experimental]\nclaude_subscription = true\n",
        );
        let repo_root = dir.path().join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        let trust = TrustStore::load(dir.path().join("trust.toml")).unwrap();
        trust.trust(&repo_root).unwrap();

        let loader =
            ConfigLoader::new(dir.path().join("config.toml")).with_project_dir(&project_dir);
        let loaded = loader
            .load(
                &trust,
                Some(&repo_root),
                std::iter::empty(),
                PartialConfig::empty(),
            )
            .unwrap();
        assert!(!loaded.config.experimental.claude_subscription);
    }

    #[test]
    fn cli_overrides_win_over_every_other_layer() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("config.toml"),
            "default_provider = \"codex\"\n",
        );
        let trust = TrustStore::load(dir.path().join("trust.toml")).unwrap();
        let loader = ConfigLoader::new(dir.path().join("config.toml"));
        let cli = PartialConfig {
            default_provider: Some(xlightcli_protocol::ProviderId::new("claude")),
            ..Default::default()
        };
        let loaded = loader.load(&trust, None, std::iter::empty(), cli).unwrap();
        assert_eq!(
            loaded.config.default_provider,
            Some(xlightcli_protocol::ProviderId::new("claude"))
        );
        assert_eq!(loaded.origins.get("default_provider"), Origin::Cli);
    }
}
