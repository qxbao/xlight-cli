// SPDX-License-Identifier: GPL-3.0-only

//! Black-box `wiremock` tests for the `gemini-api` transport (PATTERNS.md §13), exercised only
//! through the crate's public surface (`AgyProvider`/`AgyEndpoints`/`Provider`/`TransportAdapter`)
//! — no `pub(crate)` access. No real Google endpoint is contacted.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};
use xlightcli_auth::{AccountInfo, CredentialHandle};
use xlightcli_config::ExperimentalFlags;
use xlightcli_protocol::{
    AgentEvent, AuthKind, ModelId, ProviderError, ProviderId, TransportId, TurnRequest,
};
use xlightcli_provider::Provider;
use xlightcli_provider_agy::{AgyEndpoints, AgyProvider};

fn account() -> AccountInfo {
    AccountInfo {
        provider: ProviderId::new("agy"),
        transport: TransportId::new("gemini-api"),
        account_id: "acc".into(),
        label: None,
        auth_kind: AuthKind::ApiKey,
        metadata: serde_json::json!({}),
    }
}

fn provider_with(gemini_base_url: String) -> AgyProvider {
    let endpoints = AgyEndpoints {
        gemini_base_url,
        ..AgyEndpoints::default()
    };
    AgyProvider::new(reqwest::Client::new(), &ExperimentalFlags::default())
        .with_endpoints(endpoints)
}

#[tokio::test]
async fn stream_200_yields_turn_started_text_and_completed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hi\"}]},\"finishReason\":\"STOP\"}]}\n\n",
            "text/event-stream",
        ))
        .mount(&server)
        .await;

    let provider = provider_with(server.uri());
    let transport = provider
        .transport(&TransportId::new("gemini-api"))
        .expect("gemini-api transport must always be registered")
        .clone();
    let cred = CredentialHandle::for_tests(account(), "XLC-SENTINEL-SECRET");

    let stream = transport
        .stream(
            TurnRequest::simple(ModelId::new("gemini-3.1-pro"), "hi"),
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
    assert!(
        events.iter().all(|e| e.is_ok()),
        "no error expected: {events:?}"
    );
}

#[tokio::test]
async fn stream_429_maps_to_rate_limited_with_retry_after() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "12")
                .set_body_string("{\"error\":{\"message\":\"quota\"}}"),
        )
        .mount(&server)
        .await;

    let provider = provider_with(server.uri());
    let transport = provider
        .transport(&TransportId::new("gemini-api"))
        .unwrap()
        .clone();
    let cred = CredentialHandle::for_tests(account(), "XLC-SENTINEL-SECRET");

    // `EventStream` (`Pin<Box<dyn Stream<...> + Send>>`) has no `Debug` impl, so `unwrap_err()`
    // (which requires `T: Debug`) can't be used on `Result<EventStream, ProviderError>` — match
    // instead.
    let err = match transport
        .stream(
            TurnRequest::simple(ModelId::new("gemini-3.1-pro"), "hi"),
            cred,
            CancellationToken::new(),
        )
        .await
    {
        Ok(_) => panic!("expected stream() to fail with a 429"),
        Err(e) => e,
    };
    match err {
        ProviderError::RateLimited { retry_after, .. } => {
            assert_eq!(retry_after, Some(Duration::from_secs(12)));
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

#[tokio::test]
async fn stream_with_malformed_sse_frame_surfaces_protocol_mismatch() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw("data: not-json\n\n", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let provider = provider_with(server.uri());
    let transport = provider
        .transport(&TransportId::new("gemini-api"))
        .unwrap()
        .clone();
    let cred = CredentialHandle::for_tests(account(), "XLC-SENTINEL-SECRET");

    let stream = transport
        .stream(
            TurnRequest::simple(ModelId::new("gemini-3.1-pro"), "hi"),
            cred,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let events: Vec<Result<AgentEvent, ProviderError>> = stream.collect().await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Err(ProviderError::ProtocolMismatch { .. })))
    );
}

#[tokio::test]
async fn quota_is_always_none_for_gemini_api() {
    let provider = provider_with("https://example.invalid".into());
    let transport = provider
        .transport(&TransportId::new("gemini-api"))
        .unwrap()
        .clone();
    let cred = CredentialHandle::for_tests(account(), "XLC-SENTINEL-SECRET");
    assert!(transport.quota(&cred).await.unwrap().is_none());
}
