// SPDX-License-Identifier: GPL-3.0-only

//! `pub(crate)` Gemini / Cloud Code Assist wire types + translator, shared by `transport_gemini`
//! and `transport_antigravity`. No wire type may appear in a `pub` signature (INV-3).
//!
//! `request` builds the flat Gemini `generateContent` body (and, for Antigravity, the Cloud Code
//! Assist envelope around it) from a canonical `TurnRequest`. `response::Translator` is the pure
//! SSE→`AgentEvent` state machine (PATTERNS.md §5/§6). This module also hosts `run_stream`, the
//! HTTP+SSE plumbing shared by both transports (PATTERNS.md §6 canonical event-stream shape) so
//! `transport_*.rs` stays focused on endpoint/header/gating concerns (PATTERNS.md §5).

mod request;
mod response;

use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use xlightcli_auth::CredentialHandle;
use xlightcli_protocol::{
    AgentEvent, ModelId, ProtocolVersion, ProviderError, ProviderId, TransportId,
};
use xlightcli_provider::EventStream;

pub(crate) use request::build_generate_content_body;
// Only the `antigravity` transport (feature-gated) needs these; gating the re-export itself
// (rather than the call sites) keeps the default (no `antigravity-subscription`) build free of
// "unused import" warnings, which `cargo clippy --all-targets -- -D warnings` treats as errors.
#[cfg(feature = "antigravity-subscription")]
pub(crate) use request::{antigravity_session_id, build_antigravity_envelope, new_request_id};
pub(crate) use response::Translator;

/// Sends one JSON POST with the credential's auth header, retrying **exactly once** after a
/// single-flight `on_unauthorized()` refresh on a `401` (PATTERNS.md §4). Any other status is
/// returned as-is for the caller to map via `provider::map_status`.
pub(crate) async fn post_json_with_retry(
    http: &reqwest::Client,
    url: &str,
    base_headers: http::HeaderMap,
    body: &serde_json::Value,
    cred: &CredentialHandle,
) -> Result<reqwest::Response, ProviderError> {
    let mut headers = base_headers.clone();
    cred.authorize(&mut headers)
        .await
        .map_err(crate::map_auth_error)?;
    let response = http
        .post(url)
        .headers(headers)
        .json(body)
        .send()
        .await
        .map_err(|e| ProviderError::Network(e.to_string()))?;
    if response.status() != http::StatusCode::UNAUTHORIZED {
        return Ok(response);
    }
    cred.on_unauthorized()
        .await
        .map_err(crate::map_auth_error)?;
    let mut retry_headers = base_headers;
    cred.authorize(&mut retry_headers)
        .await
        .map_err(crate::map_auth_error)?;
    http.post(url)
        .headers(retry_headers)
        .json(body)
        .send()
        .await
        .map_err(|e| ProviderError::Network(e.to_string()))
}

/// Shared streaming plumbing (PATTERNS.md §6 canonical shape): POST the wire body, map non-2xx via
/// `provider::map_status`, then translate the SSE body through [`Translator`]. Exactly one
/// `TurnStarted` is yielded first and exactly one `Completed` last on success.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_stream(
    http: reqwest::Client,
    url: String,
    base_headers: http::HeaderMap,
    body: serde_json::Value,
    cred: CredentialHandle,
    provider: ProviderId,
    transport: TransportId,
    wrapped: bool,
    protocol_version: ProtocolVersion,
    model: ModelId,
    cancel: CancellationToken,
) -> Result<EventStream, ProviderError> {
    let response = post_json_with_retry(&http, &url, base_headers, &body, &cred).await?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let resp_headers = response.headers().clone();
        let text = response.text().await.unwrap_or_default();
        return Err(xlightcli_provider::map_status(status, &resp_headers, &text));
    }
    let byte_stream = Box::pin(
        response
            .bytes_stream()
            .map(|chunk| chunk.map_err(|e| ProviderError::Network(e.to_string()))),
    );
    let mut sse = xlightcli_provider::parse_sse(byte_stream);
    // `async_stream::stream!` (not `try_stream!`, PATTERNS.md §6's snippet is aspirational
    // pseudocode — `?` inside `try_stream!` does not type-check against this crate's pinned
    // `async-stream` version): every item is yielded already wrapped in `Ok`/`Err`, and an error
    // ends the stream via an explicit `return` right after, matching the "errors travel as
    // `Result::Err` items" contract (crate::protocol::error module doc).
    Ok(Box::pin(async_stream::stream! {
        yield Ok(AgentEvent::TurnStarted { model });
        let mut translator = Translator::new(provider, transport, wrapped, protocol_version);
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    yield Err(ProviderError::Cancelled);
                    return;
                }
                next = sse.next() => match next {
                    Some(Ok(event)) => match translator.feed(event) {
                        Ok(outs) => {
                            for out in outs {
                                yield Ok(out);
                            }
                        }
                        Err(e) => {
                            yield Err(e);
                            return;
                        }
                    },
                    Some(Err(e)) => {
                        yield Err(e);
                        return;
                    }
                    None => break,
                },
            }
        }
        match translator.finish() {
            Ok(ev) => yield Ok(ev),
            Err(e) => yield Err(e),
        }
    }))
}
