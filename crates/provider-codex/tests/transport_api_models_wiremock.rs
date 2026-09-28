// SPDX-License-Identifier: GPL-3.0-only

//! The public OpenAI API transport fetches the model list through the documented `/v1/models`
//! endpoint with its own API-key credential; this test never reaches a real provider.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use xlightcli_auth::{AccountInfo, CredentialHandle};
use xlightcli_config::ExperimentalFlags;
use xlightcli_protocol::{AuthKind, ProviderId, TransportId};
use xlightcli_provider::Provider;
use xlightcli_provider_codex::{CodexEndpoints, CodexProvider};

#[tokio::test]
async fn openai_api_lists_models_from_the_live_catalog_shape() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", "Bearer XLC-SENTINEL-API-KEY"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{"id": "gpt-6-luna"}, {"id": "gpt-6-sol"}]
        })))
        .mount(&server)
        .await;

    let provider = CodexProvider::new(reqwest::Client::new(), &ExperimentalFlags::default())
        .with_endpoints(CodexEndpoints {
            openai_api_base: format!("{}/v1", server.uri()),
            ..CodexEndpoints::default()
        });
    let transport = provider.transport(&TransportId::new("openai-api")).unwrap();
    let credential = CredentialHandle::for_tests(
        AccountInfo {
            provider: ProviderId::new("codex"),
            transport: TransportId::new("openai-api"),
            account_id: "synthetic-account".to_string(),
            label: None,
            auth_kind: AuthKind::ApiKey,
            metadata: serde_json::json!({}),
        },
        "XLC-SENTINEL-API-KEY",
    );
    let models = transport.list_models(&credential).await.unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id.as_str(), "gpt-6-luna");
}
