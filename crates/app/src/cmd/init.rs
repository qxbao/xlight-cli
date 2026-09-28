// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli init` (docs/PLAN.md §18.1): provider selection, auth (reuse/browser/API key),
//! import prompts, writes `.xlightcli/config.toml` and/or `~/.config/xlightcli/config.toml`.
//!
//! **Status (Phase 1 Wave A): stub.** The full interactive wizard needs `ConfigLoader`/`TrustStore`
//! (`xlightcli-config`, already real) wired to an interactive prompt UI that doesn't exist yet —
//! Wave B. Reports a clear, non-fake message (INV-10) instead of doing nothing silently.

use crate::cmd::error::CliError;

pub async fn dispatch() -> Result<(), CliError> {
    Err(CliError::other(
        "xlightcli init is not implemented yet (Phase 1 Wave B); run `xlightcli auth login <provider>` \
         to authenticate directly in the meantime",
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[tokio::test]
    async fn reports_a_clear_not_implemented_error() {
        let err = dispatch().await.unwrap_err();
        assert_eq!(err.exit_code_number(), 1);
    }
}
