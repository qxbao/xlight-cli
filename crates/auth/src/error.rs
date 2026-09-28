// SPDX-License-Identifier: GPL-3.0-only

//! `AuthError` (PATTERNS.md §2).

use xlightcli_protocol::ProviderId;

#[derive(Debug, Clone, thiserror::Error)]
pub enum AuthError {
    #[error("no credential for {provider}")]
    NotLoggedIn { provider: ProviderId },
    #[error("refresh rejected by upstream (re-login required)")]
    RefreshRejected,
    #[error("secret store unavailable: {0}")]
    StoreUnavailable(String),
    #[error("oauth flow failed: {0}")]
    OAuth(String),
    /// Marks a Wave 1 stub. Wave 2 replaces the call site with a real implementation without
    /// changing the surrounding public signature.
    #[error("not implemented yet: {0}")]
    NotImplemented(&'static str),
}
