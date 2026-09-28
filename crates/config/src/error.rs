// SPDX-License-Identifier: GPL-3.0-only

//! `ConfigError` (PATTERNS.md §2): everything that can go wrong loading/merging/parsing config.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse TOML at {path}: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: Box<toml::de::Error>,
    },

    #[error("failed to serialize TOML for {path}: {source}")]
    TomlSerialize {
        path: PathBuf,
        #[source]
        source: Box<toml::ser::Error>,
    },

    /// A permission rule string didn't match the `action(target)` grammar (docs/PLAN.md §7.3).
    #[error("invalid permission rule {0:?}: expected `action(target)`")]
    InvalidRule(String),

    /// A duration string didn't match the minimal grammar this crate supports (docs/PLAN.md §10.3
    /// example `max_wall_clock = "30m"`): an integer followed by `s`/`m`/`h`, or a bare integer of
    /// seconds. **Not** a full `humantime`-style parser — deliberately minimal for Phase 1
    /// (documented scope decision, see `budget::BudgetDefaults::max_wall_clock_duration`).
    #[error(
        "invalid duration {0:?}: expected e.g. \"30s\", \"5m\", \"1h\", or a plain integer of seconds"
    )]
    InvalidDuration(String),

    #[error("trust store error: {0}")]
    Trust(String),
}
