// SPDX-License-Identifier: GPL-3.0-only

//! `AgentLoop` (docs/PLAN.md §9.1): the single, provider-independent agent loop. No `match
//! provider_id` anywhere in this file (INV-2, enforced by `cargo xtask check-deps` refusing this
//! crate any `provider-*` dependency at all).
//!
//! **Status (Phase 1 Wave A): stub.** The real loop is:
//!
//! ```text
//! loop {
//!     req    = context_manager.build(session, agent_profile)
//!     permit = scheduler.llm_permit(transport, account).await
//!     stream = transport.stream(req, credential_handle, cancel)
//!     for event in stream { persist (batched) + forward UiEvent }
//!     match stop {
//!         ToolUse   => results = tool_executor.run(calls, permissions).await; append ToolResult; continue
//!         EndTurn   => break
//!         MaxTokens => continue-or-stop per policy
//!         Cancelled => break
//!     }
//!     budget.check()?
//! }
//! ```
//!
//! Wave B wires this up using `xlightcli_provider::TransportAdapter::stream`,
//! `crate::context::ContextManager`, and a `ToolExecutor` (not yet declared — the Wave A brief
//! scoped `tools`'s executor-side wiring separately; `xlightcli_tools::Tool`/`ToolRegistry`
//! already exist for it to call into).

use crate::error::RuntimeError;
use crate::handle::RuntimeDeps;
use crate::session::Session;

/// Namespace for the (currently stubbed) agent loop entry point.
#[derive(Debug)]
pub struct AgentLoop;

impl AgentLoop {
    /// Runs one full turn for `session` (docs/PLAN.md §9.1). Wave B.
    pub async fn run_turn(_deps: &RuntimeDeps, _session: &Session) -> Result<(), RuntimeError> {
        Err(RuntimeError::NotImplemented("AgentLoop::run_turn"))
    }
}
