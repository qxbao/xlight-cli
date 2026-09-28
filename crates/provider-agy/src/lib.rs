// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli-provider-agy` — Antigravity (agy) provider adapter (CODEBASE.md §2): `gemini-api`
//! (stable, API key) and `antigravity` (**experimental**, feature `antigravity-subscription`, off
//! by default per D-002) transports (docs/PLAN.md §4.2).
//!
//! - `auth`: `AuthAdapter` impl (API key entry; Google OAuth for the antigravity transport,
//!   discovery of `~/.gemini`/`.agents/`, import & own — D-017).
//! - `wire`: `pub(crate)` Gemini / Cloud Code Assist wire types + the SSE→`AgentEvent` translator,
//!   shared by both transports (docs/PLAN.md §4.2).
//! - `transport_gemini`: `TransportAdapter` for `gemini-api` (`streamGenerateContent`).
//! - `transport_antigravity`: `TransportAdapter` for `antigravity` (Cloud Code Assist), compiled
//!   only under the `antigravity-subscription` cargo feature **and** gated at runtime by
//!   `TransportGate` (PATTERNS.md §5, D-002) — never enabled by default.
//! - `quota`: `/usage` quota snapshot parsing (D-027), Native if the backend exposes it.
//! - `import`: `ConfigImporter` reading `~/.gemini`/`.agents/` (read-only, D-017).
//! - `features`: `ProviderFeaturePack` + agy-specific commands (`docs/commands.md`).

mod auth;
mod consts;
mod features;
mod import;
// Only `transport_antigravity` uses this (`gemini-api`'s `quota()` is always `Ok(None)`).
#[cfg(feature = "antigravity-subscription")]
mod quota;
#[cfg(feature = "antigravity-subscription")]
mod transport_antigravity;
mod transport_gemini;
mod wire;

use std::sync::Arc;

use xlightcli_auth::{AuthAdapter, AuthError};
use xlightcli_protocol::{
    AuthFailure, ProviderError, ReasoningConfig, ReasoningEffort, TurnRequest,
};
use xlightcli_provider::{ConfigImporter, Provider, ProviderFeaturePack, TransportAdapter};

/// Overridable endpoints (`Default` reads env overrides once; wiremock tests use
/// [`AgyProvider::with_endpoints`] to point at a mock server instead).
#[derive(Debug, Clone)]
pub struct AgyEndpoints {
    /// `gemini-api` base URL. Overridable via `GOOGLE_GEMINI_BASE_URL` (docs/providers/agy.md).
    pub gemini_base_url: String,
    /// Cloud Code Assist production base URL (`streamGenerateContent`, `loadCodeAssist`).
    pub cca_base_url: String,
    /// Cloud Code Assist "daily" base URL (quota, onboarding) — a distinct host in the ported
    /// source (`docs/providers/agy.md`, UNVERIFIED).
    pub cca_daily_base_url: String,
    /// Cloud Code Assist project id discovered during OAuth login (`loadCodeAssist`/
    /// `onboardUser`). **Known Phase 0 gap**: `xlightcli_auth::CredentialSecret` has no slot for
    /// this non-secret per-account metadata, so there is currently no principled way for
    /// `AgyAuthAdapter::login` to hand it to the transport per-credential; this env-sourced
    /// override is the stopgap (`GOOGLE_ANTIGRAVITY_PROJECT_ID`) until that contract grows one
    /// (flagged to the team lead — see the Wave 2 report and `docs/providers/agy.md`).
    pub antigravity_project_id: Option<String>,
}

impl Default for AgyEndpoints {
    fn default() -> Self {
        let gemini_base_url = std::env::var(consts::gemini_api::BASE_URL_ENV)
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| consts::gemini_api::DEFAULT_BASE_URL.to_string());
        let antigravity_project_id = std::env::var(consts::antigravity::PROJECT_ID_ENV)
            .ok()
            .filter(|v| !v.is_empty());
        Self {
            gemini_base_url,
            cca_base_url: consts::antigravity::CCA_PROD_BASE_URL.to_string(),
            cca_daily_base_url: consts::antigravity::CCA_DAILY_BASE_URL.to_string(),
            antigravity_project_id,
        }
    }
}

/// Antigravity (agy) provider: owns the `gemini-api` transport, plus `antigravity` when the
/// `antigravity-subscription` cargo feature is enabled.
pub struct AgyProvider {
    http: reqwest::Client,
    experimental_antigravity_subscription: bool,
    auth: auth::AgyAuthAdapter,
    features: features::AgyFeaturePack,
    importer: import::AgyMcpImporter,
    transports: Vec<Arc<dyn TransportAdapter>>,
    endpoints: AgyEndpoints,
}

