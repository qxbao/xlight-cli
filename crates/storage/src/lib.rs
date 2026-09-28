// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli-storage` — SQLite WAL persistence worker (CODEBASE.md §2, docs/PLAN.md §11.2).
//!
//! **Status (Phase 1 Wave A — contracts):** `accounts` (Wave 2, unchanged) plus the full
//! event-sourced schema from `migrations/0002_sessions.sql`: `workspaces`, `sessions`, `agents`,
//! `events`, `messages`, `tool_calls`, `artifacts`, `summaries` (schema only — no API yet, see
//! `crate::worker` doc), `provider_usage`. [`Storage`] is the real writer-thread-backed handle
//! (PATTERNS.md §10): a dedicated OS thread owns the one write connection and receives
//! [`crate::worker::StorageCmd`]s over a bounded `tokio::sync::mpsc` channel; bulk/paged reads use
//! a second, independent connection. `AccountIndex` keeps its own Wave-2 connection style
//! (`spawn_blocking` per call) — it predates `Storage` and there's no correctness reason to
//! migrate it onto the writer thread for a single small table.
//!
//! Canonical payloads: every JSON column stores a `xlightcli_protocol` type (never a provider's
//! wire JSON, PATTERNS.md §10, INV-3) — see `crate::records` module doc.

mod accounts;
mod db;
mod error;
mod records;
mod storage;
mod worker;

pub use accounts::{AccountIndex, AccountRecord};
pub use error::StorageError;
pub use records::{
    AgentRecord, AgentState, ArtifactRecord, EventRecord, MessagePage, MessageRecord, NewArtifact,
    NewEvent, NewToolCall, NewUsageRow, SessionRecord, SessionStatus, StoredEventKind,
    ToolCallRecord, ToolCallStatus, UsageRecord, WorkspaceRecord,
};
pub use storage::Storage;
