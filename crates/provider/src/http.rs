// SPDX-License-Identifier: GPL-3.0-only

//! Shared `reqwest::Client` factory (CODEBASE.md §5: one client per transport, shared via `Arc`
//! from `app::wiring` — "Each agent creating its own `reqwest::Client`" is an explicit anti-pattern,
//! PATTERNS.md §15).

use std::time::Duration;

use xlightcli_protocol::ProviderError;

#[derive(Debug, Clone)]
pub struct HttpClientConfig {
    /// Sent as the `User-Agent` header. Adapters set this per transport (some upstreams key
    /// behavior off it — see docs/PLAN.md R-10 re: impersonation risk for experimental
    /// transports; stable transports should use an honest xlightcli user agent).
    pub user_agent: String,
    pub connect_timeout: Duration,
    /// Whole-request timeout. Streaming responses (SSE) should usually disable this in favor of
    /// a per-read/idle timeout — left as a caller concern for Wave 2 transports.
    pub request_timeout: Duration,
}

impl HttpClientConfig {
    pub fn new(user_agent: impl Into<String>) -> Self {
        Self {
            user_agent: user_agent.into(),
            ..Self::default()
        }
    }
}

impl Default for HttpClientConfig {
    fn default() -> Self {
        Self {
            user_agent: format!("xlightcli/{}", env!("CARGO_PKG_VERSION")),
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(120),
        }
    }
}

/// Builds a `reqwest::Client` for one transport. Callers are expected to build this once (in
/// `app::wiring`) and share it via `Arc`/`Clone` (reqwest clients are cheap to clone, backed by a
/// shared connection pool).
pub fn build_client(config: &HttpClientConfig) -> Result<reqwest::Client, ProviderError> {
    reqwest::Client::builder()
        .user_agent(config.user_agent.clone())
        .connect_timeout(config.connect_timeout)
        .timeout(config.request_timeout)
        .build()
        .map_err(|e| ProviderError::Network(e.to_string()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn builds_a_client_with_defaults() {
        let client = build_client(&HttpClientConfig::default());
        assert!(client.is_ok());
    }

    #[test]
    fn custom_user_agent_is_applied() {
        let config = HttpClientConfig::new("xlightcli-test/0.0.1");
        assert_eq!(config.user_agent, "xlightcli-test/0.0.1");
        assert!(build_client(&config).is_ok());
    }
}
