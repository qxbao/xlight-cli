// SPDX-License-Identifier: GPL-3.0-only

//! Permission dialog (docs/PLAN.md §7.3): shown on `UiEvent::PermissionRequested`, answered via
//! `RuntimeHandle::respond_to_permission`. Rendering is Wave B.

use xlightcli_protocol::ToolCallId;
use xlightcli_runtime::PermissionRequest;

#[derive(Debug, Clone)]
pub struct PermissionDialogView {
    pub tool_call_id: ToolCallId,
    pub request: PermissionRequest,
}
