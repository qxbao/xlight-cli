// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli dev probe <provider> [--transport T] [--model M] "<prompt>"` — Phase 0 smoke test
//! entry point (docs/PLAN.md §19): `credential -> send prompt -> stream response`.
//!
//! Split into `probe` (the full CLI path: provider/transport lookup, `AuthBroker::credential`,
//! model resolution) and `stream_and_render` (just "given a transport + credential + model,
//! stream and print"), so the latter can be exercised directly in tests against a
//! `provider::testing::MockTransport` without needing `AuthBroker` to have real persistence yet.

use std::sync::Arc;

use futures::StreamExt as _;
use tokio_util::sync::CancellationToken;
use xlightcli_auth::{AuthError, CredentialHandle};
use xlightcli_protocol::{ModelId, ProviderId, Stability, TransportId, TurnRequest};
use xlightcli_provider::{Provider, TransportAdapter};

use crate::cmd::error::CliError;
use crate::wiring::AppContext;

pub async fn probe(
    ctx: &AppContext,
    provider_arg: &str,
    transport_arg: Option<&str>,
    model_arg: Option<&str>,
    prompt: &str,
    cancel: CancellationToken,
) -> Result<(), CliError> {
    let (transport, cred) = resolve(ctx, provider_arg, transport_arg).await?;
    let model_id = match model_arg {
        Some(m) => ModelId::new(m),
        None => first_model_id(transport, &cred).await?,
    };

    stream_and_render(transport, model_id, cred, prompt, cancel).await
}

/// `dev models`: the model catalog as the transport reports it for this account.
pub async fn models(
    ctx: &AppContext,
    provider_arg: &str,
    transport_arg: Option<&str>,
) -> Result<(), CliError> {
    let (transport, cred) = resolve(ctx, provider_arg, transport_arg).await?;
    let models = transport
        .list_models(&cred)
        .await
        .map_err(|err| CliError::other(format!("failed to list models: {err}")))?;
    if models.is_empty() {
        crate::output::info("(the transport returned no models)");
    }
    for m in models {
        let ctx_window = m
            .context_window
            .map(|w| format!("  ctx={w}"))
            .unwrap_or_default();
        let reasoning = if m.supports_reasoning {
            "  reasoning"
        } else {
            ""
        };
        crate::output::info(&format!(
            "{:<40} {}{ctx_window}{reasoning}",
            m.id, m.display_name
        ));
    }
    Ok(())
}

/// `dev quota`: plan/quota snapshot (`TransportAdapter::quota`, D-027).
///
/// **D-030 accepted gap, now closed:** the summary line used to always echo
/// `QuotaSnapshot::used_percent`/`resets_at` verbatim — but each provider's `quota.rs` picks a
/// single "primary" bucket to fill those fields with its own heuristic (e.g. `provider-agy` always
/// prefers the Gemini 5h window over weekly, `docs/providers/agy.md`), which can disagree with
/// which bucket is actually most exhausted (observed: `used: 0.0%` from a fresh 5h window while
/// the weekly window sat at ~8%). This now walks `QuotaSnapshot::detail` itself
/// ([`extract_buckets`]) to print one line per bucket found and picks the summary from whichever
/// bucket has the *highest* `used_percent`, independent of the provider's own bucket choice. Falls
/// back to the top-level `used_percent`/`resets_at` fields when `detail` has no recognizable
/// bucket shape (e.g. a transport with no detail at all).
pub async fn quota(
    ctx: &AppContext,
    provider_arg: &str,
    transport_arg: Option<&str>,
) -> Result<(), CliError> {
    let (transport, cred) = resolve(ctx, provider_arg, transport_arg).await?;
    let snapshot = transport
        .quota(&cred)
        .await
        .map_err(|err| CliError::other(format!("failed to fetch quota: {err}")))?;
    let Some(q) = snapshot else {
        crate::output::info(&format!(
            "{} does not expose quota information",
            transport.id()
        ));
        return Ok(());
    };
    crate::output::info(&format!(
        "plan:         {}",
        q.plan.as_deref().unwrap_or("-")
    ));

    let buckets = extract_buckets(&q.detail);
    if buckets.is_empty() {
        crate::output::info(&format!(
            "used:         {}",
            q.used_percent
                .map_or("-".to_string(), |p| format!("{p:.1}%"))
        ));
        crate::output::info(&format!(
            "resets at:    {}",
            q.resets_at.map_or("-".to_string(), |t| t.to_string())
        ));
    } else {
        crate::output::info("buckets:");
        for bucket in &buckets {
            crate::output::info(&format!(
                "  {:<16} {:<12} used {:>5.1}%  resets {}",
                bucket.group,
                bucket.window,
                bucket.used_percent,
                bucket.resets_at.as_deref().unwrap_or("-"),
            ));
        }
        // `total_cmp` (not `partial_cmp`) since `used_percent` is always a finite f32 here (every
        // producer clamps it), so there is no `NaN` case to worry about either way. `if let`
        // instead of `.expect()` even though `buckets` is non-empty in this branch (D-016: no
        // `expect()` outside tests, even for a "can't actually happen" case).
        if let Some(most_used) = buckets
            .iter()
            .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))
        {
            crate::output::info(&format!(
                "used:         {:.1}%  (most-used bucket: {}/{})",
                most_used.used_percent, most_used.group, most_used.window
            ));
            crate::output::info(&format!(
                "resets at:    {}",
                most_used.resets_at.as_deref().unwrap_or("-")
            ));
        }
    }

    if !q.detail.is_null() {
        let detail = serde_json::to_string_pretty(&q.detail).unwrap_or_default();
        crate::output::info(&format!(
            "detail:\n{}",
            xlightcli_auth::redact::redact(&detail)
        ));
    }
    Ok(())
}

