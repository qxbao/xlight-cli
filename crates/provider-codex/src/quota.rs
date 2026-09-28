// SPDX-License-Identifier: GPL-3.0-only

//! `/usage` quota snapshot parsing for Codex (D-027).
//!
//! **U**: the exact `GET {chatgpt_backend_base}/wham/usage` response shape is not independently
//! verified (docs/providers/codex.md lists candidate header/event names but not a confirmed body
//! schema). Parsed leniently: every field is best-effort `Option`, and the full raw body is kept
//! in `QuotaSnapshot::detail` so `/usage --verbose` can still show it even if this parser's
//! guesses about field names are wrong.

use time::OffsetDateTime;
use xlightcli_protocol::QuotaSnapshot;

pub(crate) fn parse_usage_snapshot(value: &serde_json::Value) -> QuotaSnapshot {
    let plan = value
        .get("plan_type")
        .and_then(|v| v.as_str())
        .or_else(|| value.get("plan").and_then(|v| v.as_str()))
        .map(str::to_string);
    let used_percent = primary_field(value, "used_percent")
        .and_then(|v| v.as_f64())
        .map(|f| f as f32);
    let resets_at = primary_field(value, "resets_in_seconds")
        .and_then(|v| v.as_i64())
        .map(|secs| OffsetDateTime::now_utc() + time::Duration::seconds(secs));
    QuotaSnapshot {
        plan,
        used_percent,
        resets_at,
        detail: value.clone(),
    }
}

/// Tries a few candidate shapes for the "primary rate-limit window" object: a bare `primary` key,
/// or one nested under `rate_limits` (matches the `codex.rate_limits` SSE event shape).
fn primary_field<'a>(value: &'a serde_json::Value, field: &str) -> Option<&'a serde_json::Value> {
    value
        .pointer(&format!("/primary/{field}"))
        .or_else(|| value.pointer(&format!("/rate_limits/primary/{field}")))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn parses_plan_and_primary_window() {
        let value = serde_json::json!({
            "plan_type": "plus",
            "primary": {"used_percent": 42.5, "resets_in_seconds": 3600}
        });
        let snapshot = parse_usage_snapshot(&value);
        assert_eq!(snapshot.plan.as_deref(), Some("plus"));
        assert_eq!(snapshot.used_percent, Some(42.5));
        assert!(snapshot.resets_at.is_some());
        assert_eq!(snapshot.detail, value);
    }

    #[test]
    fn parses_rate_limits_nested_shape() {
        let value = serde_json::json!({
            "rate_limits": {"primary": {"used_percent": 10.0}}
        });
        let snapshot = parse_usage_snapshot(&value);
        assert_eq!(snapshot.used_percent, Some(10.0));
    }

    #[test]
    fn missing_fields_degrade_to_none_rather_than_failing() {
        let snapshot = parse_usage_snapshot(&serde_json::json!({}));
        assert_eq!(snapshot.plan, None);
        assert_eq!(snapshot.used_percent, None);
        assert_eq!(snapshot.resets_at, None);
    }
}
