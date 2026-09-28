// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli-provider-claude` — Claude provider adapter (CODEBASE.md §2): `anthropic-api`
//! (stable, API key) and `claude-subscription` (**experimental**, feature `claude-subscription`,
//! off by default per D-002) transports (docs/PLAN.md §4.2).
//!
//! - `auth`: `AuthAdapter` impl (API key entry; Claude OAuth for the subscription transport,
//!   discovery of `~/.claude`, import & own — D-017).
//! - `wire`: `pub(crate)` Anthropic Messages wire types + the SSE→`AgentEvent` translator, shared
//!   by both transports (docs/PLAN.md §4.2).
//! - `transport_api`: `TransportAdapter` for `anthropic-api`.
//! - `transport_subscription`: `TransportAdapter` for `claude-subscription`, compiled only under
//!   the `claude-subscription` cargo feature **and** gated at runtime by `TransportGate`
//!   (PATTERNS.md §5, D-002) — never enabled by default.
//! - `quota`: `/usage` snapshot parsing (D-027).
//! - `import`: `ConfigImporter` reading `~/.claude` (read-only, D-017).
//! - `features`: `ProviderFeaturePack`; `features::insights` implements `claude.insights`
//!   (Compatible, D-021: self-contained HTML report — not implemented until Phase 2).
//!
//! Ported code (docs/PLAN.md §4.5, `THIRD_PARTY.md`): OAuth endpoints/client id/scopes
//! (`auth.rs`, `consts.rs`), the Claude Code credential file shape (`auth.rs`), and the
//! client-fingerprint headers (`transport_subscription.rs`, `consts.rs`) are ported from
//! OpenCodex (MIT) @ 3cc34e1181926b64331490fdcfee162ffb62fe73.

mod auth;
mod consts;
mod features;
mod import;
mod quota;
mod transport_api;
#[cfg(feature = "claude-subscription")]
mod transport_subscription;
mod wire;

use std::path::PathBuf;
use std::sync::Arc;

use xlightcli_provider::{ConfigImporter, Provider, ProviderFeaturePack, TransportAdapter};

/// Endpoints/client-id this provider talks to — overridable for tests (wiremock) via
/// [`ClaudeProvider::with_endpoints`]. Always public and feature-independent (CONTRACTS.md §5:
/// adapter constructors/public shapes must not change across cargo features), even though the
/// OAuth fields only matter once `claude-subscription` is enabled.
#[derive(Debug, Clone)]
pub struct ClaudeEndpoints {
    /// `anthropic-api` base URL (also used by `claude-subscription`, same Messages endpoint).
    pub anthropic_api_base_url: String,
    pub oauth_authorize_url: String,
    pub oauth_token_url: String,
    pub oauth_client_id: String,
    pub oauth_callback_port: u16,
    pub oauth_callback_path: String,
}

impl Default for ClaudeEndpoints {
    fn default() -> Self {
        Self {
            anthropic_api_base_url: consts::ANTHROPIC_API_BASE_URL.to_string(),
            oauth_authorize_url: consts::OAUTH_AUTHORIZE_URL.to_string(),
            oauth_token_url: consts::OAUTH_TOKEN_URL.to_string(),
            oauth_client_id: consts::OAUTH_CLIENT_ID.to_string(),
            oauth_callback_port: consts::OAUTH_CALLBACK_PORT,
            oauth_callback_path: consts::OAUTH_CALLBACK_PATH.to_string(),
        }
    }
}

/// Claude provider: owns the `anthropic-api` transport, plus `claude-subscription` when the
/// `claude-subscription` cargo feature is enabled.
pub struct ClaudeProvider {
    /// Retained (alongside `experimental_claude_subscription`) so `with_endpoints` can rebuild
    /// every sub-component against new endpoints without the caller re-supplying them.
    http: reqwest::Client,
    experimental_claude_subscription: bool,
    auth: Arc<auth::ClaudeAuthAdapter>,
    transports: Vec<Arc<dyn TransportAdapter>>,
    features: features::ClaudeFeaturePack,
    importer: import::ClaudeImporter,
}

