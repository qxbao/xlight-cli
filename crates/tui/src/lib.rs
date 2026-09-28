// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli-tui` — ratatui + crossterm terminal UI (CODEBASE.md §2, Phase 1).
//!
//! **Status: empty skeleton (Phase 0).** The TUI only ever talks to `runtime::RuntimeHandle`; it
//! must never depend on `storage`/`provider`/`provider-*` directly (CODEBASE.md §3).
//! `ratatui`/`crossterm` are added to `[workspace.dependencies]` when Phase 1 starts building the
//! real UI, to keep Wave 1's dependency surface limited to what Phase 0 needs.
