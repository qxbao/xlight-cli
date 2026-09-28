// SPDX-License-Identifier: GPL-3.0-only
// Portions derived from OpenCodex (MIT) @ 3cc34e1181926b64331490fdcfee162ffb62fe73:
// src/adapters/google.ts (Cloud Code Assist request construction)
// See THIRD_PARTY.md.

//! `TransportAdapter` impl for `antigravity` (**experimental**, Cloud Code Assist, D-002).
//!
//! Compiled only under cargo feature `antigravity-subscription` (off by default) and gated at
//! runtime by `TransportGate::ensure_enabled` before every `stream()`/`quota()`/`list_models()`
//! call (PATTERNS.md §5).
//!
//! **This transport deliberately impersonates the official Antigravity IDE client**: its HTTP
//! `User-Agent` (`crate::consts::antigravity::request_user_agent`) reproduces the real IDE's
//! decompiled fingerprint, and its OAuth client id/secret are the ones embedded in that same
//! client (`crate::consts::antigravity::OAUTH_CLIENT_ID`) — the Cloud Code Assist backend answers
//! `404` to CLI-shaped User-Agents for newer models, so there is no honest way to reach them
//! without matching the fingerprint the OAuth token was minted under. That is exactly the risk
//! docs/PLAN.md §15 (R-10) and D-002 exist for: this is why the transport is experimental, off by
//! default, and behind an explicit `[experimental]` opt-in — never enable it unconditionally.
//!
//! Every endpoint/header/client-id constant is ported from OpenCodex (MIT)
//! `@ 3cc34e1181926b64331490fdcfee162ffb62fe73` and is **UNVERIFIED as of 2026-09-28** against a
//! live account (`crate::consts::antigravity` doc comments, `docs/providers/agy.md`).

use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use xlightcli_auth::CredentialHandle;
use xlightcli_protocol::{
    AuthKind, CapabilityMode, ModelId, ModelInfo, ProtocolVersion, ProviderCapabilities,
    ProviderError, ProviderId, QuotaSnapshot, ReasoningEffort, Stability, TransportId, TurnRequest,
};
use xlightcli_provider::{EventStream, TransportAdapter, TransportGate};

use crate::consts::antigravity;
use crate::{quota, wire};

fn capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        reasoning: true,
        images: true,
        tool_calls: true,
        parallel_tool_calls: true,
        web_search: CapabilityMode::Unsupported,
        mcp: CapabilityMode::Core,
        session_resume: CapabilityMode::Core,
        usage: CapabilityMode::Native,
        quota: CapabilityMode::Native,
        context_window: None,
    }
}

fn request_headers() -> http::HeaderMap {
    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    if let Ok(value) = http::HeaderValue::from_str(&antigravity::request_user_agent()) {
        headers.insert(http::header::USER_AGENT, value);
    }
    headers
}

/// Resolves the Cloud Code Assist project id: an explicit `AgyEndpoints::antigravity_project_id`
/// override wins if set; otherwise falls back to the id `AgyAuthAdapter::login`/`refresh`
/// discovered and stored on `AccountInfo.metadata` (`consts::antigravity::
/// PROJECT_ID_METADATA_KEY`). A `CredentialHandle::for_tests` handle has no such metadata unless
/// the test puts it there, so tests typically go through the override instead.
fn resolve_project_id(
    override_project_id: &Option<String>,
    cred: &CredentialHandle,
) -> Result<String, ProviderError> {
    if let Some(id) = override_project_id {
        return Ok(id.clone());
    }
    cred.account()
        .metadata
        .get(antigravity::PROJECT_ID_METADATA_KEY)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            ProviderError::InvalidRequest(format!(
                "antigravity requires a discovered Cloud Code Assist project id; log in again so \
                 AgyAuthAdapter can discover it, or set {} / AgyEndpoints::antigravity_project_id \
                 to override (see docs/providers/agy.md)",
                antigravity::PROJECT_ID_ENV
            ))
        })
}

#[derive(Debug)]
pub(crate) struct AntigravityTransport {
    http: reqwest::Client,
    cca_daily_base_url: String,
    project_id: Option<String>,
    capabilities: ProviderCapabilities,
    gate: TransportGate,
    experimental_opt_in: bool,
}

