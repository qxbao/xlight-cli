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
use crate::transport_common::send_with_reauth;
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

/// **H**: public OpenAI model ids; **U**: this exact short list vs. the full catalog (Phase 0
/// doesn't call `GET /v1/models` yet).
fn static_models() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            id: ModelId::new("gpt-5-codex"),
            display_name: "GPT-5 Codex".to_string(),
            context_window: None,
            max_output_tokens: None,
            supports_reasoning: true,
        },
        ModelInfo {
            id: ModelId::new("gpt-5"),
            display_name: "GPT-5".to_string(),
            context_window: None,
            max_output_tokens: None,
            supports_reasoning: true,
        },
    ]
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

    async fn list_models(&self, _cred: &CredentialHandle) -> Result<Vec<ModelInfo>, ProviderError> {
        Ok(static_models())
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
