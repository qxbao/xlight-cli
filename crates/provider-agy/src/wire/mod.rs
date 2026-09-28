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
        return Err(map_google_status(status, &resp_headers, &text));
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

/// Maps a non-2xx Google response. Recognizes `google.rpc.ErrorInfo` reason
/// `VALIDATION_REQUIRED` (the account must be verified in a browser) and surfaces the full
/// verification link instead of a truncated body excerpt; everything else goes through
/// `provider::map_status`.
pub(crate) fn map_google_status(
    status: u16,
    headers: &http::HeaderMap,
    body: &str,
) -> xlightcli_protocol::ProviderError {
    if status == 429 {
        let excerpt = xlightcli_provider::body_excerpt(&xlightcli_auth::redact::redact(body));
        tracing::debug!(status, body = %excerpt, "google rate limit response");
        if let xlightcli_protocol::ProviderError::RateLimited { retry_after, info } =
            xlightcli_provider::map_status(status, headers, body)
        {
            return xlightcli_protocol::ProviderError::RateLimited {
                retry_after: retry_after.or_else(|| google_retry_delay(body)),
                info,
            };
        }
    }
    if let Some(msg) = validation_required_message(body) {
        return xlightcli_protocol::ProviderError::Upstream {
            status,
            body_excerpt: msg,
        };
    }
    xlightcli_provider::map_status(status, headers, body)
}

/// Reads `google.rpc.RetryInfo.retryDelay` (e.g. `"37s"` or `"1.5s"`) from an error body.
fn google_retry_delay(body: &str) -> Option<std::time::Duration> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    json.get("error")?
        .get("details")?
        .as_array()?
        .iter()
        .find_map(|d| d.get("retryDelay").and_then(serde_json::Value::as_str))
        .and_then(|s| s.strip_suffix('s'))
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|secs| secs.is_finite() && *secs >= 0.0)
        .map(std::time::Duration::from_secs_f64)
}

fn validation_required_message(body: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let error = json.get("error")?;
    let info = error.get("details")?.as_array()?.iter().find(|d| {
        d.get("reason").and_then(serde_json::Value::as_str) == Some("VALIDATION_REQUIRED")
    })?;
    let message = error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Google requires account verification");
    let url = info
        .get("metadata")
        .and_then(|m| m.get("validation_url"))
        .and_then(serde_json::Value::as_str)
        .filter(|u| u.starts_with("https://"));
    Some(match url {
        Some(url) => format!(
            "{message} Google requires you to verify this account before it can be used here. \
             Open this link in a browser signed in as the same Google account, then retry:\n  {url}"
        ),
        None => format!("{message} (Google reason: VALIDATION_REQUIRED)"),
    })
}

#[cfg(test)]
mod google_status_tests {
    use super::map_google_status;
    use xlightcli_protocol::ProviderError;

    #[test]
    fn validation_required_surfaces_full_link() {
        let body = r#"{"error":{"code":403,"message":"Verify your account to continue.","status":"PERMISSION_DENIED","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"VALIDATION_REQUIRED","domain":"cloudcode-pa.googleapis.com","metadata":{"validation_url":"https://accounts.google.com/signin/continue?sarp=1&scc=1&plt=AKgnsbXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX&end=1"}}]}}"#;
        match map_google_status(403, &http::HeaderMap::new(), body) {
            ProviderError::Upstream {
                status: 403,
                body_excerpt,
            } => {
                assert!(
                    body_excerpt.contains("&end=1"),
                    "link must not be truncated"
                );
                assert!(body_excerpt.contains("Verify your account"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn rate_limit_reads_google_retry_delay() {
        let body = r#"{"error":{"code":429,"message":"Resource has been exhausted","status":"RESOURCE_EXHAUSTED","details":[{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"37s"}]}}"#;
        match map_google_status(429, &http::HeaderMap::new(), body) {
            ProviderError::RateLimited { retry_after, .. } => {
                assert_eq!(retry_after, Some(std::time::Duration::from_secs(37)));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn other_403_falls_back_to_generic_mapping() {
        let body =
            r#"{"error":{"code":403,"message":"nope","details":[{"reason":"SOMETHING_ELSE"}]}}"#;
        assert!(matches!(
            map_google_status(403, &http::HeaderMap::new(), body),
            ProviderError::Upstream { status: 403, .. }
        ));
    }
}
