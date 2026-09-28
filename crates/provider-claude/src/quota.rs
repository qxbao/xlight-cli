// SPDX-License-Identifier: GPL-3.0-only

//! Rate-limit / `/usage` quota header parsing (D-027), shared by both transports.
//!
//! `anthropic-api` only exposes standard per-request rate-limit headers (no quota/plan
//! endpoint), so `TransportAdapter::quota()` there always returns `Ok(None)`
//! (docs/CONTRACTS.md §6, item 4: "Ok(None) for API if nothing reliable"). `claude-subscription`
//! additionally reports rolling-window utilization via `anthropic-ratelimit-unified-*` headers
//! (**U** — docs/providers/claude.md); `transport_subscription` caches the most recent one it has
//! seen and `quota()` returns that cached snapshot.

use http::HeaderMap;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use xlightcli_protocol::RateLimitInfo;

use crate::consts;

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

fn header_u64(headers: &HeaderMap, name: &str) -> Option<u64> {
    header_str(headers, name)?.parse().ok()
}

fn header_reset_at(headers: &HeaderMap, name: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(header_str(headers, name)?, &Rfc3339).ok()
}

#[cfg(feature = "claude-subscription")]
fn header_f32(headers: &HeaderMap, name: &str) -> Option<f32> {
    header_str(headers, name)?.parse().ok()
}

/// Parses the standard `anthropic-ratelimit-requests-*` headers present on every `anthropic-api`
/// response (**M**, docs/providers/claude.md). Returns `None` when none of the headers are
/// present (e.g. a mocked/compat upstream).
pub(crate) fn parse_standard_rate_limit(headers: &HeaderMap) -> Option<RateLimitInfo> {
    let limit = header_u64(headers, consts::RATELIMIT_REQUESTS_LIMIT_HEADER);
    let remaining = header_u64(headers, consts::RATELIMIT_REQUESTS_REMAINING_HEADER);
    let reset_at = header_reset_at(headers, consts::RATELIMIT_REQUESTS_RESET_HEADER);
    if limit.is_none() && remaining.is_none() && reset_at.is_none() {
        return None;
    }
    Some(RateLimitInfo {
        limit,
        remaining,
        reset_at,
    })
}

#[cfg(feature = "claude-subscription")]
pub(crate) fn parse_unified_quota(
    headers: &HeaderMap,
) -> Option<xlightcli_protocol::QuotaSnapshot> {
    let util_5h = header_f32(headers, consts::RATELIMIT_UNIFIED_5H_UTILIZATION_HEADER);
    let util_7d = header_f32(headers, consts::RATELIMIT_UNIFIED_7D_UTILIZATION_HEADER);
    if util_5h.is_none() && util_7d.is_none() {
        return None;
    }
    let reset_5h = header_reset_at(headers, consts::RATELIMIT_UNIFIED_5H_RESET_HEADER);
    let reset_7d = header_reset_at(headers, consts::RATELIMIT_UNIFIED_7D_RESET_HEADER);
    // Surface whichever window is closer to being exhausted; both windows are kept verbatim in
    // `detail` for `/usage --verbose` (docs/CONTRACTS.md §1: `detail` is opaque to core).
    let used_percent = [util_5h, util_7d]
        .into_iter()
        .flatten()
        .fold(0.0_f32, f32::max);
    let resets_at = [reset_5h, reset_7d].into_iter().flatten().min();
    Some(xlightcli_protocol::QuotaSnapshot {
        plan: None,
        used_percent: Some(used_percent * 100.0),
        resets_at,
        detail: serde_json::json!({
            "window_5h": {"utilization": util_5h, "resets_at": reset_5h.map(|t| t.to_string())},
            "window_7d": {"utilization": util_7d, "resets_at": reset_7d.map(|t| t.to_string())},
        }),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use http::HeaderValue;
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn no_headers_returns_none() {
        assert!(parse_standard_rate_limit(&HeaderMap::new()).is_none());
    }

    #[test]
    fn parses_limit_and_remaining() {
        let mut headers = HeaderMap::new();
        headers.insert(
            consts::RATELIMIT_REQUESTS_LIMIT_HEADER,
            HeaderValue::from_static("100"),
        );
        headers.insert(
            consts::RATELIMIT_REQUESTS_REMAINING_HEADER,
            HeaderValue::from_static("42"),
        );
        let info = parse_standard_rate_limit(&headers).unwrap();
        assert_eq!(info.limit, Some(100));
        assert_eq!(info.remaining, Some(42));
    }

    #[cfg(feature = "claude-subscription")]
    #[test]
    fn parses_unified_quota_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            consts::RATELIMIT_UNIFIED_5H_UTILIZATION_HEADER,
            HeaderValue::from_static("0.5"),
        );
        headers.insert(
            consts::RATELIMIT_UNIFIED_7D_UTILIZATION_HEADER,
            HeaderValue::from_static("0.2"),
        );
        let snapshot = parse_unified_quota(&headers).unwrap();
        assert_eq!(snapshot.used_percent, Some(50.0));
    }

    #[cfg(feature = "claude-subscription")]
    #[test]
    fn missing_unified_headers_returns_none() {
        assert!(parse_unified_quota(&HeaderMap::new()).is_none());
    }
}
