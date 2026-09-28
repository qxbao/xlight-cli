// SPDX-License-Identifier: GPL-3.0-only

//! `TransportAdapter` impl for `openai-api` (stable, optional, `OPENAI_API_KEY`). Same Responses
//! wire shape as `chatgpt` (`wire::request`/`wire::response` are shared), different base URL and
//! no ChatGPT-only quota endpoint.

use async_trait::async_trait;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use xlightcli_auth::CredentialHandle;
use xlightcli_protocol::{
    AuthKind, CapabilityMode, ModelId, ModelInfo, ProtocolVersion, ProviderCapabilities,
    ProviderError, ProviderId, QuotaSnapshot, ReasoningConfig, ReasoningEffort, Stability,
    TransportId, TurnRequest,
};
use xlightcli_provider::{EventStream, TransportAdapter, TransportGate};

use crate::CodexEndpoints;
use crate::consts;
use crate::transport_common::{get_json, send_with_reauth};
use crate::wire::{self, PROTOCOL_VERSION};

const TRANSPORT_ID: &str = "openai-api";

#[derive(Debug)]
pub(crate) struct OpenAiApiTransport {
    http: reqwest::Client,
    endpoints: CodexEndpoints,
    capabilities: ProviderCapabilities,
    gate: TransportGate,
}

impl OpenAiApiTransport {
    pub(crate) fn new(http: reqwest::Client, endpoints: CodexEndpoints) -> Self {
        Self {
            http,
            endpoints,
            capabilities: capabilities(),
            gate: TransportGate::new(TransportId::new(TRANSPORT_ID), Stability::Stable),
        }
    }
}

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
        // The public OpenAI API has no per-key `/usage` endpoint an ordinary API key can call
        // (only an org-admin key can, out of scope for Phase 0) — see docs/commands.md §6.
        quota: CapabilityMode::Unsupported,
        context_window: None,
    }
}

/// Public `GET /v1/models` catalog. It may contain models unavailable for Responses; the first
/// turn remains the authority for whether a selected model can serve this request.
fn parse_models(body: &serde_json::Value) -> Result<Vec<ModelInfo>, ProviderError> {
    let data = body
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| ProviderError::ProtocolMismatch {
            expected: PROTOCOL_VERSION,
            detail: "GET /v1/models response has no data array".to_string(),
        })?;
    let mut models: Vec<_> = data
        .iter()
        .filter_map(|entry| {
            let id = entry.get("id")?.as_str()?;
            Some(ModelInfo {
                id: ModelId::new(id),
                display_name: id.to_string(),
                context_window: None,
                max_output_tokens: None,
                // The catalog does not report reasoning capability per model.
                supports_reasoning: false,
            })
        })
        .collect();
    models.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
    Ok(models)
}

#[async_trait]
impl TransportAdapter for OpenAiApiTransport {
    fn id(&self) -> TransportId {
        TransportId::new(TRANSPORT_ID)
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
        PROTOCOL_VERSION
    }

    async fn list_models(&self, cred: &CredentialHandle) -> Result<Vec<ModelInfo>, ProviderError> {
        self.gate.ensure_enabled(true)?;
        let url = format!(
            "{}/models",
            self.endpoints.openai_api_base.trim_end_matches('/')
        );
        let body = get_json(&self.http, &url, cred).await?;
        parse_models(&body)
    }

    async fn quota(
        &self,
        _cred: &CredentialHandle,
    ) -> Result<Option<QuotaSnapshot>, ProviderError> {
        Ok(None)
    }

    fn apply_effort(&self, req: &mut TurnRequest, effort: ReasoningEffort) {
        match &mut req.reasoning {
            Some(cfg) => cfg.effort = effort,
            None => {
                req.reasoning = Some(ReasoningConfig {
                    effort,
                    include_text: false,
                })
            }
        }
    }

    async fn stream(
        &self,
        req: TurnRequest,
        cred: CredentialHandle,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        self.gate.ensure_enabled(true)?;

        let provider = ProviderId::new("codex");
        let transport = TransportId::new(TRANSPORT_ID);
        let model = req.model.clone();
        let body =
            wire::request::build_request_body(&req, req.model.as_str(), &provider, &transport);
        let url = format!(
            "{}{}",
            self.endpoints.openai_api_base,
            consts::RESPONSES_PATH
        );
        let resp = send_with_reauth(&self.http, &url, &[], &body, &cred).await?;

        let byte_stream = resp
            .bytes_stream()
            .map(|chunk| chunk.map_err(|e| ProviderError::Network(e.to_string())));
        let mut sse = xlightcli_provider::sse::parse(byte_stream);
        let mut translator = wire::response::Translator::new(provider, transport, model);

        // See the matching comment in `transport_chatgpt.rs`: `?` inside `tokio::select!`'s arms
        // is opaque to `try_stream!`'s macro, so the select! only produces a value here.
        Ok(Box::pin(async_stream::try_stream! {
            loop {
                let next: Option<Result<xlightcli_provider::SseEvent, ProviderError>> = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => Some(Err(ProviderError::Cancelled)),
                    ev = sse.next() => ev,
                };
                let Some(event) = next else { break };
                for out in translator.feed(event?)? {
                    yield out;
                }
            }
            yield translator.finish()?;
        }))
    }
}

#[cfg(test)]
mod model_tests {
    use super::*;

    #[test]
    fn parses_live_model_ids_without_inventing_capabilities() {
        let models = parse_models(&serde_json::json!({"data": [
            {"id": "gpt-6-sol"}, {"id": "gpt-5.6-terra"}
        ]}))
        .unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id.as_str(), "gpt-5.6-terra");
        assert!(!models[0].supports_reasoning);
    }

    #[test]
    fn missing_data_array_is_a_protocol_mismatch() {
        assert!(matches!(
            parse_models(&serde_json::json!({})),
            Err(ProviderError::ProtocolMismatch { .. })
        ));
    }
}
