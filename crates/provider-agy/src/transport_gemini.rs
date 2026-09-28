// SPDX-License-Identifier: GPL-3.0-only

//! `TransportAdapter` impl for `gemini-api` (stable, API key, `streamGenerateContent`).
//!
//! Public Generative Language API (`ai.google.dev`) — H confidence, official Google docs, not an
//! OpenCodex port (docs/providers/agy.md). Authenticated with a plain API key
//! (`x-goog-api-key`, `GEMINI_API_KEY` / `ui.prompt_api_key`), always available (no cargo feature
//! gate, unlike `antigravity`).

use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use xlightcli_auth::CredentialHandle;
use xlightcli_protocol::{
    AuthKind, CapabilityMode, ModelId, ModelInfo, ProtocolVersion, ProviderCapabilities,
    ProviderError, ProviderId, QuotaSnapshot, ReasoningEffort, Stability, TransportId, TurnRequest,
};
use xlightcli_provider::{EventStream, TransportAdapter};

use crate::consts::gemini_api;
use crate::wire;

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
        quota: CapabilityMode::Unsupported,
        context_window: None,
    }
}

#[derive(Debug)]
pub(crate) struct GeminiApiTransport {
    http: reqwest::Client,
    base_url: String,
    capabilities: ProviderCapabilities,
}

impl GeminiApiTransport {
    pub(crate) fn new(http: reqwest::Client, base_url: String) -> Self {
        Self {
            http,
            base_url,
            capabilities: capabilities(),
        }
    }
}

fn parse_model_list(body: &Value) -> Vec<ModelInfo> {
    let Some(models) = body.get("models").and_then(Value::as_array) else {
        return Vec::new();
    };
    models
        .iter()
        .filter_map(|model| {
            let name = model.get("name").and_then(Value::as_str)?;
            let id = name.strip_prefix("models/").unwrap_or(name);
            Some(ModelInfo {
                id: ModelId::new(id),
                display_name: model
                    .get("displayName")
                    .and_then(Value::as_str)
                    .unwrap_or(id)
                    .to_string(),
                context_window: model
                    .get("inputTokenLimit")
                    .and_then(Value::as_u64)
                    .map(|v| v as u32),
                max_output_tokens: model
                    .get("outputTokenLimit")
                    .and_then(Value::as_u64)
                    .map(|v| v as u32),
                supports_reasoning: model
                    .get("displayName")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains("thinking")
                    || id.contains("thinking"),
            })
        })
        .collect()
}

#[async_trait]
impl TransportAdapter for GeminiApiTransport {
    fn id(&self) -> TransportId {
        TransportId::new("gemini-api")
    }

    fn stability(&self) -> Stability {
        Stability::Stable
    }

    fn required_auth(&self) -> AuthKind {
        AuthKind::ApiKey
    }

    fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    fn protocol_version(&self) -> ProtocolVersion {
        ProtocolVersion(1)
    }

    async fn list_models(&self, cred: &CredentialHandle) -> Result<Vec<ModelInfo>, ProviderError> {
        let url = format!("{}/{}/models", self.base_url, gemini_api::API_VERSION);
        let mut headers = http::HeaderMap::new();
        cred.authorize(&mut headers)
            .await
            .map_err(crate::map_auth_error)?;
        let response = self
            .http
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| ProviderError::Network(e.to_string()))?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let resp_headers = response.headers().clone();
            let text = response.text().await.unwrap_or_default();
            return Err(crate::wire::map_google_status(status, &resp_headers, &text));
        }
        let body: Value = response
            .json()
            .await
            .map_err(|e| ProviderError::Network(e.to_string()))?;
        Ok(parse_model_list(&body))
    }

    async fn quota(
        &self,
        _cred: &CredentialHandle,
    ) -> Result<Option<QuotaSnapshot>, ProviderError> {
        // The public Generative Language API exposes no per-key quota endpoint
        // (docs/providers/agy.md transport table).
        Ok(None)
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
        let provider = ProviderId::new("agy");
        let transport = self.id();
        let body = wire::build_generate_content_body(&provider, &transport, &req)?;
        let url = format!(
            "{}/{}/models/{}:streamGenerateContent?alt=sse",
            self.base_url,
            gemini_api::API_VERSION,
            req.model.as_str()
        );
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
        let model = req.model.clone();
        wire::run_stream(
            self.http.clone(),
            url,
            headers,
            body,
            cred,
            provider,
            transport,
            false,
            ProtocolVersion(1),
            model,
            cancel,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    #[test]
    fn parse_model_list_strips_models_prefix_and_reads_limits() {
        let body = json!({
            "models": [
                {"name": "models/gemini-3.1-pro", "displayName": "Gemini 3.1 Pro", "inputTokenLimit": 1000000, "outputTokenLimit": 8192}
            ]
        });
        let models = parse_model_list(&body);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, ModelId::new("gemini-3.1-pro"));
        assert_eq!(models[0].context_window, Some(1_000_000));
        assert_eq!(models[0].max_output_tokens, Some(8192));
    }

    #[test]
    fn parse_model_list_handles_missing_models_key() {
        assert!(parse_model_list(&json!({})).is_empty());
    }
}
