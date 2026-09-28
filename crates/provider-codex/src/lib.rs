// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli-provider-codex` — Codex provider adapter (CODEBASE.md §2): `chatgpt` (stable,
//! ChatGPT OAuth) and `openai-api` (stable, optional, API key) transports (docs/PLAN.md §4.2).
//!
//! Endpoint/header confidence labels live in `consts.rs` and `docs/providers/codex.md`; keep both
//! updated together (PATTERNS.md §5).

mod auth;
mod consts;
mod features;
mod import;
mod quota;
mod transport_api;
mod transport_chatgpt;
mod transport_common;
mod wire;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use xlightcli_protocol::ProviderId;

pub use features::CodexFeaturePack;
pub use import::CodexImporter;

use auth::CodexAuthAdapter;
use xlightcli_auth::AuthAdapter;
use xlightcli_provider::{ConfigImporter, Provider, ProviderFeaturePack, TransportAdapter};

/// Base URLs / paths the Codex adapter talks to. Overridable via [`CodexProvider::with_endpoints`]
/// so transport/quota/auth tests can point at a `wiremock` server instead of the real upstream —
/// and so a corrected value from a live spike doesn't require touching adapter code.
#[derive(Debug, Clone)]
pub struct CodexEndpoints {
    pub chatgpt_backend_base: String,
    pub chatgpt_oauth_authorize_url: String,
    pub chatgpt_oauth_token_url: String,
    pub chatgpt_device_usercode_url: String,
    pub chatgpt_device_token_url: String,
    pub openai_api_base: String,
}

impl Default for CodexEndpoints {
    fn default() -> Self {
        Self {
            chatgpt_backend_base: consts::CHATGPT_BACKEND_BASE.to_string(),
            chatgpt_oauth_authorize_url: consts::CHATGPT_OAUTH_AUTHORIZE_URL.to_string(),
            chatgpt_oauth_token_url: consts::CHATGPT_OAUTH_TOKEN_URL.to_string(),
            chatgpt_device_usercode_url: consts::CHATGPT_DEVICE_USERCODE_URL.to_string(),
            chatgpt_device_token_url: consts::CHATGPT_DEVICE_TOKEN_URL.to_string(),
            openai_api_base: consts::OPENAI_API_BASE.to_string(),
        }
    }
}

/// Codex provider: owns the `chatgpt` and `openai-api` transports.
pub struct CodexProvider {
    http: reqwest::Client,
    endpoints: CodexEndpoints,
    auth: CodexAuthAdapter,
    transports: Vec<Arc<dyn TransportAdapter>>,
    features: CodexFeaturePack,
    importer: CodexImporter,
}

impl std::fmt::Debug for CodexProvider {
    /// Manual impl: `dyn TransportAdapter` has no `Debug` bound, so only transport ids are shown
    /// (matches `provider::testing::MockProvider`'s pattern).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexProvider")
            .field("endpoints", &self.endpoints)
            .field(
                "transports",
                &self.transports.iter().map(|t| t.id()).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl CodexProvider {
    /// Constructor shape shared across all three adapter crates (docs/CONTRACTS.md §5): a shared
    /// `reqwest::Client` plus the resolved experimental flags (D-002). Codex has no experimental
    /// transport today (`chatgpt` is stable per D-002/§15), so `flags` is unused but kept for a
    /// uniform signature across `provider-{codex,claude,agy}`.
    pub fn new(http: reqwest::Client, flags: &xlightcli_config::ExperimentalFlags) -> Self {
        let _ = flags;
        Self::build(http, CodexEndpoints::default())
    }

    /// Overrides the endpoints this provider talks to (tests, or a corrected live-spike value).
    /// Rebuilds every transport/auth/importer so they all see the same endpoints.
    #[must_use]
    pub fn with_endpoints(self, endpoints: CodexEndpoints) -> Self {
        Self::build(self.http, endpoints)
    }

    fn build(http: reqwest::Client, endpoints: CodexEndpoints) -> Self {
        let auth = CodexAuthAdapter::new(http.clone(), endpoints.clone());
        let chatgpt: Arc<dyn TransportAdapter> = Arc::new(
            transport_chatgpt::ChatgptTransport::new(http.clone(), endpoints.clone()),
        );
        let openai_api: Arc<dyn TransportAdapter> = Arc::new(
            transport_api::OpenAiApiTransport::new(http.clone(), endpoints.clone()),
        );
        Self {
            http,
            endpoints,
            auth,
            transports: vec![chatgpt, openai_api],
            features: CodexFeaturePack,
            importer: CodexImporter::default(),
        }
    }
}

/// `$CODEX_HOME`, defaulting to `~/.codex` (D-003: Linux/macOS only, so `$HOME` is enough — no
/// need for the `directories` crate just for another CLI's home directory). Shared by `auth.rs`
/// (discovery) and `import.rs` (config import), which each override it in tests via their own
/// `with_codex_home` constructor rather than touching the real user file.
pub(crate) fn default_codex_home() -> PathBuf {
    if let Ok(dir) = std::env::var("CODEX_HOME")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    Path::new(&home).join(".codex")
}

impl Provider for CodexProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("codex")
    }

    fn display_name(&self) -> &str {
        "Codex"
    }

    fn auth(&self) -> &dyn AuthAdapter {
        &self.auth
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

    use xlightcli_config::ExperimentalFlags;
    use xlightcli_protocol::TransportId;

    use super::*;

    #[test]
    fn provider_exposes_both_transports() {
        let provider = CodexProvider::new(reqwest::Client::new(), &ExperimentalFlags::default());
        let ids: Vec<_> = provider.transports().iter().map(|t| t.id()).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&TransportId::new("chatgpt")));
        assert!(ids.contains(&TransportId::new("openai-api")));
    }

    #[test]
    fn with_endpoints_overrides_the_default() {
        let provider = CodexProvider::new(reqwest::Client::new(), &ExperimentalFlags::default())
            .with_endpoints(CodexEndpoints {
                chatgpt_backend_base: "http://127.0.0.1:0".into(),
                ..CodexEndpoints::default()
            });
        assert_eq!(
            provider.endpoints.chatgpt_backend_base,
            "http://127.0.0.1:0"
        );
    }

    #[test]
    fn id_and_display_name() {
        let provider = CodexProvider::new(reqwest::Client::new(), &ExperimentalFlags::default());
        assert_eq!(provider.id(), ProviderId::new("codex"));
        assert_eq!(provider.display_name(), "Codex");
        assert!(provider.importer().is_some());
    }
}
