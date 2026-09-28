// SPDX-License-Identifier: GPL-3.0-only
// Portions derived from OpenCodex (MIT) @ 3cc34e1181926b64331490fdcfee162ffb62fe73:
// src/providers/quota/antigravity.ts
// See THIRD_PARTY.md.

//! `/usage` quota snapshot parsing for Antigravity (D-027).
//!
//! Ported (shape, not code) from OpenCodex (MIT) `@ 3cc34e1181926b64331490fdcfee162ffb62fe73`
//! `src/providers/quota/antigravity.ts` (`parseAntigravityQuotaSummary`,
//! `antigravityWindowsFromModels`): primary source is `v1internal:retrieveUserQuotaSummary`
//! (`groups[].buckets[]`, 5h/weekly windows, `remainingFraction`/`remainingPercentage`,
//! `resetTime`), falling back to `v1internal:fetchAvailableModels`' per-model `quotaInfo`. See
//! `THIRD_PARTY.md`.
//!
//! Simplified vs. the ported source: `xlightcli_protocol::QuotaSnapshot` is intentionally a flat
//! `{plan, used_percent, resets_at, detail}` shape (Phase 0), not OpenCodex's multi-window
//! `customWindows` list — this picks one primary window (Gemini 5h bucket, else the first bucket
//! found) for `used_percent`/`resets_at` and keeps the full parsed body in `detail` for
//! `/usage --verbose`.

use serde_json::Value;
use time::OffsetDateTime;
use xlightcli_protocol::QuotaSnapshot;

fn used_percent(bucket: &Value) -> Option<f32> {
    let target = bucket.get("remaining").unwrap_or(bucket);
    let remaining_percent = target
        .get("remainingFraction")
        .and_then(Value::as_f64)
        .map(|f| f * 100.0)
        .or_else(|| target.get("remainingPercentage").and_then(Value::as_f64));
    remaining_percent.map(|remaining| (100.0 - remaining).clamp(0.0, 100.0) as f32)
}

fn reset_time(bucket: &Value) -> Option<OffsetDateTime> {
    let raw = bucket.get("resetTime")?.as_str()?;
    OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339).ok()
}

/// Parses a `v1internal:retrieveUserQuotaSummary` response body. Returns `None` if it has no
/// `groups` at all (caller falls back to [`parse_from_models`]) — never guesses at a shape it
/// can't recognize (PATTERNS.md §5).
pub(crate) fn parse_quota_summary(body: &Value) -> Option<QuotaSnapshot> {
    let groups = body.get("groups")?.as_array()?;
    let mut fallback: Option<(f32, Option<OffsetDateTime>)> = None;
    for group in groups {
        let name = format!(
            "{} {}",
            group
                .get("displayName")
                .and_then(Value::as_str)
                .unwrap_or(""),
            group
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or(""),
        )
        .to_lowercase();
        let is_gemini = name.contains("gemini");
        let Some(buckets) = group.get("buckets").and_then(Value::as_array) else {
            continue;
        };
        for bucket in buckets {
            let window = format!(
                "{} {} {}",
                bucket.get("window").and_then(Value::as_str).unwrap_or(""),
                bucket.get("bucketId").and_then(Value::as_str).unwrap_or(""),
                bucket
                    .get("displayName")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
            )
            .to_lowercase();
            let is_5h = window.contains("5h") || window.contains("five");
            let Some(percent) = used_percent(bucket) else {
                continue;
            };
            let resets_at = reset_time(bucket);
            if is_gemini && is_5h {
                return Some(QuotaSnapshot {
                    plan: None,
                    used_percent: Some(percent),
                    resets_at,
                    detail: body.clone(),
                });
            }
            fallback.get_or_insert((percent, resets_at));
        }
    }
    fallback.map(|(percent, resets_at)| QuotaSnapshot {
        plan: None,
        used_percent: Some(percent),
        resets_at,
        detail: body.clone(),
    })
}

/// Fallback parse of `v1internal:fetchAvailableModels`' per-model `quotaInfo`/`quotaInfos`, used
/// when [`parse_quota_summary`] finds nothing usable.
pub(crate) fn parse_from_models(body: &Value) -> Option<QuotaSnapshot> {
    let models = body.get("models")?.as_object()?;
    for info in models.values() {
        let quota_info = match info.get("quotaInfo") {
            Some(Value::Array(arr)) => arr.first(),
            Some(obj @ Value::Object(_)) => Some(obj),
            _ => info
                .get("quotaInfos")
                .and_then(Value::as_array)
                .and_then(|arr| arr.first()),
        };
        if let Some(quota_info) = quota_info
            && let Some(percent) = used_percent(quota_info)
        {
            return Some(QuotaSnapshot {
                plan: None,
                used_percent: Some(percent),
                resets_at: reset_time(quota_info),
                detail: body.clone(),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_gemini_five_hour_bucket_as_primary() {
        let body = json!({
            "groups": [{
                "displayName": "Gemini",
                "buckets": [
                    {"window": "5h", "remainingFraction": 0.75, "resetTime": "2026-09-28T12:00:00Z"},
                    {"window": "weekly", "remainingFraction": 0.9}
                ]
            }]
        });
        let snapshot = parse_quota_summary(&body).unwrap();
        assert_eq!(snapshot.used_percent, Some(25.0));
        assert!(snapshot.resets_at.is_some());
    }

    #[test]
    fn falls_back_to_first_bucket_when_no_gemini_five_hour_window() {
        let body = json!({
            "groups": [{
                "displayName": "Claude",
                "buckets": [{"window": "weekly", "remainingPercentage": 40.0}]
            }]
        });
        let snapshot = parse_quota_summary(&body).unwrap();
        assert_eq!(snapshot.used_percent, Some(60.0));
    }

    #[test]
    fn no_groups_key_returns_none() {
        assert!(parse_quota_summary(&json!({"foo": "bar"})).is_none());
    }

    #[test]
    fn parse_from_models_reads_per_model_quota_info() {
        let body = json!({
            "models": {
                "gemini-3.1-pro": {"quotaInfo": {"remainingFraction": 0.5}}
            }
        });
        let snapshot = parse_from_models(&body).unwrap();
        assert_eq!(snapshot.used_percent, Some(50.0));
    }
}
