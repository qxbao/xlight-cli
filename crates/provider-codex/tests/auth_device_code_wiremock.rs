// SPDX-License-Identifier: GPL-3.0-only

//! `wiremock` test for the full ChatGPT device-code login: the two Codex-specific HTTP calls
//! (`request_user_code` / `poll_device_grant` in `src/auth.rs`, implemented directly against
//! `reqwest` since Codex's grant doesn't match the RFC 8628 shape `xlightcli_auth::oauth`'s
//! generic device-code helpers assume — see the final report) plus the final token exchange,
//! which now goes through `xlightcli_auth::oauth::exchange_code_for_token` (implemented by the
//! concurrent Wave 2 `auth` agent as of this run).
//!
//! If wiremock cannot bind/connect ONLY because of the sandbox's loopback restriction, this would
//! be marked `#[ignore = "needs loopback (sandbox)"]` — empirically it binds/connects fine in this
//! sandbox (verified by running it un-ignored), so it runs by default.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use async_trait::async_trait;
use base64::Engine;
use secrecy::{ExposeSecret, SecretString};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use xlightcli_auth::{AuthError, AuthMethod, CredentialSecret, LoginUi};
use xlightcli_config::ExperimentalFlags;
use xlightcli_protocol::{AuthKind, ProviderId};
use xlightcli_provider::Provider;
use xlightcli_provider_codex::{CodexEndpoints, CodexProvider};

struct RecordingUi {
    shown_code: std::sync::Mutex<Option<String>>,
}

#[async_trait]
impl LoginUi for RecordingUi {
    async fn show_browser_url(&self, _url: &str) {}
    async fn show_device_code(&self, _verification_uri: &str, user_code: &str) {
        *self.shown_code.lock().unwrap() = Some(user_code.to_string());
    }
    async fn prompt_api_key(&self, _provider: &ProviderId) -> Result<SecretString, AuthError> {
        Ok(SecretString::from("unused".to_string()))
    }
}

fn fake_id_token(account_id: &str) -> String {
    let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"{\"alg\":\"none\"}");
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(format!("{{\"chatgpt_account_id\":\"{account_id}\"}}"));
    format!("{header}.{payload}.sig")
}

#[tokio::test]
async fn device_code_login_succeeds_end_to_end() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "device_auth_id": "dev-auth-1",
            "user_code": "ABCD-1234",
            "interval": 0.01
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "authorization_code": "auth-code-1",
            "code_verifier": "verifier-1"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "XLC-SENTINEL-ACCESS",
            "refresh_token": "XLC-SENTINEL-REFRESH",
            "expires_in": 3600,
            "id_token": fake_id_token("acc-e2e-test")
        })))
        .mount(&server)
        .await;

    let provider = CodexProvider::new(reqwest::Client::new(), &ExperimentalFlags::default())
        .with_endpoints(CodexEndpoints {
            chatgpt_device_usercode_url: format!(
                "{}/api/accounts/deviceauth/usercode",
                server.uri()
            ),
            chatgpt_device_token_url: format!("{}/api/accounts/deviceauth/token", server.uri()),
            chatgpt_oauth_token_url: format!("{}/oauth/token", server.uri()),
            ..CodexEndpoints::default()
        });
    let ui = RecordingUi {
        shown_code: std::sync::Mutex::new(None),
    };

    let credential_set = provider
        .auth()
        .login(AuthMethod::DeviceCode, &ui)
        .await
        .expect("device code login should succeed against the mocked endpoints");

    assert_eq!(
        ui.shown_code.into_inner().unwrap().as_deref(),
        Some("ABCD-1234")
    );
    assert_eq!(credential_set.account.account_id, "acc-e2e-test");
    assert_eq!(credential_set.account.auth_kind, AuthKind::Subscription);
    match credential_set.secret {
        CredentialSecret::Bearer {
            access_token,
            refresh_token,
            ..
        } => {
            assert_eq!(access_token.expose_secret(), "XLC-SENTINEL-ACCESS");
            assert_eq!(
                refresh_token.unwrap().expose_secret(),
                "XLC-SENTINEL-REFRESH"
            );
        }
        CredentialSecret::Header { .. } => panic!("expected Bearer secret"),
    }
}
