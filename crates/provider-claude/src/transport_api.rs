// SPDX-License-Identifier: GPL-3.0-only

//! `TransportAdapter` impl for `anthropic-api` (stable, API key) (PATTERNS.md §5, §6).

use async_trait::async_trait;
use futures::StreamExt;
use http::HeaderValue;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use xlightcli_auth::{AuthError, CredentialHandle};
use xlightcli_protocol::{
    AgentEvent, AuthFailure, AuthKind, CapabilityMode, ModelId, ModelInfo, ProtocolVersion,
    ProviderCapabilities, ProviderError, ProviderId, QuotaSnapshot, ReasoningConfig,
    ReasoningEffort, Stability, TransportId, TurnRequest,
};
use xlightcli_provider::{EventStream, TransportAdapter, map_status};

use crate::wire::{request as wire_request, response as wire_response};
use crate::{ClaudeEndpoints, consts, quota};

pub(crate) fn map_auth_error(err: AuthError) -> ProviderError {
    match err {
        AuthError::NotLoggedIn { .. } => ProviderError::Auth(AuthFailure::NoCredential),
        AuthError::RefreshRejected => ProviderError::Auth(AuthFailure::Rejected),
        other => ProviderError::Auth(AuthFailure::RefreshFailed(other.to_string())),
    }
}

fn provider_id() -> ProviderId {
    ProviderId::new("claude")
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
    display_name: Option<String>,
}

#[derive(Debug)]
pub(crate) struct AnthropicApiTransport {
    http: reqwest::Client,
    base_url: String,
    capabilities: ProviderCapabilities,
}

impl AnthropicApiTransport {
    pub(crate) fn new(http: reqwest::Client, endpoints: &ClaudeEndpoints) -> Self {
        Self {
            http,
            base_url: endpoints.anthropic_api_base_url.clone(),
            capabilities: ProviderCapabilities {
                reasoning: true,
                images: true,
                tool_calls: true,
                parallel_tool_calls: true,
                // Not implemented in Phase 0 (docs/providers/claude.md checklist).
                web_search: CapabilityMode::Unsupported,
                mcp: CapabilityMode::Core,
                session_resume: CapabilityMode::Core,
                // `anthropic-ratelimit-*` headers give real per-request usage info.
                usage: CapabilityMode::Native,
                // No quota/plan endpoint for a plain API key.
                quota: CapabilityMode::Unsupported,
                context_window: None,
            },
        }
    }

    fn messages_url(&self) -> String {
        format!("{}{}", self.base_url, consts::MESSAGES_PATH)
    }

    fn models_url(&self) -> String {
        format!("{}{}", self.base_url, consts::MODELS_PATH)
    }

    fn common_headers(&self) -> http::HeaderMap {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            consts::ANTHROPIC_VERSION_HEADER,
            HeaderValue::from_static(consts::ANTHROPIC_VERSION),
        );
        headers
    }

    /// Sends the Messages request, retrying exactly once after a 401 via
    /// `CredentialHandle::on_unauthorized` (PATTERNS.md §4).
    async fn send(
        &self,
        body: &serde_json::Value,
        cred: &CredentialHandle,
    ) -> Result<reqwest::Response, ProviderError> {
        let mut retried = false;
        loop {
            let mut headers = self.common_headers();
            headers.insert(
                http::header::ACCEPT,
                HeaderValue::from_static("text/event-stream"),
            );
            cred.authorize(&mut headers).await.map_err(map_auth_error)?;
            let resp = self
                .http
                .post(self.messages_url())
                .headers(headers)
                .json(body)
                .send()
                .await
                .map_err(|e| ProviderError::Network(e.to_string()))?;
            if resp.status() == reqwest::StatusCode::UNAUTHORIZED && !retried {
                retried = true;
                cred.on_unauthorized().await.map_err(map_auth_error)?;
                continue;
            }
            if !resp.status().is_success() {
                let status = resp.status().as_u16();
                let headers = resp.headers().clone();
                let body_text = resp.text().await.unwrap_or_default();
                return Err(map_status(status, &headers, &body_text));
            }
            return Ok(resp);
        }
    }
}