impl ClaudeProvider {
    /// Constructor shape shared across all three adapter crates (docs/CONTRACTS.md §5): a shared
    /// `reqwest::Client` plus the resolved experimental flags (D-002).
    pub fn new(http: reqwest::Client, flags: &xlightcli_config::ExperimentalFlags) -> Self {
        Self::build(http, flags.claude_subscription, ClaudeEndpoints::default())
    }

    /// Overrides the endpoints/client id this provider talks to (tests only — wiremock needs a
    /// local base URL instead of `https://api.anthropic.com`).
    pub fn with_endpoints(self, endpoints: ClaudeEndpoints) -> Self {
        Self::build(self.http, self.experimental_claude_subscription, endpoints)
    }

    fn build(http: reqwest::Client, claude_subscription: bool, endpoints: ClaudeEndpoints) -> Self {
        #[allow(unused_mut)] // only mutated when the `claude-subscription` feature is enabled
        let mut transports: Vec<Arc<dyn TransportAdapter>> = vec![Arc::new(
            transport_api::AnthropicApiTransport::new(http.clone(), &endpoints),
        )];
        #[cfg(feature = "claude-subscription")]
        {
            transports.push(Arc::new(
                transport_subscription::AnthropicSubscriptionTransport::new(
                    http.clone(),
                    &endpoints,
                    claude_subscription,
                ),
            ));
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            http: http.clone(),
            experimental_claude_subscription: claude_subscription,
            auth: Arc::new(auth::ClaudeAuthAdapter::new(http, endpoints)),
            transports,
            features: features::ClaudeFeaturePack,
            importer: import::ClaudeImporter::new(home, None),
        }
    }
}

impl std::fmt::Debug for ClaudeProvider {
    /// Manual impl: `dyn TransportAdapter` has no `Debug` bound, so only transport ids are shown
    /// (matches `xlightcli_provider::testing::MockProvider`'s pattern).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaudeProvider")
            .field(
                "transports",
                &self.transports.iter().map(|t| t.id()).collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl Provider for ClaudeProvider {
    fn id(&self) -> xlightcli_protocol::ProviderId {
        xlightcli_protocol::ProviderId::new("claude")
    }

    fn display_name(&self) -> &str {
        "Claude"
    }

    fn auth(&self) -> &dyn xlightcli_auth::AuthAdapter {
        self.auth.as_ref()
    }

    fn transports(&self) -> &[Arc<dyn TransportAdapter>] {
        &self.transports
    }

    fn features(&self) -> &dyn ProviderFeaturePack {
        &self.features
    }

    fn importer(&self) -> Option<&dyn ConfigImporter> {
        Some(&self.importer)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use xlightcli_protocol::TransportId;

    use super::*;

    #[test]
    fn default_build_only_exposes_the_stable_api_transport() {
        let provider = ClaudeProvider::new(
            reqwest::Client::new(),
            &xlightcli_config::ExperimentalFlags::default(),
        );
        assert_eq!(provider.id().as_str(), "claude");
        assert!(
            provider
                .transport(&TransportId::new("anthropic-api"))
                .is_some()
        );
        #[cfg(not(feature = "claude-subscription"))]
        assert!(
            provider
                .transport(&TransportId::new("claude-subscription"))
                .is_none()
        );
    }

    #[cfg(feature = "claude-subscription")]
    #[test]
    fn subscription_transport_is_present_when_the_feature_is_enabled() {
        let provider = ClaudeProvider::new(
            reqwest::Client::new(),
            &xlightcli_config::ExperimentalFlags::default(),
        );
        assert!(
            provider
                .transport(&TransportId::new("claude-subscription"))
                .is_some()
        );
    }

    #[test]
    fn importer_is_always_available() {
        let provider = ClaudeProvider::new(
            reqwest::Client::new(),
            &xlightcli_config::ExperimentalFlags::default(),
        );
        assert!(provider.importer().is_some());
    }
}
