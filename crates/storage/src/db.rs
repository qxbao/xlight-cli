// SPDX-License-Identifier: GPL-3.0-only

//! Connection open + migration runner (PATTERNS.md §10).
//!
//! Runs entirely on a blocking thread (called from inside `tokio::task::spawn_blocking`); nothing
//! here is async. Migrations are append-only `NNNN_description.sql` files embedded at compile
//! time via `include_str!` and tracked in a `schema_migrations` table so re-opening an existing
//! database is a no-op.

use std::path::Path;

use rusqlite::Connection;

use crate::error::StorageError;

/// Ordered, append-only list of migrations. Add new entries at the end; never edit or remove a
/// migration that has already shipped (CODEBASE.md §7).
const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("migrations/0001_accounts.sql")),
    (2, include_str!("migrations/0002_sessions.sql")),
    (3, include_str!("migrations/0003_permission_grants.sql")),
];

/// Opens (creating if needed) the sqlite database at `path`, enables WAL, and applies any
/// migration that hasn't run yet.
pub(crate) fn open_and_migrate(path: &Path) -> Result<Connection, StorageError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        // Best-effort: if this fails, `Connection::open` below will surface a clearer sqlite
        // error (e.g. "unable to open database file").
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(path)?;
    // WAL lets readers and the (future, Phase 1) writer thread proceed concurrently
    // (docs/PLAN.md §11.2, PATTERNS.md §10).
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", true)?;
    run_migrations(&conn)?;
    Ok(conn)
}

/// Opens a second connection to an already-migrated file database, for the read-side of
/// `Storage` (PATTERNS.md §10 "the reader uses its own read-only connection"). Does **not**
/// re-run migrations (the writer connection already did) but still enables `foreign_keys` for
/// consistency.
pub(crate) fn open_reader(path: &Path) -> Result<Connection, StorageError> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(conn)
}

/// Opens an in-memory database with migrations applied — used by tests that don't need a file on
/// disk.
#[cfg(test)]
pub(crate) fn open_in_memory_and_migrate() -> Result<Connection, StorageError> {
    let conn = Connection::open_in_memory()?;
    conn.pragma_update(None, "foreign_keys", true)?;
    run_migrations(&conn)?;
    Ok(conn)
}

fn run_migrations(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version    INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL
        );",
    )?;
    for (version, sql) in MIGRATIONS {
        let already_applied: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = ?1)",
            [version],
            |row| row.get(0),
        )?;
        if already_applied {
            continue;
        }
        conn.execute_batch(sql)?;
        conn.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            rusqlite::params![version, crate::accounts::now_rfc3339()],
        )?;
    }
    Ok(())
}