impl AntigravityTransport {
    pub(crate) fn new(
        http: reqwest::Client,
        cca_daily_base_url: String,
        project_id: Option<String>,
        experimental_opt_in: bool,
    ) -> Self {
        Self {
            http,
            cca_daily_base_url,
            project_id,
            capabilities: capabilities(),
            gate: TransportGate::new(TransportId::new("antigravity"), Stability::Experimental),
            experimental_opt_in,
        }
    }

    fn ensure_enabled(&self) -> Result<(), ProviderError> {
        self.gate.ensure_enabled(self.experimental_opt_in)
    }
}

#[async_trait]
impl TransportAdapter for AntigravityTransport {
    fn id(&self) -> TransportId {
        TransportId::new("antigravity")
    }

    fn stability(&self) -> Stability {
        Stability::Experimental
    }

    fn required_auth(&self) -> AuthKind {
        AuthKind::Subscription
    }

    fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    fn protocol_version(&self) -> ProtocolVersion {
        ProtocolVersion(1)
    }

    async fn list_models(&self, cred: &CredentialHandle) -> Result<Vec<ModelInfo>, ProviderError> {
        self.ensure_enabled()?;
        let project = resolve_project_id(&self.project_id, cred)?;
        let url = format!(
            "{}/{}:fetchAvailableModels",
            self.cca_daily_base_url,
            antigravity::CCA_API_VERSION
        );
        let body = serde_json::json!({ "project": project });
        let response =
            wire::post_json_with_retry(&self.http, &url, request_headers(), &body, cred).await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let resp_headers = response.headers().clone();
            let text = response.text().await.unwrap_or_default();
            return Err(crate::wire::map_google_status(status, &resp_headers, &text));
        }
        let json: Value = response
            .json()
            .await
            .map_err(|e| ProviderError::Network(e.to_string()))?;
        Ok(parse_model_catalog(&json))
    }

    async fn quota(&self, cred: &CredentialHandle) -> Result<Option<QuotaSnapshot>, ProviderError> {
        self.ensure_enabled()?;
        let project = resolve_project_id(&self.project_id, cred)?;
        let body = serde_json::json!({ "project": project });

        let summary_url = format!(
            "{}/{}:retrieveUserQuotaSummary",
            self.cca_daily_base_url,
            antigravity::CCA_API_VERSION
        );
        if let Ok(response) =
            wire::post_json_with_retry(&self.http, &summary_url, request_headers(), &body, cred)
                .await
            && response.status().is_success()
            && let Ok(json) = response.json::<Value>().await
            && let Some(snapshot) = quota::parse_quota_summary(&json)
        {
            return Ok(Some(snapshot));
        }

        let models_url = format!(
            "{}/{}:fetchAvailableModels",
            self.cca_daily_base_url,
            antigravity::CCA_API_VERSION
        );
        let response =
            wire::post_json_with_retry(&self.http, &models_url, request_headers(), &body, cred)
                .await?;
        if !response.status().is_success() {
            return Ok(None);
        }
        let json: Value = response
            .json()
            .await
            .map_err(|e| ProviderError::Network(e.to_string()))?;
        Ok(quota::parse_from_models(&json))
    }

    fn apply_effort(&self, req: &mut TurnRequest, effort: ReasoningEffort) {
        crate::apply_effort_default(req, effort);
    }

    async fn stream(
        &self,
        req: TurnRequest,
        cred: CredentialHandle,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        self.ensure_enabled()?;
        let project = resolve_project_id(&self.project_id, &cred)?;
        let provider = ProviderId::new("agy");
        let transport = self.id();
        let flat_body = wire::build_generate_content_body(&provider, &transport, &req)?;
        let session_id = wire::antigravity_session_id(&req.messages);
        let request_id = wire::new_request_id();
        let envelope = wire::build_antigravity_envelope(
            req.model.as_str(),
            &project,
            &request_id,
            &session_id,
            flat_body,
        );
        // Stream on the daily host like the rest of the agent-path calls: OpenCodex's shipped
        // antigravity provider uses `https://daily-cloudcode-pa.googleapis.com` as its baseUrl
        // (src/oauth/index.ts v1 seed fingerprint). Live 2026-09-28: the prod host answered 429
        // RESOURCE_EXHAUSTED while quota for the same account showed 91% remaining.
        let url = format!(
            "{}/{}:streamGenerateContent?alt=sse",
            self.cca_daily_base_url,
            antigravity::CCA_API_VERSION
        );
        let model: ModelId = req.model.clone();
        wire::run_stream(
            self.http.clone(),
            url,
            request_headers(),
            envelope,
            cred,
            provider,
            transport,
            true,
            ProtocolVersion(1),
            model,
            cancel,
        )
        .await
    }
}