#[async_trait]
impl TransportAdapter for AnthropicApiTransport {
    fn id(&self) -> TransportId {
        TransportId::new("anthropic-api")
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
        wire_response::PROTOCOL_VERSION
    }

    async fn list_models(&self, cred: &CredentialHandle) -> Result<Vec<ModelInfo>, ProviderError> {
        let mut headers = self.common_headers();
        cred.authorize(&mut headers).await.map_err(map_auth_error)?;
        let resp = self
            .http
            .get(self.models_url())
            .headers(headers)
            .send()
            .await
            .map_err(|e| ProviderError::Network(e.to_string()))?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let headers = resp.headers().clone();
            let body = resp.text().await.unwrap_or_default();
            return Err(map_status(status, &headers, &body));
        }
        let parsed: ModelsResponse =
            resp.json()
                .await
                .map_err(|e| ProviderError::ProtocolMismatch {
                    expected: wire_response::PROTOCOL_VERSION,
                    detail: format!("GET /v1/models response didn't match the expected shape: {e}"),
                })?;
        Ok(parsed
            .data
            .into_iter()
            .map(|entry| {
                let display_name = entry.display_name.unwrap_or_else(|| entry.id.clone());
                ModelInfo {
                    id: ModelId::new(entry.id),
                    display_name,
                    // Not exposed by `GET /v1/models` (docs/providers/claude.md); left
                    // unknown rather than guessed.
                    context_window: None,
                    max_output_tokens: None,
                    supports_reasoning: true,
                }
            })
            .collect())
    }

    async fn quota(
        &self,
        _cred: &CredentialHandle,
    ) -> Result<Option<QuotaSnapshot>, ProviderError> {
        // No quota/plan endpoint for a plain API key (docs/CONTRACTS.md §6 item 4).
        Ok(None)
    }

    fn apply_effort(&self, req: &mut TurnRequest, effort: ReasoningEffort) {
        req.reasoning = Some(ReasoningConfig {
            effort,
            include_text: true,
        });
    }

    async fn stream(
        &self,
        req: TurnRequest,
        cred: CredentialHandle,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        let model = req.model.clone();
        let transport_id = self.id();
        let body = wire_request::build_body(
            &req,
            &provider_id(),
            &transport_id,
            wire_request::RequestOptions {
                oauth_mode: false,
                stream: true,
            },
        )?;
        let resp = self.send(&body, &cred).await?;
        let rate_limit = quota::parse_standard_rate_limit(resp.headers());
        let byte_stream = resp
            .bytes_stream()
            .map(|item| item.map_err(|e| ProviderError::Network(e.to_string())))
            .boxed();
        let mut sse = xlightcli_provider::parse_sse(byte_stream);

        Ok(Box::pin(async_stream::try_stream! {
            yield AgentEvent::TurnStarted { model };
            if let Some(info) = rate_limit {
                yield AgentEvent::RateLimit(info);
            }
            let mut translator = wire_response::Translator::new(provider_id(), transport_id.clone(), false);
            loop {
                // The `?` operator inside `try_stream!` only rewrites correctly at the macro's own
                // top level, not inside another macro's (`tokio::select!`'s) branch bodies — so the
                // branches below only ever produce a plain value, and every fallible step happens
                // after the `select!` has already resolved (PATTERNS.md §6 sample is illustrative,
                // not literal, on this point).
                let mut cancelled = false;
                let next = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => { cancelled = true; None }
                    next = sse.next() => next,
                };
                if cancelled {
                    Err(ProviderError::Cancelled)?;
                }
                match next {
                    Some(event) => {
                        let event = event?;
                        for out in translator.feed(event)? {
                            yield out;
                        }
                    }
                    None => break,
                }
            }
            yield translator.finish()?;
        }))
    }
}
