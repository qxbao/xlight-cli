// SPDX-License-Identifier: GPL-3.0-only

//! `ProviderError` — the single error type every transport stream yields (docs/PLAN.md §4.3).
//!
//! Errors travel as `Result::Err(ProviderError)` items in the event stream, never as an
//! `AgentEvent` variant. Mapping from wire status/headers/body happens exactly once, at the
//! transport boundary, via `provider::error::map_status` (PATTERNS.md §2).

use std::time::{Duration, SystemTime};

use crate::capability::ProtocolVersion;
use crate::event::RateLimitInfo;

/// Canonical error surfaced by a `TransportAdapter`. Never carries raw secrets or full
/// response bodies (INV-4): `body_excerpt` is redacted and length-limited by the caller.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ProviderError {
    #[error("authentication failed: {0}")]
    Auth(#[from] AuthFailure),

    #[error("rate limited")]
    RateLimited {
        retry_after: Option<Duration>,
        info: RateLimitInfo,
    },

    #[error("quota exhausted")]
    QuotaExhausted { resets_at: Option<SystemTime> },

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// Response shape didn't match the pinned `ProtocolVersion` (PATTERNS.md §5): never guess,
    /// surface this instead.
    #[error("protocol mismatch: expected {expected:?}: {detail}")]
    ProtocolMismatch {
        expected: ProtocolVersion,
        detail: String,
    },

    /// Experimental transport gate rejected the call (feature disabled or kill switch), see
    /// `provider::gate`.
    #[error("transport disabled: {reason}")]
    TransportDisabled { reason: String },

    #[error("network error: {0}")]
    Network(String),

    /// Non-2xx upstream response that doesn't map to a more specific variant.
    #[error("upstream error {status}: {body_excerpt}")]
    Upstream { status: u16, body_excerpt: String },

    #[error("cancelled")]
    Cancelled,
}

/// Sub-classification of authentication failures, used by `AuthBroker`/`CredentialHandle` to
/// decide whether to refresh, re-login, or give up.
#[derive(Debug, Clone, thiserror::Error)]
pub enum AuthFailure {
    #[error("no credential available for this provider/transport")]
    NoCredential,
    #[error("credential rejected by upstream (401/403)")]
    Rejected,
    #[error("refresh failed: {0}")]
    RefreshFailed(String),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn provider_error_display_does_not_include_placeholder_secrets() {
        let err = ProviderError::Upstream {
            status: 401,
            body_excerpt: "<redacted>".into(),
        };
        assert_eq!(err.to_string(), "upstream error 401: <redacted>");
    }

    #[test]
    fn auth_failure_converts_into_provider_error() {
        let err: ProviderError = AuthFailure::Rejected.into();
        assert!(matches!(err, ProviderError::Auth(AuthFailure::Rejected)));
    }
}
