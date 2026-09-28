// SPDX-License-Identifier: GPL-3.0-only

//! Command palette (`core.mcp`-adjacent autocomplete UX, docs/PLAN.md §18.2): only shows commands
//! applicable to the active provider/transport (docs/commands.md §1.7 resolution order). Matching
//! against `RuntimeHandle::commands()` + fuzzy filtering is Wave B.

#[derive(Debug, Clone, Default)]
pub struct CommandPaletteView {
    pub query: String,
    pub selected: usize,
}
