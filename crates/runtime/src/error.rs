// SPDX-License-Identifier: GPL-3.0-only

//! `RuntimeError` (PATTERNS.md §2).

use xlightcli_protocol::SessionId;

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("unknown session {0}")]
    UnknownSession(SessionId),

    #[error("session {0} has no active turn to cancel")]
    NoActiveTurn(SessionId),

    #[error("unknown command {0:?}")]
    UnknownCommand(String),

    #[error("no permission request is pending for tool call {0}")]
    NoPendingPermission(xlightcli_protocol::ToolCallId),

    #[error("the UI event channel has already been subscribed to")]
    AlreadySubscribed,

    #[error("storage error: {0}")]
    Storage(#[from] xlightcli_storage::StorageError),

    #[error("tool error: {0}")]
    Tool(#[from] xlightcli_tools::ToolError),

    #[error("provider error: {0}")]
    Provider(#[from] xlightcli_protocol::ProviderError),

    /// [Wave B addition] surfaces `AuthBroker::credential`/`login`/`logout` failures (no
    /// credential stored yet, refresh rejected, secret store unavailable) through the same error
    /// type every other runtime failure travels as, so `AgentLoop`/`exec::run_exec`/the `/login`
    /// and `/logout` command bodies don't need a second error channel.
    #[error("auth error: {0}")]
    Auth(#[from] xlightcli_auth::AuthError),

    /// [Wave B addition] the request itself can't be carried out as asked — an unknown
    /// provider/transport id, `--continue`/`--resume` with no session to continue, a session
    /// whose provider has no matching transport, etc. Distinct from `Provider`/`Auth` (those are
    /// failures *talking to* something that was correctly identified) and from `NotImplemented`
    /// (that's "not built yet", this is "well-formed request, bad input" — INV-10 still applies:
    /// never silently substitute a default).
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// Declared but not yet implemented (Wave A: the agent loop / context assembly / exec runner
    /// are stubs — see the module doc of `crate::agent`/`crate::context`/`crate::exec`). Grep for
    /// `NotImplemented` to find every spot Wave B needs to fill in.
    #[error("{0} not implemented yet (Wave B)")]
    NotImplemented(&'static str),
}