/// Parses `v1internal:fetchAvailableModels`' `models` map into `ModelInfo`s. Simplified vs.
/// OpenCodex's `antigravity-models.ts` catalog (no picker collapsing, no retired-tier aliasing,
/// no per-tier suffix expansion — documented as a Phase 0 gap in `docs/providers/agy.md`).
fn parse_model_catalog(body: &Value) -> Vec<ModelInfo> {
    let Some(models) = body.get("models").and_then(Value::as_object) else {
        return Vec::new();
    };
    models
        .iter()
        .map(|(id, info)| ModelInfo {
            id: ModelId::new(id.clone()),
            display_name: info
                .get("displayName")
                .and_then(Value::as_str)
                .unwrap_or(id)
                .to_string(),
            context_window: info
                .get("maxTokens")
                .and_then(Value::as_u64)
                .map(|v| v as u32),
            max_output_tokens: info
                .get("maxOutputTokens")
                .and_then(Value::as_u64)
                .map(|v| v as u32),
            supports_reasoning: info.get("quotaInfo").is_some() && id.contains("gemini"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;
    use serde_json::json;
    use xlightcli_auth::{AccountInfo, CredentialHandle};
    use xlightcli_protocol::{
        AuthKind as AuthKindT, ProviderId as ProviderIdT, TransportId as TransportIdT,
    };

    use super::*;

    fn account() -> AccountInfo {
        AccountInfo {
            provider: ProviderIdT::new("agy"),
            transport: TransportIdT::new("antigravity"),
            account_id: "acc".into(),
            label: None,
            auth_kind: AuthKindT::Subscription,
            metadata: serde_json::json!({}),
        }
    }

    fn transport(experimental_opt_in: bool) -> AntigravityTransport {
        AntigravityTransport::new(
            reqwest::Client::new(),
            "https://example.invalid".into(),
            Some("test-project".into()),
            experimental_opt_in,
        )
    }

    #[tokio::test]
    async fn stream_without_experimental_opt_in_is_transport_disabled() {
        let t = transport(false);
        let cred = CredentialHandle::for_tests(account(), "token");
        // `EventStream` has no `Debug` impl, so `unwrap_err()` doesn't work here — match instead.
        let err = match t
            .stream(
                TurnRequest::simple(ModelId::new("gemini-pro-agent"), "hi"),
                cred,
                CancellationToken::new(),
            )
            .await
        {
            Ok(_) => panic!("expected stream() to fail: experimental opt-in is off"),
            Err(e) => e,
        };
        assert!(matches!(err, ProviderError::TransportDisabled { .. }));
    }

    #[tokio::test]
    async fn quota_without_a_configured_project_id_is_invalid_request() {
        let t = AntigravityTransport::new(
            reqwest::Client::new(),
            "https://example.invalid".into(),
            None,
            true,
        );
        let cred = CredentialHandle::for_tests(account(), "token");
        let err = t.quota(&cred).await.unwrap_err();
        assert!(matches!(err, ProviderError::InvalidRequest(_)));
    }

    #[test]
    fn resolve_project_id_prefers_override_then_falls_back_to_account_metadata() {
        let no_metadata = CredentialHandle::for_tests(account(), "token");
        assert_eq!(
            resolve_project_id(&Some("proj-1".into()), &no_metadata).unwrap(),
            "proj-1",
            "explicit override must win even with no metadata"
        );
        assert!(resolve_project_id(&None, &no_metadata).is_err());

        let mut with_metadata_account = account();
        with_metadata_account.metadata =
            json!({ antigravity::PROJECT_ID_METADATA_KEY: "proj-from-metadata" });
        let with_metadata = CredentialHandle::for_tests(with_metadata_account, "token");
        assert_eq!(
            resolve_project_id(&None, &with_metadata).unwrap(),
            "proj-from-metadata"
        );
    }

    #[test]
    fn parse_model_catalog_reads_display_name_and_limits() {
        let body = json!({
            "models": {
                "gemini-pro-agent": {"displayName": "Gemini 3.1 Pro", "maxTokens": 1000000, "maxOutputTokens": 8192, "quotaInfo": {}}
            }
        });
        let models = parse_model_catalog(&body);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].display_name, "Gemini 3.1 Pro");
        assert_eq!(models[0].context_window, Some(1_000_000));
        assert!(models[0].supports_reasoning);
    }
}
