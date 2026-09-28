// SPDX-License-Identifier: GPL-3.0-only

//! `map_status` — the single place wire status/headers/body get turned into a `ProviderError`
//! (PATTERNS.md §2: "map wire errors to `ProviderError` exactly once at the transport boundary").

use std::time::Duration;

use http::HeaderMap;
use xlightcli_protocol::{AuthFailure, ProviderError, RateLimitInfo};

/// Maximum length kept from a response body when building `ProviderError::Upstream`'s
/// `body_excerpt` (docs/PLAN.md §14: never log/store a full response body).
pub const BODY_EXCERPT_LIMIT: usize = 512;

/// Truncates `body` to `BODY_EXCERPT_LIMIT` bytes (on a UTF-8 boundary) for safe inclusion in an
/// error. Callers are responsible for redacting secrets from `body` before calling this (this
/// function only bounds the length).
pub fn body_excerpt(body: &str) -> String {
    if body.len() <= BODY_EXCERPT_LIMIT {
        return body.to_string();
    }
    let mut end = BODY_EXCERPT_LIMIT;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… (truncated)", &body[..end])
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(http::header::RETRY_AFTER)?.to_str().ok()?;
    // `Retry-After` is either a number of seconds or an HTTP-date; we only support the common
    // "seconds" form used by every provider we target.
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// Maps a non-2xx HTTP response to the appropriate `ProviderError` variant. Called exactly once
/// per response, at the transport boundary.
pub fn map_status(status: u16, headers: &HeaderMap, body: &str) -> ProviderError {
    // Redact before anything else: the excerpt ends up in errors and logs (INV-4).
    let excerpt = body_excerpt(&xlightcli_auth::redact::redact(body));
    tracing::debug!(status, body = %excerpt, "upstream error response");
    match status {
        // 401 = the credential itself was rejected (refresh + retry makes sense). 403 = the
        // credential is valid but not allowed (project, plan, client policy): keep the redacted
        // body so the user can see why instead of a generic "credential rejected".
        401 => ProviderError::Auth(AuthFailure::Rejected),
        403 => ProviderError::Upstream {
            status,
            body_excerpt: excerpt,
        },
        429 => ProviderError::RateLimited {
            retry_after: retry_after(headers),
            info: RateLimitInfo {
                limit: header_u64(headers, "x-ratelimit-limit"),
                remaining: header_u64(headers, "x-ratelimit-remaining"),
                reset_at: None,
            },
        },
        500..=599 => ProviderError::Upstream {
            status,
            body_excerpt: excerpt,
        },
        _ => ProviderError::Upstream {
            status,
            body_excerpt: excerpt,
        },
    }
}

fn header_u64(headers: &HeaderMap, name: &str) -> Option<u64> {
    headers.get(name)?.to_str().ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use http::HeaderValue;
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn maps_401_to_auth_rejected_and_403_to_upstream_with_body() {
        let headers = HeaderMap::new();
        assert!(matches!(
            map_status(401, &headers, ""),
            ProviderError::Auth(AuthFailure::Rejected)
        ));
        match map_status(
            403,
            &headers,
            r#"{"error":{"message":"project not allowed"}}"#,
        ) {
            ProviderError::Upstream {
                status: 403,
                body_excerpt,
            } => assert!(body_excerpt.contains("project not allowed")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn maps_429_with_retry_after() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::RETRY_AFTER, HeaderValue::from_static("30"));
        let err = map_status(429, &headers, "");
        match err {
            ProviderError::RateLimited { retry_after, .. } => {
                assert_eq!(retry_after, Some(Duration::from_secs(30)));
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn maps_5xx_to_upstream_with_redacted_excerpt() {
        let headers = HeaderMap::new();
        let err = map_status(503, &headers, "internal secret trace");
        match err {
            ProviderError::Upstream {
                status,
                body_excerpt,
            } => {
                assert_eq!(status, 503);
                assert_eq!(body_excerpt, "internal secret trace");
            }
            other => panic!("expected Upstream, got {other:?}"),
        }
    }

    #[test]
    fn body_excerpt_is_length_limited() {
        let long_body = "a".repeat(BODY_EXCERPT_LIMIT * 2);
        let excerpt = body_excerpt(&long_body);
        assert!(excerpt.len() < long_body.len());
        assert!(excerpt.ends_with("(truncated)"));
    }

    #[test]
    fn body_excerpt_under_limit_is_untouched() {
        assert_eq!(body_excerpt("short"), "short");
    }
}
