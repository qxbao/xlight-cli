// SPDX-License-Identifier: GPL-3.0-only

//! Transport-level tests for `anthropic-api` (PATTERNS.md §13): 200 SSE round-trip, 401 →
//! `on_unauthorized` retry, 429 with `Retry-After`, 529 `overloaded_error`, and a schema mismatch.
//! Uses `wiremock` against `ClaudeProvider::with_endpoints` (never a real Anthropic endpoint —
//! AGENTS.md §7).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use xlightcli_auth::{AccountInfo, CredentialHandle};
use xlightcli_protocol::{
    AgentEvent, AuthKind, ContentBlock, ModelId, ProviderError, ProviderId, TransportId,
    TurnRequest,
};
use xlightcli_provider::Provider;
use xlightcli_provider_claude::{ClaudeEndpoints, ClaudeProvider};

fn cred() -> CredentialHandle {
    CredentialHandle::for_tests(
        AccountInfo {
            provider: ProviderId::new("claude"),
            transport: TransportId::new("anthropic-api"),
            account_id: "test-account".into(),
            label: None,
            auth_kind: AuthKind::ApiKey,
            metadata: serde_json::json!({}),
        },
        "XLC-SENTINEL-SECRET",
    )
}

fn provider_against(server: &MockServer) -> ClaudeProvider {
    ClaudeProvider::new(
        reqwest::Client::new(),
        &xlightcli_config::ExperimentalFlags::default(),
    )
    .with_endpoints(ClaudeEndpoints {
        anthropic_api_base_url: server.uri(),
        ..ClaudeEndpoints::default()
    })
}

/// `Result::expect_err`/`unwrap_err` require `T: Debug`, but `EventStream` (a boxed `dyn Stream`)
/// isn't — hence this manual match instead.
fn expect_stream_err(
    result: Result<xlightcli_provider::EventStream, ProviderError>,
    msg: &str,
) -> ProviderError {
    match result {
        Ok(_) => panic!("{msg}"),
        Err(e) => e,
    }
}

async fn fixture(name: &str) -> String {
    tokio::fs::read_to_string(format!(
        "{}/tests/fixtures/{name}.sse",
        env!("CARGO_MANIFEST_DIR")
    ))
    .await
    .unwrap_or_else(|e| panic!("reading fixture {name}: {e}"))
}

#[tokio::test]
async fn successful_sse_stream_round_trips_to_a_completed_message() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(fixture("text").await, "text/event-stream"),
        )
        .mount(&server)
        .await;

    let provider = provider_against(&server);
    let transport = provider
        .transport(&TransportId::new("anthropic-api"))
        .expect("anthropic-api transport must always be registered")
        .clone();
    let req = TurnRequest::simple(ModelId::new("claude-fixture"), "hi");
    let stream = transport
        .stream(req, cred(), CancellationToken::new())
        .await
        .expect("stream() should succeed against a 200 SSE response");
    let events: Vec<Result<AgentEvent, ProviderError>> = stream.collect().await;

    assert!(matches!(
        events.first(),
        Some(Ok(AgentEvent::TurnStarted { .. }))
    ));
    match events.last() {
        Some(Ok(AgentEvent::Completed { message, .. })) => {
            assert_eq!(
                message.content,
                vec![ContentBlock::Text {
                    text: "Hello, world!".into()
                }]
            );
        }
        other => panic!("expected the last event to be Completed, got {other:?}"),
    }
}

