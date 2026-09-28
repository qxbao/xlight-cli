// SPDX-License-Identifier: GPL-3.0-only

//! Read-only discovery helpers shared by adapters' `AuthAdapter::discover_existing` (D-017).
//!
//! xlightcli only ever *reads* another CLI's credential/config files to offer "Reuse existing
//! login" — never writes to them, never spawns the official binary (INV-1). Provider-specific
//! paths and parsers live in `provider-<x>::auth`; this module only has the tiny bit of logic
//! that's genuinely shared.

use std::path::{Path, PathBuf};

/// A location a provider's official CLI might keep credentials/config.
#[derive(Debug, Clone)]
pub struct DiscoveryLocation {
    /// Human-readable description shown to the user (e.g. `"Codex CLI (~/.codex/auth.json)"`).
    pub description: String,
    pub path: PathBuf,
}

/// Filters `candidates` down to the ones that currently exist on disk. Adapters call this with
/// their provider-specific candidate list; existence is the only universal check done here.
pub fn existing(candidates: Vec<DiscoveryLocation>) -> Vec<DiscoveryLocation> {
    candidates
        .into_iter()
        .filter(|loc| loc.path.exists())
        .collect()
}

/// Joins `home` with `relative`, without following the shell (`~`) — callers pass an already
/// resolved home directory (e.g. from `xlightcli_config::paths`-style lookups).
pub fn under_home(home: &Path, relative: &str) -> PathBuf {
    home.join(relative)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn existing_filters_out_missing_paths() {
        let temp = tempfile::tempdir().unwrap();
        let present = temp.path().join("present.json");
        std::fs::write(&present, b"{}").unwrap();
        let missing = temp.path().join("missing.json");

        let found = existing(vec![
            DiscoveryLocation {
                description: "present".into(),
                path: present.clone(),
            },
            DiscoveryLocation {
                description: "missing".into(),
                path: missing,
            },
        ]);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, present);
    }

    #[test]
    fn under_home_joins_relative_path() {
        let home = Path::new("/home/u");
        assert_eq!(
            under_home(home, ".codex/auth.json"),
            PathBuf::from("/home/u/.codex/auth.json")
        );
    }
}