/// One quota bucket found by [`extract_buckets`]: a group label (best-effort, e.g. `"Gemini"`), a
/// window label (e.g. `"5h"`/`"weekly"`/`"primary"`), how much of it is used, and (if present) a
/// human-readable reset time/duration.
#[derive(Debug, Clone, PartialEq)]
struct QuotaBucket {
    group: String,
    window: String,
    used_percent: f32,
    resets_at: Option<String>,
}

fn bucket_used_percent(obj: &serde_json::Map<String, serde_json::Value>) -> Option<f32> {
    if let Some(fraction) = obj
        .get("remainingFraction")
        .and_then(serde_json::Value::as_f64)
    {
        return Some(((1.0 - fraction) * 100.0).clamp(0.0, 100.0) as f32);
    }
    if let Some(percent) = obj
        .get("remainingPercentage")
        .and_then(serde_json::Value::as_f64)
    {
        return Some((100.0 - percent).clamp(0.0, 100.0) as f32);
    }
    if let Some(utilization) = obj.get("utilization").and_then(serde_json::Value::as_f64) {
        return Some((utilization * 100.0).clamp(0.0, 100.0) as f32);
    }
    if let Some(used) = obj.get("used_percent").and_then(serde_json::Value::as_f64) {
        return Some(used.clamp(0.0, 100.0) as f32);
    }
    None
}

fn bucket_reset_label(obj: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    if let Some(s) = obj.get("resetTime").and_then(serde_json::Value::as_str) {
        return Some(s.to_string());
    }
    if let Some(s) = obj.get("resets_at").and_then(serde_json::Value::as_str) {
        return Some(s.to_string());
    }
    if let Some(secs) = obj
        .get("resets_in_seconds")
        .and_then(serde_json::Value::as_i64)
    {
        return Some(format!("in {secs}s"));
    }
    None
}

/// A best-effort human label for `obj`, preferring an explicit name-shaped field over the raw JSON
/// key it was found under (`fallback`).
fn bucket_label(obj: &serde_json::Map<String, serde_json::Value>, fallback: &str) -> String {
    for key in ["displayName", "window", "bucketId", "description"] {
        if let Some(s) = obj.get(key).and_then(serde_json::Value::as_str)
            && !s.is_empty()
        {
            return s.to_string();
        }
    }
    fallback.to_string()
}

