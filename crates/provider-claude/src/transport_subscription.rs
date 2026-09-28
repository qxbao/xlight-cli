// SPDX-License-Identifier: GPL-3.0-only
// Portions derived from OpenCodex (MIT) @ 3cc34e1181926b64331490fdcfee162ffb62fe73:
// src/adapters/client-fingerprint.ts (header values only).
// See THIRD_PARTY.md.

//! `TransportAdapter` impl for `claude-subscription` (**experimental**, D-002) (PATTERNS.md §5).
//!
//! Compiled only under cargo feature `claude-subscription` (off by default) and calls
//! `TransportGate::ensure_enabled` before every `stream()`/`quota()`/`list_models()` call. Shares
//! `wire::request`/`wire::response` with `transport_api`; the only wire differences are handled by
//! `wire::request::RequestOptions::oauth_mode` (docs/PLAN.md §4.5 port map).
//!
//! Sends Claude Code's own client-fingerprint headers (`consts::CLAUDE_CODE_STATIC_HEADERS`) —
//! see the doc comment there for why this is in-scope for this transport specifically (INV-9) and
//! nowhere else.

use std::sync::Mutex;

use async_trait::async_trait;
use futures::StreamExt;
use http::HeaderValue;
use tokio_util::sync::CancellationToken;
use xlightcli_auth::CredentialHandle;
use xlightcli_protocol::{
    AgentEvent, AuthKind, CapabilityMode, ModelInfo, ProtocolVersion, ProviderCapabilities,
    ProviderError, ProviderId, QuotaSnapshot, ReasoningConfig, ReasoningEffort, Stability,
    TransportId, TurnRequest,
};
use xlightcli_provider::{EventStream, TransportAdapter, TransportGate, map_status};

use crate::transport_api::map_auth_error;
use crate::wire::{request as wire_request, response as wire_response};
use crate::{ClaudeEndpoints, consts, quota};

fn provider_id() -> ProviderId {
    ProviderId::new("claude")
}

/// Best-effort translation of this process's arch/OS into the same vocabulary the real Claude
/// Code CLI's `X-Stainless-Arch`/`-OS` headers use (Node's `process.arch`/`process.platform`).
/// **M** — approximate; only needs to be plausible, not byte-identical.
fn stainless_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    }
}

fn stainless_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    }
}

#[derive(Debug)]
pub(crate) struct AnthropicSubscriptionTransport {
    http: reqwest::Client,
    base_url: String,
    capabilities: ProviderCapabilities,
    gate: TransportGate,
    experimental_opt_in: bool,
    /// Stable per-process session id, mirroring the real client's one-id-per-CLI-session
    /// behavior (simplified: a random id generated once, rather than the deterministic
    /// token-derived hash OpenCodex uses — see `consts.rs`).
    session_id: String,
    last_quota: Mutex<Option<QuotaSnapshot>>,
}

impl AnthropicSubscriptionTransport {
    pub(crate) fn new(
        http: reqwest::Client,
        endpoints: &ClaudeEndpoints,
        experimental_opt_in: bool,
    ) -> Self {
        Self {
            http,
            base_url: endpoints.anthropic_api_base_url.clone(),
            capabilities: ProviderCapabilities {
                reasoning: true,
                images: true,
                tool_calls: true,
                parallel_tool_calls: true,
                web_search: CapabilityMode::Unsupported,
                mcp: CapabilityMode::Core,
                session_resume: CapabilityMode::Core,
                usage: CapabilityMode::Native,
                // `anthropic-ratelimit-unified-*` rolling-window headers (**U**, unverified —
                // see `quota()`/`quota.rs::parse_unified_quota`).
                quota: CapabilityMode::Native,
                context_window: None,
            },
            gate: TransportGate::new(
                TransportId::new("claude-subscription"),
                Stability::Experimental,
            ),
            experimental_opt_in,
            session_id: uuid::Uuid::new_v4().to_string(),
            last_quota: Mutex::new(None),
        }
    }

    fn messages_url(&self) -> String {
        format!("{}{}", self.base_url, consts::MESSAGES_PATH)
    }

    fn common_headers(&self) -> http::HeaderMap {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            consts::ANTHROPIC_VERSION_HEADER,
            HeaderValue::from_static(consts::ANTHROPIC_VERSION),
        );
        headers.insert(
            consts::ANTHROPIC_BETA_HEADER,
            HeaderValue::from_static(consts::OAUTH_BETA_HEADER_VALUE),
        );
        for &(name, value) in consts::CLAUDE_CODE_STATIC_HEADERS {
            headers.insert(name, HeaderValue::from_static(value));
        }
        headers.insert(
            "x-stainless-arch",
            HeaderValue::from_static(stainless_arch()),
        );
        headers.insert("x-stainless-os", HeaderValue::from_static(stainless_os()));
        if let Ok(v) = HeaderValue::from_str(&self.session_id) {
            headers.insert(consts::CLAUDE_CODE_SESSION_ID_HEADER, v);
        }
        if let Ok(v) = HeaderValue::from_str(&uuid::Uuid::new_v4().to_string()) {
            headers.insert(consts::CLIENT_REQUEST_ID_HEADER, v);
        }
        headers
    }

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
impl TransportAdapter for AnthropicSubscriptionTransport {
    fn id(&self) -> TransportId {
        TransportId::new("claude-subscription")
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
        wire_response::PROTOCOL_VERSION
    }

    async fn list_models(&self, _cred: &CredentialHandle) -> Result<Vec<ModelInfo>, ProviderError> {
        self.gate.ensure_enabled(self.experimental_opt_in)?;
        // `GET /v1/models` against an OAuth bearer token is unverified (docs/providers/claude.md);
        // returning an empty list rather than hardcoding guessed model ids (AGENTS.md §6: no
        // hardcoding unverified endpoint/model data without a live-verified spike).
        tracing::debug!(
            "claude-subscription: list_models is an unverified Phase 0 placeholder (empty)"
        );
        Ok(Vec::new())
    }

    async fn quota(
        &self,
        _cred: &CredentialHandle,
    ) -> Result<Option<QuotaSnapshot>, ProviderError> {
        self.gate.ensure_enabled(self.experimental_opt_in)?;
        #[allow(clippy::unwrap_used)] // poisoned only on a prior panic while holding the lock
        Ok(self.last_quota.lock().unwrap().clone())
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
        self.gate.ensure_enabled(self.experimental_opt_in)?;
        let model = req.model.clone();
        let transport_id = self.id();
        let body = wire_request::build_body(
            &req,
            &provider_id(),
            &transport_id,
            wire_request::RequestOptions {
                oauth_mode: true,
                stream: true,
            },
        )?;
        let resp = self.send(&body, &cred).await?;
        if let Some(snapshot) = quota::parse_unified_quota(resp.headers()) {
            #[allow(clippy::unwrap_used)]
            {
                *self.last_quota.lock().unwrap() = Some(snapshot);
            }
        }
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
            let mut translator = wire_response::Translator::new(provider_id(), transport_id.clone(), true);
            loop {
                // See `transport_api::stream` for why the fallible steps live outside the
                // `tokio::select!` branch bodies.
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
