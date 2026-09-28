// SPDX-License-Identifier: GPL-3.0-only

//! `TuiError` (PATTERNS.md §2). `run` (`crate::run`) is deliberately anyhow-free — `app::main`
//! converts this into its own `anyhow`-based reporting.

#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error("terminal io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("runtime error: {0}")]
    Runtime(#[from] xlightcli_runtime::RuntimeError),

    /// Declared but not yet implemented (Wave B: real rendering, `crate::view::*`).
    #[error("{0} not implemented yet (Wave B)")]
    NotImplemented(&'static str),
}
