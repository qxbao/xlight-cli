// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli-runtime` — provider-independent agent runtime: `Session`, `AgentLoop`,
//! `ContextManager`, `CommandRegistry`, `RuntimeHandle` (CODEBASE.md §2, docs/PLAN.md §9).
//!
//! **Invariant (INV-2, enforced by `cargo xtask check-deps`):** this crate must never depend on a
//! `provider-*` crate and must never `match` on a `ProviderId`. Provider differences are only ever
//! visible through `xlightcli_protocol::ProviderCapabilities` and `provider_options`.
//!
//! **Status (Phase 1 Wave A — contracts):** `RuntimeHandle`'s channel/session plumbing,
//! `CommandRegistry`, `ContextManager`'s token-estimate/compaction-trigger logic, and the
//! `ExecOptions`/`ExecOutput` headless contract are real. `AgentLoop::run_turn`,
//! `ContextManager::build_turn_request`, and `exec::run_exec` are Wave B stubs — every one returns
//! `RuntimeError::NotImplemented`, never a fake result (INV-10).

pub mod agent;
pub mod commands;
pub mod context;
pub mod error;
pub mod exec;
pub mod handle;
pub mod session;

#[cfg(feature = "testing")]
pub mod testing;

pub use agent::AgentLoop;
pub use commands::CommandRegistry;
pub use context::ContextManager;
pub use error::RuntimeError;
pub use exec::{ExecExitCode, ExecOptions, ExecOutput, ExecOutputFormat, ExecUsage, run_exec};
pub use handle::{
    CommandOutcome, NoticeLevel, PermissionResponse, RuntimeConfig, RuntimeDeps, RuntimeHandle,
    ToolCallSummary, UiEvent,
};
pub use session::Session;

// Re-exported so `tui` (which depends only on `protocol`/`config`/`runtime`, CODEBASE.md §3, and
// therefore cannot name a `xlightcli_tools` type directly) can still refer to the types `UiEvent`
// and `RuntimeHandle` expose in their public signatures.
pub use xlightcli_tools::{ArtifactRef, ExecutionMode, PermissionMode, PermissionRequest};
