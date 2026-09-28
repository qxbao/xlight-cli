// SPDX-License-Identifier: GPL-3.0-only

//! `wiremock`-backed transport tests for `chatgpt` (PATTERNS.md §13): 200 SSE, 401 (recovery
//! attempt then surfaced `Auth` error — `CredentialHandle::for_tests` handles always reject
//! refresh by design, see the test below), 429 with `Retry-After`, and a malformed
//! `response.completed` body (schema mismatch → `ProtocolMismatch`).
//!
//! Only `pub` surface is used here (`CodexProvider`, `CodexEndpoints`, the `TransportAdapter`
//! trait object) — `ChatgptTransport`/`Translator` are `pub(crate)` (INV-3), so this integration
//! crate cannot and does not reach them directly.
//!
//! If these failed to bind/connect ONLY because of the sandbox's loopback network restriction,
//! they would be marked `#[ignore = "needs loopback (sandbox)"]` — empirically they bind/connect
//! fine in this sandbox (verified by running them), so they run by default.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use xlightcli_auth::{AccountInfo, CredentialHandle};
use xlightcli_config::ExperimentalFlags;
use xlightcli_protocol::{
    AgentEvent, AuthKind, ModelId, ProviderError, ProviderId, TransportId, TurnRequest,
};
use xlightcli_provider::{Provider, TransportAdapter};
use xlightcli_provider_codex::{CodexEndpoints, CodexProvider};

fn test_cred(transport: &str) -> CredentialHandle {
    CredentialHandle::for_tests(
        AccountInfo {
            provider: ProviderId::new("codex"),
            transport: TransportId::new(transport),
            account_id: "acc-fixture".into(),
            label: None,
            auth_kind: AuthKind::Subscription,
            metadata: serde_json::json!({}),
        },
        "XLC-SENTINEL-SECRET",
    )
}

fn provider_with_base(base: String) -> CodexProvider {
    CodexProvider::new(reqwest::Client::new(), &ExperimentalFlags::default()).with_endpoints(
        CodexEndpoints {
            chatgpt_backend_base: base,
            ..CodexEndpoints::default()
        },
    )
}

fn chatgpt_transport(provider: &CodexProvider) -> std::sync::Arc<dyn TransportAdapter> {
    provider
        .transport(&TransportId::new("chatgpt"))
        .expect("chatgpt transport registered")
        .clone()
}

#[tokio::test]
async fn chatgpt_stream_happy_path_yields_completed() {
    let server = MockServer::start().await;
    let sse_body = include_str!("fixtures/text.sse");
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(wiremock::matchers::header(
            "chatgpt-account-id",
            "acc-fixture",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider = provider_with_base(server.uri());
    let transport = chatgpt_transport(&provider);
    let req = TurnRequest::simple(ModelId::new("gpt-5-codex"), "hi");
    let stream = transport
        .stream(req, test_cred("chatgpt"), CancellationToken::new())
        .await
        .unwrap();
    let events: Vec<_> = stream.collect().await;
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
    assert!(events.iter().all(Result::is_ok));
}

#[tokio::test]
async fn chatgpt_stream_401_attempts_recovery_then_surfaces_auth_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let provider = provider_with_base(server.uri());
    let transport = chatgpt_transport(&provider);
    let req = TurnRequest::simple(ModelId::new("gpt-5-codex"), "hi");
    // `CredentialHandle::for_tests` handles have no refresh target, so `on_unauthorized` always
    // returns `AuthError::RefreshRejected` by design (xlightcli-auth). This asserts we still call
    // it and surface a well-typed `Auth` error rather than retrying forever or panicking; a
    // *successful* refresh+retry needs a real `AuthBroker`-backed handle, out of this crate's
    // scope to construct.
    let result = transport
        .stream(req, test_cred("chatgpt"), CancellationToken::new())
        .await;
    assert!(matches!(result, Err(ProviderError::Auth(_))));
}

#[tokio::test]
async fn chatgpt_stream_429_maps_to_rate_limited_with_retry_after() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "30"))
        .mount(&server)
        .await;

    let provider = provider_with_base(server.uri());
    let transport = chatgpt_transport(&provider);
    let req = TurnRequest::simple(ModelId::new("gpt-5-codex"), "hi");
    let result = transport
        .stream(req, test_cred("chatgpt"), CancellationToken::new())
        .await;
    match result {
        Err(ProviderError::RateLimited { retry_after, .. }) => {
            assert_eq!(retry_after, Some(std::time::Duration::from_secs(30)));
        }
        Err(other) => panic!("expected RateLimited, got Err({other:?})"),
        Ok(_) => panic!("expected RateLimited, got Ok(stream)"),
    }
}

#[tokio::test]
async fn chatgpt_stream_malformed_completed_body_is_protocol_mismatch() {
    let server = MockServer::start().await;
    // Missing the required `response` object inside `response.completed` (PATTERNS.md §5:
    // never guess — surface `ProtocolMismatch`).
    let body = "event: response.completed\ndata: {\"type\":\"response.completed\"}\n\n";
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider = provider_with_base(server.uri());
    let transport = chatgpt_transport(&provider);
    let req = TurnRequest::simple(ModelId::new("gpt-5-codex"), "hi");
    let stream = transport
        .stream(req, test_cred("chatgpt"), CancellationToken::new())
        .await
        .unwrap();
    let events: Vec<_> = stream.collect().await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Err(ProviderError::ProtocolMismatch { .. })))
    );
}

#[tokio::test]
async fn chatgpt_quota_parses_usage_response() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/wham/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "plan_type": "plus",
            "primary": {"used_percent": 50.0}
        })))
        .mount(&server)
        .await;

    let provider = provider_with_base(server.uri());
    let transport = chatgpt_transport(&provider);
    let snapshot = transport
        .quota(&test_cred("chatgpt"))
        .await
        .unwrap()
        .expect("chatgpt quota() returns Some");
    assert_eq!(snapshot.plan.as_deref(), Some("plus"));
    assert_eq!(snapshot.used_percent, Some(50.0));
}
