// SPDX-License-Identifier: GPL-3.0-only

//! Shared HTTP plumbing for `transport_chatgpt` / `transport_api`: send-with-401-retry-once
//! (PATTERNS.md §4) and error mapping at the transport boundary (PATTERNS.md §2). Both
//! transports POST the same Responses API shape to different bases, so this is genuinely shared
//! rather than provider-specific.

use xlightcli_auth::CredentialHandle;
use xlightcli_protocol::{AuthFailure, ProviderError};

/// Sends `body` as JSON to `url` with `cred`'s auth header plus `extra_headers`. On a 401, calls
/// `cred.on_unauthorized()` once and retries exactly once (PATTERNS.md §4); any other non-2xx maps
/// through `provider::error::map_status`.
pub(crate) async fn send_with_reauth(
    http: &reqwest::Client,
    url: &str,
    extra_headers: &[(&'static str, &str)],
    body: &impl serde::Serialize,
    cred: &CredentialHandle,
) -> Result<reqwest::Response, ProviderError> {
    for attempt in 0..2 {
        let mut headers = http::HeaderMap::new();
        cred.authorize(&mut headers)
            .await
            .map_err(|_| ProviderError::Auth(AuthFailure::Rejected))?;
        headers.insert(
            http::header::ACCEPT,
            http::HeaderValue::from_static("text/event-stream"),
        );
        for (name, value) in extra_headers {
            if let Ok(value) = http::HeaderValue::from_str(value) {
                headers.insert(http::HeaderName::from_static(name), value);
            }
        }
        let resp = http
            .post(url)
            .headers(headers)
            .json(body)
            .send()
            .await
            .map_err(|e| ProviderError::Network(e.to_string()))?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
            cred.on_unauthorized()
                .await
                .map_err(|_| ProviderError::Auth(AuthFailure::Rejected))?;
            continue;
        }
        if !resp.status().is_success() {
            return Err(map_error_response(resp).await);
        }
        return Ok(resp);
    }
    Err(ProviderError::Auth(AuthFailure::Rejected))
}

/// GETs `url` with `cred`'s auth header and returns the parsed JSON body, for `quota()`.
pub(crate) async fn get_json(
    http: &reqwest::Client,
    url: &str,
    cred: &CredentialHandle,
) -> Result<serde_json::Value, ProviderError> {
    let mut headers = http::HeaderMap::new();
    cred.authorize(&mut headers)
        .await
        .map_err(|_| ProviderError::Auth(AuthFailure::Rejected))?;
    let resp = http
        .get(url)
        .headers(headers)
        .send()
        .await
        .map_err(|e| ProviderError::Network(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(map_error_response(resp).await);
    }
    resp.json()
        .await
        .map_err(|e| ProviderError::Network(format!("invalid JSON response: {e}")))
}

async fn map_error_response(resp: reqwest::Response) -> ProviderError {
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let body = resp.text().await.unwrap_or_default();
    xlightcli_provider::error::map_status(status, &headers, &body)
}
