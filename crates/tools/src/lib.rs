// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli-tools` — `Tool` trait, `ToolRegistry`, `PermissionEngine`, `ProcessLauncher`,
//! `OutputSpool` (CODEBASE.md §2, Phase 1).
//!
//! **Status: empty skeleton (Phase 0).** This crate is the *only* place allowed to spawn a
//! process (INV-1): `ProcessLauncher` will be the sole caller of `std::process::Command::new` /
//! `tokio::process::Command::new` in the whole workspace (see `clippy.toml`
//! `disallowed-methods`). It intentionally does not depend on `xlightcli-auth` (INV-4): no
//! credential ever reaches a tool or an MCP server.
