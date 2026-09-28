// SPDX-License-Identifier: GPL-3.0-only

//! `StorageError` (PATTERNS.md §2).

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// The blocking task running the sqlite call panicked. Distinct from `Sqlite` so callers can
    /// tell "the database rejected this" apart from "the worker thread died".
    #[error("storage worker task panicked: {0}")]
    Join(#[from] tokio::task::JoinError),

    /// A stored column held a value that doesn't parse as the type we expect (e.g. `auth_kind`,
    /// `expiry`, `metadata_json`). Indicates on-disk corruption or a schema/version mismatch, not
    /// a normal "not found" case.
    #[error("invalid stored data: {0}")]
    InvalidStoredData(String),

    /// `set_default`/similar targeted an `(provider, transport, account_id)` that isn't in the
    /// index.
    #[error("no account for {provider}/{transport}/{account_id}")]
    AccountNotFound {
        provider: String,
        transport: String,
        account_id: String,
    },
}