/// Manual impl: `dyn TransportAdapter` has no `Debug` bound (matches
/// `xlightcli_provider::testing::MockProvider`'s reasoning), so only transport ids are shown.
impl std::fmt::Debug for AgyProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgyProvider")
            .field("endpoints", &self.endpoints)
            .field(
                "transports",
                &self.transports.iter().map(|t| t.id()).collect::<Vec<_>>(),
            )
            .finish()
    }
}

fn build_transports(
    http: &reqwest::Client,
    experimental_antigravity_subscription: bool,
    endpoints: &AgyEndpoints,
) -> Vec<Arc<dyn TransportAdapter>> {
    #[cfg_attr(not(feature = "antigravity-subscription"), allow(unused_mut))]
    let mut transports: Vec<Arc<dyn TransportAdapter>> = vec![Arc::new(
        transport_gemini::GeminiApiTransport::new(http.clone(), endpoints.gemini_base_url.clone()),
    )];
    #[cfg(feature = "antigravity-subscription")]
    {
        transports.push(Arc::new(transport_antigravity::AntigravityTransport::new(
            http.clone(),
            endpoints.cca_daily_base_url.clone(),
            endpoints.antigravity_project_id.clone(),
            experimental_antigravity_subscription,
        )));
    }
    #[cfg(not(feature = "antigravity-subscription"))]
    {
        let _ = experimental_antigravity_subscription;
    }
    transports
}

impl AgyProvider {
    /// Constructor shape shared across all three adapter crates (Wave 1 brief): a shared
    /// `reqwest::Client` plus the resolved experimental flags (D-002).
    pub fn new(http: reqwest::Client, flags: &xlightcli_config::ExperimentalFlags) -> Self {
        let endpoints = AgyEndpoints::default();
        let transports = build_transports(&http, flags.antigravity_subscription, &endpoints);
        Self {
            auth: auth::AgyAuthAdapter::new(http.clone()),
            features: features::AgyFeaturePack,
            importer: import::AgyMcpImporter::new(),
            transports,
            endpoints,
            experimental_antigravity_subscription: flags.antigravity_subscription,
            http,
        }
    }

    /// Overrides the resolved endpoints (wiremock tests point these at a mock server) and
    /// rebuilds the transport list against them, keeping the experimental opt-in and shared
    /// `reqwest::Client` `new()` was built with.
    pub fn with_endpoints(mut self, endpoints: AgyEndpoints) -> Self {
        self.transports = build_transports(
            &self.http,
            self.experimental_antigravity_subscription,
            &endpoints,
        );
        self.endpoints = endpoints;
        self
    }
}

impl Provider for AgyProvider {
    fn id(&self) -> xlightcli_protocol::ProviderId {
        xlightcli_protocol::ProviderId::new("agy")
    }

    fn display_name(&self) -> &str {
        "Antigravity (Gemini)"
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

/// Maps `xlightcli_auth::AuthError` (crossing from `auth` into a `ProviderError`-typed stream
/// item) — the crate boundary the two error enums don't otherwise cross automatically. Kept as
/// one small function rather than repeated `match`es at every call site (PATTERNS.md §2: map wire
/// errors once at the transport boundary; this is the auth-error analogue of that rule).
pub(crate) fn map_auth_error(err: AuthError) -> ProviderError {
    match err {
        AuthError::NotLoggedIn { .. } => ProviderError::Auth(AuthFailure::NoCredential),
        AuthError::RefreshRejected => ProviderError::Auth(AuthFailure::RefreshFailed(
            "refresh rejected by upstream".into(),
        )),
        AuthError::StoreUnavailable(detail) => {
            ProviderError::Auth(AuthFailure::RefreshFailed(detail))
        }
        AuthError::OAuth(detail) => ProviderError::Auth(AuthFailure::RefreshFailed(detail)),
        AuthError::NotImplemented(detail) => {
            ProviderError::Auth(AuthFailure::RefreshFailed(detail.to_string()))
        }
    }
}

/// Shared `TransportAdapter::apply_effort` body for both transports (docs/providers/agy.md:
/// effort is mapped to `generationConfig.thinkingConfig.thinkingLevel` at request-build time,
/// `wire::effort_to_thinking_level`). Preserves an already-set `include_text` preference.
pub(crate) fn apply_effort_default(req: &mut TurnRequest, effort: ReasoningEffort) {
    let include_text = req
        .reasoning
        .as_ref()
        .map(|r| r.include_text)
        .unwrap_or(true);
    req.reasoning = Some(ReasoningConfig {
        effort,
        include_text,
    });
}
