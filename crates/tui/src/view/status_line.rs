// SPDX-License-Identifier: GPL-3.0-only

//! Status line (docs/PLAN.md §18.2 example: `~/repo · Codex/chatgpt · gpt-… · 3 agents (2
//! running) · ctx 41% · ask`). Also the target of `core.statusline` (a user script, Phase 6).

use xlightcli_protocol::{ModelId, ProviderId, TransportId};
use xlightcli_runtime::PermissionMode;

/// The data a status line needs; formatting into a single-line string is Wave B (and, per
/// `core.statusline`, may eventually be delegated to a user script instead of built-in formatting).
#[derive(Debug, Clone)]
pub struct StatusLineView {
    pub workspace_label: String,
    pub provider: ProviderId,
    pub transport: TransportId,
    pub model: ModelId,
    pub agents_total: u32,
    pub agents_running: u32,
    pub context_used_percent: u8,
    pub permission_mode: PermissionMode,
}
