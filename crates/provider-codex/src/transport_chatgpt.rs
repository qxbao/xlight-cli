// SPDX-License-Identifier: GPL-3.0-only

//! `TransportAdapter` impl for `chatgpt` (stable, ChatGPT OAuth backend, D-002: Codex subscription
//! is the one stable subscription transport — still gated through `TransportGate` for kill-switch
//! consistency, docs/PLAN.md §15).

use async_trait::async_trait;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use xlightcli_auth::CredentialHandle;
use xlightcli_protocol::{
    AgentEvent, AuthKind, CapabilityMode, ModelId, ModelInfo, ProtocolVersion,
    ProviderCapabilities, ProviderError, ProviderId, QuotaSnapshot, RateLimitInfo, ReasoningConfig,
    ReasoningEffort, Stability, TransportId, TurnRequest,
};
use xlightcli_provider::{EventStream, TransportAdapter, TransportGate};

use crate::CodexEndpoints;
use crate::consts;
use crate::quota;
use crate::transport_common::{get_json, send_with_reauth};
use crate::wire::{self, PROTOCOL_VERSION};

const TRANSPORT_ID: &str = "chatgpt";

#[derive(Debug)]
pub(crate) struct ChatgptTransport {
    http: reqwest::Client,
    endpoints: CodexEndpoints,
    capabilities: ProviderCapabilities,
    gate: TransportGate,
}

impl ChatgptTransport {
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
        // U: web_search / apps connectors exist upstream but are not implemented in Phase 0.
        web_search: CapabilityMode::Unsupported,
        mcp: CapabilityMode::Core,
        session_resume: CapabilityMode::Core,
        usage: CapabilityMode::Native,
        quota: CapabilityMode::Native,
        // U: per-model context window not in the Phase 0 static model list.
        context_window: None,
    }
}

/// Parses the `x-codex-primary-*` rate-limit headers (docs/providers/codex.md, **U**) off the
/// initial HTTP response, best-effort. Sent as one `AgentEvent::RateLimit` right before the
/// stream's `TurnStarted`, since it comes from headers available before any SSE body is read.
fn rate_limit_from_headers(headers: &http::HeaderMap) -> Option<RateLimitInfo> {
    let used_percent = header_value(headers, consts::HEADER_X_CODEX_PRIMARY_USED_PERCENT)?
        .parse::<f64>()
        .ok()?;
    let remaining = ((100.0 - used_percent).max(0.0)).round() as u64;
    let reset_at = header_value(headers, consts::HEADER_X_CODEX_PRIMARY_RESET_AT).and_then(|v| {
        time::OffsetDateTime::parse(&v, &time::format_description::well_known::Rfc3339).ok()
    });
    Some(RateLimitInfo {
        limit: Some(100),
        remaining: Some(remaining),
        reset_at,
    })
}

fn header_value(headers: &http::HeaderMap, name: &str) -> Option<String> {
    headers.get(name)?.to_str().ok().map(str::to_string)
}

/// **U**: Phase 0 static list — the `chatgpt` backend's model-catalog endpoint is not
/// independently verified (docs/providers/codex.md), so `list_models` cannot call it yet.
fn static_models() -> Vec<ModelInfo> {
    vec![ModelInfo {
        id: ModelId::new("gpt-5-codex"),
        display_name: "GPT-5 Codex".to_string(),
        context_window: None,
        max_output_tokens: None,
        supports_reasoning: true,
    }]
}

#[async_trait]
impl TransportAdapter for ChatgptTransport {
    fn id(&self) -> TransportId {
        TransportId::new(TRANSPORT_ID)
    }

    fn stability(&self) -> Stability {
        Stability::Stable
    }

    fn required_auth(&self) -> AuthKind {
        AuthKind::Subscription
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

    async fn quota(&self, cred: &CredentialHandle) -> Result<Option<QuotaSnapshot>, ProviderError> {
        let url = format!(
            "{}{}",
            self.endpoints.chatgpt_backend_base,
            consts::CHATGPT_USAGE_PATH
        );
        let value = get_json(&self.http, &url, cred).await?;
        Ok(Some(quota::parse_usage_snapshot(&value)))
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
        // `chatgpt` is Stable (D-002); still routed through the gate so the kill switch
        // (`xlightcli provider disable chatgpt`) works uniformly across transports.
        self.gate.ensure_enabled(true)?;

        let provider = ProviderId::new("codex");
        let transport = TransportId::new(TRANSPORT_ID);
        let model = req.model.clone();
        let body =
            wire::request::build_request_body(&req, req.model.as_str(), &provider, &transport);
        let url = format!(
            "{}{}",
            self.endpoints.chatgpt_backend_base,
            consts::RESPONSES_PATH
        );
        // `originator` stays `xlightcli` on purpose: this is the stable transport, so it doesn't
        // present itself as the official Codex client (INV-9).
        let account_id = cred.account().account_id.clone();
        let extra_headers = [
            (consts::HEADER_ORIGINATOR, consts::ORIGINATOR),
            (consts::HEADER_CHATGPT_ACCOUNT_ID, account_id.as_str()),
        ];
        let resp = send_with_reauth(&self.http, &url, &extra_headers, &body, &cred).await?;
        let initial_rate_limit = rate_limit_from_headers(resp.headers());

        let byte_stream = resp
            .bytes_stream()
            .map(|chunk| chunk.map_err(|e| ProviderError::Network(e.to_string())));
        let mut sse = xlightcli_provider::sse::parse(byte_stream);
        let mut translator = wire::response::Translator::new(provider, transport, model);

        // NOTE: `?`/`yield` inside `tokio::select!`'s arms are opaque to `try_stream!`'s macro
        // (it only special-cases the bare `yield` keyword inside foreign macro invocations, not
        // `?`); the select! only ever *produces a value*, all `?`/`yield` happen in plain code
        // right after it.
        Ok(Box::pin(async_stream::try_stream! {
            if let Some(info) = initial_rate_limit {
                yield AgentEvent::RateLimit(info);
            }
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
