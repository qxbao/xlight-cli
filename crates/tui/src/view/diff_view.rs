// SPDX-License-Identifier: GPL-3.0-only

//! Diff view (`core.diff`, docs/commands.md §2; plan/diff artifact review, D-025). Wave B renders
//! `similar`-computed hunks (already in `xlightcli-tools`' `Cargo.toml`) side-by-side or unified.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    pub path: String,
    pub unified_text: String,
}

#[derive(Debug, Clone, Default)]
pub struct DiffView {
    pub hunks: Vec<DiffHunk>,
    pub selected: usize,
}
