// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli-storage` — SQLite WAL persistence worker (CODEBASE.md §2, docs/PLAN.md §11.2).
//!
//! **Status (Phase 0 / Wave 2):** only `accounts` (the `AccountIndex` needed by `auth`:
//! provider/transport/account_id/auth_kind/expiry/keyring_ref metadata — no secrets, see the
//! `accounts` table in docs/PLAN.md §11.2) is implemented. `AccountIndex` wraps a single
//! `rusqlite::Connection` behind a mutex, hopping onto a blocking thread per call via
//! `tokio::task::spawn_blocking` — it is *not* the persistence worker thread + bounded command
//! channel described in PATTERNS.md §10. That full writer-thread architecture, shared by the
//! `events`/`messages`/`artifacts`/… tables, lands in Phase 1 alongside `db`, `worker`, `events`,
//! `artifacts` and `query` modules.
//!
//! Only the writer thread ever holds a write connection (PATTERNS.md §10); everything else talks
//! to it through a bounded channel — true from Phase 1 onward. Today, `AccountIndex` is the sole
//! writer of the `accounts` table.

mod accounts;
mod db;
mod error;

pub use accounts::{AccountIndex, AccountRecord};
pub use error::StorageError;