/// Walks `detail` looking for bucket-shaped objects — anything with a recognizable
/// remaining/utilization/used-percent field — and collects one [`QuotaBucket`] per match, tagging
/// each with a best-effort `(group, window)` label pair from its surrounding JSON.
///
/// Deliberately shape-agnostic rather than hardcoding one provider's `detail` schema
/// (docs/commands.md §6 — `provider-agy`'s `groups[].buckets[]`, `provider-claude`'s
/// `window_5h`/`window_7d`, `provider-codex`'s `primary`/`rate_limits.primary` are three different
/// shapes): `QuotaSnapshot::detail` is documented as opaque to core (D-027), so this recognizes
/// bucket-like objects by field name rather than by provider, and stops recursing into an object
/// as soon as it matches (a bucket's own fields are never themselves nested buckets).
fn extract_buckets(detail: &serde_json::Value) -> Vec<QuotaBucket> {
    fn walk(value: &serde_json::Value, group: &str, path_key: &str, out: &mut Vec<QuotaBucket>) {
        match value {
            serde_json::Value::Object(obj) => {
                if let Some(used_percent) = bucket_used_percent(obj) {
                    out.push(QuotaBucket {
                        group: group.to_string(),
                        window: bucket_label(obj, path_key),
                        used_percent,
                        resets_at: bucket_reset_label(obj),
                    });
                    return;
                }
                let next_group = bucket_label(obj, group);
                for (key, child) in obj {
                    walk(child, &next_group, key, out);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, group, path_key, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(detail, "-", "-", &mut out);
    out
}

/// Shared lookup for `dev` commands: provider → transport (default: first stable) → stored
/// credential.
async fn resolve<'a>(
    ctx: &'a AppContext,
    provider_arg: &str,
    transport_arg: Option<&str>,
) -> Result<(&'a Arc<dyn TransportAdapter>, CredentialHandle), CliError> {
    let provider_id = ProviderId::new(provider_arg);
    let provider = ctx.providers.get(&provider_id).ok_or_else(|| {
        CliError::invalid_input(format!(
            "unknown provider {provider_arg:?} (registered: {})",
            joined_provider_ids(ctx)
        ))
    })?;

    let transport_id = match transport_arg {
        Some(t) => TransportId::new(t),
        None => default_transport_id(provider)?,
    };
    let transport = provider.transport(&transport_id).ok_or_else(|| {
        CliError::invalid_input(format!(
            "provider {provider_id} has no transport {transport_id} (available: {})",
            provider
                .transports()
                .iter()
                .map(|t| t.id().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })?;

    let cred = ctx
        .auth
        .credential(&provider_id, &transport_id)
        .await
        .map_err(|err| credential_error_to_cli(&provider_id, &transport_id, err))?;
    Ok((transport, cred))
}

fn joined_provider_ids(ctx: &AppContext) -> String {
    ctx.providers
        .ids()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn default_transport_id(provider: &Arc<dyn Provider>) -> Result<TransportId, CliError> {
    provider
        .transports()
        .iter()
        .find(|t| t.stability() == Stability::Stable)
        .or_else(|| provider.transports().first())
        .map(|t| t.id())
        .ok_or_else(|| {
            CliError::invalid_input(format!(
                "provider {} has no transports registered",
                provider.id()
            ))
        })
}

async fn first_model_id(
    transport: &Arc<dyn TransportAdapter>,
    cred: &CredentialHandle,
) -> Result<ModelId, CliError> {
    let models = transport
        .list_models(cred)
        .await
        .map_err(|err| CliError::other(format!("failed to list models: {err}")))?;
    models.into_iter().next().map(|m| m.id).ok_or_else(|| {
        CliError::invalid_input("no --model given and the transport returned no models".to_string())
    })
}

fn credential_error_to_cli(
    provider: &ProviderId,
    transport: &TransportId,
    err: AuthError,
) -> CliError {
    match err {
        AuthError::NotLoggedIn { .. } => CliError::invalid_input(format!(
            "not logged in to {provider} ({transport}); run `xlightcli auth login {provider} \
             --transport {transport}` first"
        )),
        other => CliError::other(format!(
            "could not obtain credential for {provider}/{transport}: {other}"
        )),
    }
}

/// Streams one turn and prints every event via `crate::output::render_event`. `Ctrl-C` cancels
/// `cancel`, which this loop checks with `biased` priority (PATTERNS.md §3) so cancellation is
/// never starved by a fast-producing stream.
pub async fn stream_and_render(
    transport: &Arc<dyn TransportAdapter>,
    model: ModelId,
    cred: CredentialHandle,
    prompt: &str,
    cancel: CancellationToken,
) -> Result<(), CliError> {
    let req = TurnRequest::simple(model, prompt);
    let mut stream = transport
        .stream(req, cred, cancel.clone())
        .await
        .map_err(|err| CliError::other(format!("stream failed to start: {err}")))?;

    let mut produced_output = false;
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => {
                return Err(fail(produced_output, "cancelled".to_string()));
            }
            next = stream.next() => match next {
                None => break,
                Some(Ok(event)) => {
                    produced_output |= crate::output::render_event(&event);
                }
                Some(Err(err)) => {
                    return Err(fail(produced_output, format!("stream error: {err}")));
                }
            },
        }
    }
    Ok(())
}

fn fail(produced_output: bool, msg: String) -> CliError {
    // Logged (not just returned) so operators can see stream failures in
    // `$XDG_STATE_HOME/xlightcli/logs/xlightcli.log`; `crate::logging`'s redacting writer scrubs
    // the formatted line regardless of what `msg` embeds. `CliError::other`/`partial` redact `msg`
    // again on the way out (INV-4 defense in depth) — `msg` can embed an upstream body excerpt
    // (`ProviderError::Upstream`/`Network`'s `Display`).
    tracing::error!(%msg, "dev probe: stream error");
    if produced_output {
        CliError::partial(msg)
    } else {
        CliError::other(msg)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;
    use serde_json::json;
    use xlightcli_protocol::{AgentEvent, AuthKind, Message, Role, StopReason, Usage};
    use xlightcli_provider::testing::MockProvider;

    use super::*;

    /// Regression test for the D-030 gap: a fresh (0%-used) 5h bucket must not hide a
    /// significantly more exhausted weekly bucket in the summary line.
    #[test]
    fn most_used_bucket_wins_even_when_it_is_not_the_first_one() {
        let detail = json!({
            "groups": [{
                "displayName": "Gemini",
                "buckets": [
                    {"window": "5h", "remainingFraction": 1.0},
                    {"window": "weekly", "remainingFraction": 0.92, "resetTime": "2026-10-01T00:00:00Z"}
                ]
            }]
        });
        let buckets = extract_buckets(&detail);
        assert_eq!(buckets.len(), 2);
        let most_used = buckets
            .iter()
            .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))
            .unwrap();
        assert_eq!(most_used.window, "weekly");
        assert!((most_used.used_percent - 8.0).abs() < 0.01);
        assert_eq!(most_used.resets_at.as_deref(), Some("2026-10-01T00:00:00Z"));
    }

    #[test]
    fn extract_buckets_reads_agy_groups_and_buckets_shape() {
        let detail = json!({
            "groups": [{
                "displayName": "Claude",
                "buckets": [{"window": "weekly", "remainingPercentage": 40.0}]
            }]
        });
        let buckets = extract_buckets(&detail);
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].group, "Claude");
        assert_eq!(buckets[0].window, "weekly");
        assert_eq!(buckets[0].used_percent, 60.0);
    }

    #[test]
    fn extract_buckets_reads_claude_window_shape() {
        let detail = json!({
            "window_5h": {"utilization": 0.5, "resets_at": "2026-10-01T00:00:00Z"},
            "window_7d": {"utilization": 0.2, "resets_at": serde_json::Value::Null},
        });
        let mut buckets = extract_buckets(&detail);
        buckets.sort_by(|a, b| a.window.cmp(&b.window));
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].window, "window_5h");
        assert_eq!(buckets[0].used_percent, 50.0);
        assert_eq!(buckets[1].window, "window_7d");
        assert_eq!(buckets[1].used_percent, 20.0);
    }

    #[test]
    fn extract_buckets_reads_codex_primary_shape() {
        let detail = json!({
            "plan_type": "plus",
            "primary": {"used_percent": 42.5, "resets_in_seconds": 3600}
        });
        let buckets = extract_buckets(&detail);
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].window, "primary");
        assert_eq!(buckets[0].used_percent, 42.5);
        assert_eq!(buckets[0].resets_at.as_deref(), Some("in 3600s"));
    }

    #[test]
    fn extract_buckets_on_shapeless_detail_is_empty() {
        assert!(extract_buckets(&json!({"foo": "bar"})).is_empty());
        assert!(extract_buckets(&serde_json::Value::Null).is_empty());
    }

    fn test_account() -> xlightcli_auth::AccountInfo {
        xlightcli_auth::AccountInfo {
            provider: ProviderId::new("mock"),
            transport: TransportId::new("mock-transport"),
            account_id: "acc-1".into(),
            label: None,
            auth_kind: AuthKind::ApiKey,
            metadata: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn stream_and_render_succeeds_on_a_clean_completed_script() {
        let provider = MockProvider::scripted(
            "mock",
            "mock-transport",
            vec![
                Ok(AgentEvent::TurnStarted {
                    model: ModelId::new("mock-model"),
                }),
                Ok(AgentEvent::TextDelta {
                    index: 0,
                    text: "hi".into(),
                }),
                Ok(AgentEvent::Completed {
                    message: Message {
                        role: Role::Assistant,
                        content: vec![],
                    },
                    stop: StopReason::EndTurn,
                    usage: Usage::default(),
                }),
            ],
        );
        let transport = provider.transports()[0].clone();
        let cred = CredentialHandle::for_tests(test_account(), "unused-token");

        let result = stream_and_render(
            &transport,
            ModelId::new("mock-model"),
            cred,
            "hi",
            CancellationToken::new(),
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn stream_error_before_any_output_is_other_not_partial() {
        let provider = MockProvider::scripted(
            "mock",
            "mock-transport",
            vec![Err(xlightcli_protocol::ProviderError::Network(
                "boom".into(),
            ))],
        );
        let transport = provider.transports()[0].clone();
        let cred = CredentialHandle::for_tests(test_account(), "unused-token");

        let err = stream_and_render(
            &transport,
            ModelId::new("mock-model"),
            cred,
            "hi",
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.exit_code_number(), 1);
    }

    #[tokio::test]
    async fn stream_error_after_output_is_partial() {
        let provider = MockProvider::scripted(
            "mock",
            "mock-transport",
            vec![
                Ok(AgentEvent::TextDelta {
                    index: 0,
                    text: "partial".into(),
                }),
                Err(xlightcli_protocol::ProviderError::Network("boom".into())),
            ],
        );
        let transport = provider.transports()[0].clone();
        let cred = CredentialHandle::for_tests(test_account(), "unused-token");

        let err = stream_and_render(
            &transport,
            ModelId::new("mock-model"),
            cred,
            "hi",
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.exit_code_number(), 3);
    }

    /// Secret sentinel test (docs/PLAN.md §16, PATTERNS.md §13): a `CredentialHandle` carrying a
    /// sentinel secret must never leak into stdout/stderr/log even when the stream errors out.
    /// `CredentialHandle` never exposes the secret to callers (INV-4) and the mock transport never
    /// touches it either, so this mostly documents/guards the invariant at the `dev probe` call
    /// boundary; it also exercises `crate::logging`'s redacting writer end to end.
    #[tokio::test(flavor = "current_thread")]
    async fn credential_secret_never_appears_in_probe_output_or_log() {
        const SENTINEL: &str = "XLC-SENTINEL-SECRET-abcdefgh1234";
        let temp = tempfile::tempdir().unwrap();
        // Scoped (not global) subscriber: `[workspace.lints] unsafe_code = "forbid"` rules out
        // `std::env::set_var` (edition 2024 makes it `unsafe`), and racing every other test in
        // this binary for the process-global `try_init` slot would make this flaky. See
        // `logging::scoped_for_test`.
        let _log_guard = crate::logging::scoped_for_test(temp.path());

        let provider = MockProvider::scripted(
            "mock",
            "mock-transport",
            vec![
                Ok(AgentEvent::TextDelta {
                    index: 0,
                    text: "hello".into(),
                }),
                Err(xlightcli_protocol::ProviderError::Upstream {
                    status: 401,
                    body_excerpt: format!("Bearer {SENTINEL}"),
                }),
            ],
        );
        let transport = provider.transports()[0].clone();
        let cred = CredentialHandle::for_tests(test_account(), SENTINEL);

        let err = stream_and_render(
            &transport,
            ModelId::new("mock-model"),
            cred,
            "hi",
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        let rendered = err.to_string();
        assert!(
            !rendered.contains(SENTINEL),
            "sentinel leaked into CliError message: {rendered}"
        );

        let log_path = temp.path().join("xlightcli.log");
        let contents = std::fs::read_to_string(&log_path)
            .unwrap_or_else(|e| panic!("expected a log line at {}: {e}", log_path.display()));
        // `tracing` callsite interest is process-wide while this subscriber is scoped to one
        // test thread; another parallel test can change the cached interest before this event.
        // The writer's own unit test verifies redaction when a line is written. Here the
        // invariant is that any captured line must never expose the sentinel.
        assert!(
            !contents.contains(SENTINEL),
            "sentinel leaked into log file {}: {contents}",
            log_path.display()
        );
    }
}
