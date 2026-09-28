// SPDX-License-Identifier: GPL-3.0-only

#![cfg(feature = "antigravity-subscription")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Black-box `wiremock` tests for the `antigravity` transport (PATTERNS.md §13), exercised only
//! through the crate's public surface. Compiled only under `antigravity-subscription`. No real
//! Google endpoint is contacted.

use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};
use xlightcli_auth::{AccountInfo, CredentialHandle};
use xlightcli_config::ExperimentalFlags;
use xlightcli_protocol::{
    AgentEvent, AuthFailure, AuthKind, ModelId, ProviderError, ProviderId, TransportId, TurnRequest,
};
use xlightcli_provider::{Provider, TransportAdapter};
use xlightcli_provider_agy::{AgyEndpoints, AgyProvider};

fn account() -> AccountInfo {
    AccountInfo {
        provider: ProviderId::new("agy"),
        transport: TransportId::new("antigravity"),
        account_id: "acc".into(),
        label: None,
        auth_kind: AuthKind::Subscription,
        metadata: serde_json::json!({}),
    }
}

fn provider_with(cca_base_url: String, experimental_opt_in: bool) -> AgyProvider {
    let endpoints = AgyEndpoints {
        cca_base_url,
        // Set directly rather than via `GOOGLE_ANTIGRAVITY_PROJECT_ID`: mutating the process env
        // from a test would need `std::env::set_var`, which requires `unsafe` since Rust 2024 —
        // forbidden workspace-wide (`unsafe_code = "forbid"`, PATTERNS.md §1). `AgyEndpoints`
        // exists precisely so callers (including tests) never have to.
        antigravity_project_id: Some("test-project".into()),
        ..AgyEndpoints::default()
    };
    let flags = ExperimentalFlags {
        antigravity_subscription: experimental_opt_in,
        ..ExperimentalFlags::default()
    };
    AgyProvider::new(reqwest::Client::new(), &flags).with_endpoints(endpoints)
}

fn transport(provider: &AgyProvider) -> std::sync::Arc<dyn TransportAdapter> {
    provider
        .transport(&TransportId::new("antigravity"))
        .expect("antigravity transport must be registered when the cargo feature is on")
        .clone()
}

#[tokio::test]
async fn stream_without_experimental_opt_in_is_transport_disabled() {
    let provider = provider_with("https://example.invalid".into(), false);
    let cred = CredentialHandle::for_tests(account(), "XLC-SENTINEL-SECRET");
    // `EventStream` has no `Debug` impl, so `unwrap_err()` (which requires `T: Debug`) doesn't
    // work on `Result<EventStream, ProviderError>` — match instead.
    let err = match transport(&provider)
        .stream(
            TurnRequest::simple(ModelId::new("gemini-pro-agent"), "hi"),
            cred,
            CancellationToken::new(),
        )
        .await
    {
        Ok(_) => panic!("expected stream() to fail: experimental opt-in is off"),
        Err(e) => e,
    };
    assert!(matches!(err, ProviderError::TransportDisabled { .. }));
}

#[tokio::test]
async fn stream_200_yields_turn_started_and_completed_through_cca_wrapper() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hi\"}]},\"finishReason\":\"STOP\"}]}}\n\n",
            "text/event-stream",
        ))
        .mount(&server)
        .await;

    let provider = provider_with(server.uri(), true);
    let cred = CredentialHandle::for_tests(account(), "XLC-SENTINEL-SECRET");
    let stream = transport(&provider)
        .stream(
            TurnRequest::simple(ModelId::new("gemini-pro-agent"), "hi"),
            cred,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let events: Vec<Result<AgentEvent, ProviderError>> = stream.collect().await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Ok(AgentEvent::TurnStarted { .. })))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Ok(AgentEvent::Completed { .. })))
    );
}

#[tokio::test]
async fn stream_401_surfaces_auth_error_after_attempting_refresh() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_string("{}"))
        .mount(&server)
        .await;

    let provider = provider_with(server.uri(), true);
    let cred = CredentialHandle::for_tests(account(), "XLC-SENTINEL-SECRET");
    let err = match transport(&provider)
        .stream(
            TurnRequest::simple(ModelId::new("gemini-pro-agent"), "hi"),
            cred,
            CancellationToken::new(),
        )
        .await
    {
        Ok(_) => panic!("expected stream() to fail with a 401"),
        Err(e) => e,
    };
    // `CredentialHandle::on_unauthorized` is still a Wave 2 stub (`AuthError::NotImplemented`) as
    // of this writing, so the single-flight refresh PATTERNS.md §4 prescribes cannot succeed yet
    // — the retry-once *plumbing* is what's under test here, not a real refresh.
    assert!(matches!(
        err,
        ProviderError::Auth(AuthFailure::RefreshFailed(_))
    ));
}

#[tokio::test]
async fn stream_429_maps_to_rate_limited() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "5")
                .set_body_string("{}"),
        )
        .mount(&server)
        .await;

    let provider = provider_with(server.uri(), true);
    let cred = CredentialHandle::for_tests(account(), "XLC-SENTINEL-SECRET");
    let err = match transport(&provider)
        .stream(
            TurnRequest::simple(ModelId::new("gemini-pro-agent"), "hi"),
            cred,
            CancellationToken::new(),
        )
        .await
    {
        Ok(_) => panic!("expected stream() to fail with a 429"),
        Err(e) => e,
    };
    assert!(matches!(err, ProviderError::RateLimited { .. }));
}

#[tokio::test]
async fn quota_falls_back_to_fetch_available_models_when_summary_is_unusable() {
    let server = MockServer::start().await;
    // Both `retrieveUserQuotaSummary` and `fetchAvailableModels` are plain POSTs to the same mock
    // server; the summary attempt returns a body `parse_quota_summary` can't use (no `groups`),
    // so the transport must fall back to the models-based parse.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "models": { "gemini-pro-agent": { "quotaInfo": { "remainingFraction": 0.2 } } }
        })))
        .mount(&server)
        .await;

    let flags = ExperimentalFlags {
        antigravity_subscription: true,
        ..ExperimentalFlags::default()
    };
    let provider = AgyProvider::new(reqwest::Client::new(), &flags).with_endpoints(AgyEndpoints {
        cca_base_url: server.uri(),
        cca_daily_base_url: server.uri(),
        antigravity_project_id: Some("test-project".into()),
        ..AgyEndpoints::default()
    });
    let cred = CredentialHandle::for_tests(account(), "XLC-SENTINEL-SECRET");
    let snapshot = transport(&provider).quota(&cred).await.unwrap().unwrap();
    assert_eq!(snapshot.used_percent, Some(80.0));
}