#[tokio::test]
async fn rate_limit_headers_surface_as_a_rate_limit_event() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("anthropic-ratelimit-requests-limit", "1000")
                .insert_header("anthropic-ratelimit-requests-remaining", "999")
                .set_body_raw(fixture("text").await, "text/event-stream"),
        )
        .mount(&server)
        .await;

    let provider = provider_against(&server);
    let transport = provider
        .transport(&TransportId::new("anthropic-api"))
        .expect("anthropic-api transport must always be registered")
        .clone();
    let req = TurnRequest::simple(ModelId::new("claude-fixture"), "hi");
    let stream = transport
        .stream(req, cred(), CancellationToken::new())
        .await
        .expect("stream() should succeed against a 200 SSE response");
    let events: Vec<Result<AgentEvent, ProviderError>> = stream.collect().await;

    let rate_limit = events.into_iter().find_map(|e| match e {
        Ok(AgentEvent::RateLimit(info)) => Some(info),
        _ => None,
    });
    match rate_limit {
        Some(info) => {
            assert_eq!(info.limit, Some(1000));
            assert_eq!(info.remaining, Some(999));
        }
        None => panic!("expected an AgentEvent::RateLimit in the stream"),
    }
}

#[tokio::test]
async fn a_401_response_triggers_on_unauthorized_and_surfaces_as_an_auth_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(401).set_body_string("{\"error\":\"invalid api key\"}"))
        .mount(&server)
        .await;

    let provider = provider_against(&server);
    let transport = provider
        .transport(&TransportId::new("anthropic-api"))
        .unwrap()
        .clone();
    let req = TurnRequest::simple(ModelId::new("claude-fixture"), "hi");
    // `CredentialHandle::on_unauthorized` is still a Wave 2 stub in `xlightcli-auth` (always
    // `NotImplemented`), so the retry-once wiring can only be exercised up to that boundary here:
    // exactly one HTTP request should reach the mock, and the resulting error should be an auth
    // failure (not a raw network/upstream error) — proving the 401 path is wired to
    // `on_unauthorized` rather than being treated as a generic failure.
    let err = expect_stream_err(
        transport
            .stream(req, cred(), CancellationToken::new())
            .await,
        "401 must not succeed",
    );
    assert!(matches!(err, ProviderError::Auth(_)));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_429_response_maps_to_rate_limited_with_retry_after() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "30")
                .set_body_string("{\"error\":\"rate limited\"}"),
        )
        .mount(&server)
        .await;

    let provider = provider_against(&server);
    let transport = provider
        .transport(&TransportId::new("anthropic-api"))
        .unwrap()
        .clone();
    let req = TurnRequest::simple(ModelId::new("claude-fixture"), "hi");
    let err = expect_stream_err(
        transport
            .stream(req, cred(), CancellationToken::new())
            .await,
        "429 must not succeed",
    );
    match err {
        ProviderError::RateLimited { retry_after, .. } => {
            assert_eq!(retry_after, Some(std::time::Duration::from_secs(30)));
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

#[tokio::test]
async fn a_529_overloaded_response_maps_to_upstream_529() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(529).set_body_string("{\"error\":\"overloaded\"}"))
        .mount(&server)
        .await;

    let provider = provider_against(&server);
    let transport = provider
        .transport(&TransportId::new("anthropic-api"))
        .unwrap()
        .clone();
    let req = TurnRequest::simple(ModelId::new("claude-fixture"), "hi");
    let err = expect_stream_err(
        transport
            .stream(req, cred(), CancellationToken::new())
            .await,
        "529 must not succeed",
    );
    assert!(matches!(err, ProviderError::Upstream { status: 529, .. }));
}

#[tokio::test]
async fn an_unknown_content_block_type_surfaces_as_protocol_mismatch() {
    let server = MockServer::start().await;
    let body = "event: message_start\n\
                data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n\
                event: content_block_start\n\
                data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"a_type_from_the_future\"}}\n\n";
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider = provider_against(&server);
    let transport = provider
        .transport(&TransportId::new("anthropic-api"))
        .unwrap()
        .clone();
    let req = TurnRequest::simple(ModelId::new("claude-fixture"), "hi");
    let stream = transport
        .stream(req, cred(), CancellationToken::new())
        .await
        .expect("stream() itself should succeed; the mismatch surfaces as a stream item");
    let events: Vec<_> = stream.collect().await;
    let mismatch = events
        .into_iter()
        .find_map(|e| e.err())
        .expect("expected a ProtocolMismatch error somewhere in the stream");
    assert!(matches!(mismatch, ProviderError::ProtocolMismatch { .. }));
}
