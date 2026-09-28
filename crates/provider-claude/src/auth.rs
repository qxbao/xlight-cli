// SPDX-License-Identifier: GPL-3.0-only
// Portions derived from OpenCodex (MIT) @ 3cc34e1181926b64331490fdcfee162ffb62fe73:
// src/oauth/anthropic.ts, src/oauth/local-token-detect.ts.
// See THIRD_PARTY.md.

//! `AuthAdapter` impl for Claude: API key entry (`anthropic-api`, always available) and, under
//! cargo feature `claude-subscription`, Claude Pro/Max OAuth (browser PKCE + loopback, refresh,
//! discovery/import of the local Claude Code credential file) (docs/PLAN.md §6, D-002).
//!
//! Port notes (docs/PLAN.md §4.5): the OAuth endpoints/client id/scopes and the Claude Code
//! credential file shape are ported from OpenCodex (MIT) @
//! 3cc34e1181926b64331490fdcfee162ffb62fe73, `src/oauth/anthropic.ts` and
//! `src/oauth/local-token-detect.ts` (see `THIRD_PARTY.md`). Not ported: account pooling/local
//! CLI spawning/keychain reads (INV-1, INV-9) — see `consts.rs` doc comments for what's
//! deliberately left unimplemented (macOS Keychain).

use std::path::PathBuf;
#[cfg(feature = "claude-subscription")]
use std::time::Duration;

use async_trait::async_trait;
#[cfg(feature = "claude-subscription")]
use secrecy::ExposeSecret;
use secrecy::SecretString;
use serde::Deserialize;
use time::OffsetDateTime;
use xlightcli_auth::{
    AccountInfo, AuthAdapter, AuthError, AuthMethod, CredentialSecret, CredentialSet,
    DiscoveredCredential, LoginUi, discovery,
};
use xlightcli_protocol::{AuthKind, ProviderId, TransportId};

use crate::consts;

const PROVIDER_ID: &str = "claude";

fn provider_id() -> ProviderId {
    ProviderId::new(PROVIDER_ID)
}

/// Resolves the home directory used to locate `~/.claude` (Linux; macOS falls back to the same
/// file when the Keychain isn't read — see `consts.rs`). Deliberately not `directories::BaseDirs`
/// (that's an XDG *app data* helper owned by `config`; this crate reads *another* CLI's dotfiles,
/// which always live under the literal home directory regardless of `XDG_*`).
fn resolve_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `CLAUDE_CONFIG_DIR` env override, else `<home>/.claude` (docs/import.md, `local-token-detect.ts`).
fn claude_config_dir(home: &std::path::Path) -> PathBuf {
    match std::env::var(consts::CLAUDE_CONFIG_DIR_ENV) {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => home.join(consts::CLAUDE_CONFIG_DIR_DEFAULT_RELATIVE),
    }
}

#[derive(Debug, Deserialize)]
struct CredentialsFile {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: OauthPayload,
}

#[derive(Debug, Deserialize)]
struct OauthPayload {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(rename = "refreshToken")]
    refresh_token: String,
    /// Epoch milliseconds. Optional in practice; treated as "unknown" (no expiry known) when
    /// absent rather than guessed.
    #[serde(rename = "expiresAt")]
    expires_at: Option<i64>,
}

fn parse_claude_credentials_json(raw: &str) -> Result<CredentialSet, AuthError> {
    let file: CredentialsFile = serde_json::from_str(raw)
        .map_err(|e| AuthError::OAuth(format!("malformed Claude Code credentials file: {e}")))?;
    let expires_at = file
        .claude_ai_oauth
        .expires_at
        .and_then(|ms| OffsetDateTime::from_unix_timestamp(ms / 1000).ok());
    Ok(CredentialSet {
        account: AccountInfo {
            provider: provider_id(),
            transport: TransportId::new("claude-subscription"),
            // The credentials file itself carries no stable account id/email (only the OAuth
            // token-exchange response does, in `account.uuid`/`account.email_address`) — label it
            // by source instead of guessing an identifier.
            account_id: "claude-code-imported".to_string(),
            label: Some("Claude Code (imported)".to_string()),
            auth_kind: AuthKind::Subscription,
            metadata: serde_json::json!({}),
        },
        secret: CredentialSecret::Bearer {
            access_token: SecretString::from(file.claude_ai_oauth.access_token),
            refresh_token: Some(SecretString::from(file.claude_ai_oauth.refresh_token)),
            expires_at,
        },
    })
}

/// `AuthAdapter` for the Claude provider. Constructed once by `ClaudeProvider::new`.
#[derive(Debug)]
pub(crate) struct ClaudeAuthAdapter {
    // Only read by the `claude-subscription`-gated OAuth login/refresh flows below; without the
    // feature these two fields are genuinely unused (the API-key/discovery/import paths don't
    // need them), hence the `cfg_attr`.
    #[cfg_attr(not(feature = "claude-subscription"), allow(dead_code))]
    http: reqwest::Client,
    home: PathBuf,
    methods: Vec<AuthMethod>,
    #[cfg_attr(not(feature = "claude-subscription"), allow(dead_code))]
    endpoints: crate::ClaudeEndpoints,
}

impl ClaudeAuthAdapter {
    pub(crate) fn new(http: reqwest::Client, endpoints: crate::ClaudeEndpoints) -> Self {
        let mut methods = vec![AuthMethod::ApiKey];
        if cfg!(feature = "claude-subscription") {
            methods.push(AuthMethod::ReuseExisting);
            methods.push(AuthMethod::BrowserOAuth);
        }
        Self {
            http,
            home: resolve_home(),
            methods,
            endpoints,
        }
    }

    /// Test-only hook so discovery/import tests use a tempdir instead of the real `$HOME`
    /// (never read the user's real `~/.claude` — team brief / AGENTS.md §7).
    #[cfg(test)]
    pub(crate) fn with_home(mut self, home: PathBuf) -> Self {
        self.home = home;
        self
    }

    async fn login_api_key(&self, ui: &dyn LoginUi) -> Result<CredentialSet, AuthError> {
        self.login_api_key_with_env(ui, std::env::var("ANTHROPIC_API_KEY").ok())
            .await
    }

    /// Split out from `login_api_key` so tests can supply the "env var" value directly —
    /// `[lints] unsafe_code = "forbid"` (workspace-wide) rules out `std::env::set_var` (`unsafe`
    /// since edition 2024) even in `#[cfg(test)]` code.
    async fn login_api_key_with_env(
        &self,
        ui: &dyn LoginUi,
        env_api_key: Option<String>,
    ) -> Result<CredentialSet, AuthError> {
        let secret = match env_api_key {
            Some(v) if !v.trim().is_empty() => SecretString::from(v),
            _ => ui.prompt_api_key(&provider_id()).await?,
        };
        Ok(CredentialSet {
            account: AccountInfo {
                provider: provider_id(),
                transport: TransportId::new("anthropic-api"),
                account_id: "api-key".to_string(),
                label: None,
                auth_kind: AuthKind::ApiKey,
                metadata: serde_json::json!({}),
            },
            secret: CredentialSecret::Header {
                header_name: consts::API_KEY_HEADER.to_string(),
                value: secret,
            },
        })
    }

    async fn login_reuse_existing(&self) -> Result<CredentialSet, AuthError> {
        let found = self.discover_existing().await;
        let first = found
            .into_iter()
            .next()
            .ok_or_else(|| AuthError::OAuth("no existing Claude Code credential found".into()))?;
        self.import(&first).await
    }

    #[cfg(feature = "claude-subscription")]
    async fn login_browser_oauth(&self, ui: &dyn LoginUi) -> Result<CredentialSet, AuthError> {
        let pkce = xlightcli_auth::oauth::generate_pkce();
        let state = uuid::Uuid::new_v4().to_string();
        let server = xlightcli_auth::oauth::LoopbackServer::bind(
            Some(self.endpoints.oauth_callback_port),
            &self.endpoints.oauth_callback_path,
            "localhost",
        )
        .await?;
        let redirect_uri = server.redirect_uri().to_string();
        let url = xlightcli_auth::oauth::build_authorization_url(
            xlightcli_auth::oauth::AuthorizationUrlParams {
                authorize_endpoint: &self.endpoints.oauth_authorize_url,
                client_id: &self.endpoints.oauth_client_id,
                redirect_uri: &redirect_uri,
                scope: consts::OAUTH_SCOPES,
                state: &state,
                code_challenge: &pkce.challenge,
                code_challenge_method: pkce.challenge_method,
                extra_params: &[("code", "true")],
            },
        )?;
        ui.show_browser_url(url.as_str()).await;
        let callback = server
            .wait_for_callback(&state, Duration::from_secs(300))
            .await?;
        // Anthropic's token endpoint takes a JSON body (not RFC 6749 form encoding) and may hand
        // back the code as `code#state` (OpenCodex src/oauth/anthropic.ts `exchangeToken`).
        let (code, exchange_state) = split_code_and_state(callback.code.expose_secret(), &state);
        let body = serde_json::json!({
            "grant_type": "authorization_code",
            "client_id": self.endpoints.oauth_client_id,
            "code": code,
            "state": exchange_state,
            "redirect_uri": redirect_uri,
            "code_verifier": pkce.verifier.expose_secret(),
        });
        let token = post_json_token(&self.http, &self.endpoints.oauth_token_url, &body).await?;
        Ok(credential_set_from_token(
            TransportId::new("claude-subscription"),
            token,
        ))
    }
}

/// Splits a callback code of the form `code#state` (Anthropic appends the state as a fragment);
/// falls back to the state we sent when there is no fragment.
#[cfg(feature = "claude-subscription")]
fn split_code_and_state<'a>(raw_code: &'a str, sent_state: &'a str) -> (&'a str, &'a str) {
    match raw_code.split_once('#') {
        Some((code, frag)) if !frag.is_empty() => (code, frag),
        Some((code, _)) => (code, sent_state),
        None => (raw_code, sent_state),
    }
}

/// POSTs a JSON token request and parses a standard OAuth token response. Error messages carry
/// only the status and a redacted, truncated body excerpt (docs/PLAN.md §14).
#[cfg(feature = "claude-subscription")]
async fn post_json_token(
    http: &reqwest::Client,
    token_url: &str,
    body: &serde_json::Value,
) -> Result<xlightcli_auth::oauth::TokenResponse, AuthError> {
    let resp = http
        .post(token_url)
        .header(reqwest::header::ACCEPT, "application/json")
        .json(body)
        .send()
        .await
        .map_err(|e| AuthError::OAuth(format!("token endpoint request failed: {e}")))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| AuthError::OAuth(format!("reading token endpoint response: {e}")))?;
    if !status.is_success() {
        return Err(AuthError::OAuth(format!(
            "token endpoint returned {status}: {}",
            xlightcli_provider::error::body_excerpt(&xlightcli_auth::redact::redact(&text))
        )));
    }
    let raw: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| AuthError::OAuth(format!("token endpoint returned invalid JSON: {e}")))?;
    let access_token = raw
        .get("access_token")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| AuthError::OAuth("token response has no access_token".into()))?;
    Ok(xlightcli_auth::oauth::TokenResponse {
        access_token: SecretString::from(access_token.to_owned()),
        refresh_token: raw
            .get("refresh_token")
            .and_then(serde_json::Value::as_str)
            .map(|t| SecretString::from(t.to_owned())),
        expires_in: raw
            .get("expires_in")
            .and_then(serde_json::Value::as_u64)
            .map(Duration::from_secs),
        token_type: raw
            .get("token_type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Bearer")
            .to_owned(),
        raw,
    })
}

/// Builds a `CredentialSet` from an OAuth token response, pulling `account.uuid`/
/// `account.email_address` out of `TokenResponse::raw` when present (Anthropic's token endpoint
/// echoes them back — OpenCodex `credsFrom`) rather than leaving the account unidentified.
#[cfg(feature = "claude-subscription")]
fn credential_set_from_token(
    transport: TransportId,
    token: xlightcli_auth::oauth::TokenResponse,
) -> CredentialSet {
    let account_id = token
        .raw
        .get("account")
        .and_then(|a| a.get("uuid"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| "claude-subscription".to_string());
    let label = token
        .raw
        .get("account")
        .and_then(|a| a.get("email_address"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let expires_at = token.expires_in.map(|d| OffsetDateTime::now_utc() + d);
    CredentialSet {
        account: AccountInfo {
            provider: provider_id(),
            transport,
            account_id,
            label,
            auth_kind: AuthKind::Subscription,
            metadata: serde_json::json!({}),
        },
        secret: CredentialSecret::Bearer {
            access_token: token.access_token,
            refresh_token: token.refresh_token,
            expires_at,
        },
    }
}

#[async_trait]
impl AuthAdapter for ClaudeAuthAdapter {
    fn methods(&self) -> &[AuthMethod] {
        &self.methods
    }

    async fn discover_existing(&self) -> Vec<DiscoveredCredential> {
        let config_dir = claude_config_dir(&self.home);
        let path = config_dir.join(consts::CREDENTIALS_FILENAME);
        let found = discovery::existing(vec![discovery::DiscoveryLocation {
            description: format!("Claude Code credentials ({})", path.display()),
            path,
        }]);
        found
            .into_iter()
            .map(|loc| DiscoveredCredential {
                provider: provider_id(),
                transport: TransportId::new("claude-subscription"),
                account_label: "Claude Code".to_string(),
                source: loc.path,
            })
            .collect()
    }

    async fn import(&self, found: &DiscoveredCredential) -> Result<CredentialSet, AuthError> {
        let raw = tokio::fs::read_to_string(&found.source)
            .await
            .map_err(|e| AuthError::OAuth(format!("reading {}: {e}", found.source.display())))?;
        parse_claude_credentials_json(&raw)
    }

    async fn login(
        &self,
        method: AuthMethod,
        ui: &dyn LoginUi,
    ) -> Result<CredentialSet, AuthError> {
        match method {
            AuthMethod::ApiKey => self.login_api_key(ui).await,
            AuthMethod::ReuseExisting => self.login_reuse_existing().await,
            #[cfg(feature = "claude-subscription")]
            AuthMethod::BrowserOAuth => self.login_browser_oauth(ui).await,
            #[cfg(not(feature = "claude-subscription"))]
            AuthMethod::BrowserOAuth => Err(AuthError::NotImplemented(
                "claude-subscription cargo feature is disabled",
            )),
            AuthMethod::DeviceCode => Err(AuthError::NotImplemented(
                "claude has no documented device-code flow",
            )),
        }
    }

    async fn refresh(&self, current: &CredentialSet) -> Result<CredentialSet, AuthError> {
        match &current.secret {
            // API keys don't expire/refresh from our side.
            CredentialSecret::Header { .. } => Ok(current.clone()),
            CredentialSecret::Bearer { refresh_token, .. } => {
                let _ = refresh_token;
                #[cfg(feature = "claude-subscription")]
                {
                    let refresh_token = refresh_token.clone().ok_or(AuthError::RefreshRejected)?;
                    let body = serde_json::json!({
                        "grant_type": "refresh_token",
                        "client_id": self.endpoints.oauth_client_id,
                        "refresh_token": refresh_token.expose_secret(),
                    });
                    let mut token =
                        post_json_token(&self.http, &self.endpoints.oauth_token_url, &body).await?;
                    // Upstream may omit a rotated refresh token; keep the current one then.
                    if token.refresh_token.is_none() {
                        token.refresh_token = Some(refresh_token);
                    }
                    Ok(credential_set_from_token(
                        current.account.transport.clone(),
                        token,
                    ))
                }
                #[cfg(not(feature = "claude-subscription"))]
                {
                    Err(AuthError::NotImplemented(
                        "claude-subscription cargo feature is disabled",
                    ))
                }
            }
        }
    }

    async fn revoke(&self, _current: &CredentialSet) -> Result<(), AuthError> {
        // Anthropic has no documented token-revocation endpoint (docs/providers/claude.md); this
        // is a local no-op — `AuthBroker`/`SecretStore` own actually forgetting the credential.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use async_trait::async_trait;
    use pretty_assertions::assert_eq;
    use xlightcli_auth::AuthError;

    use super::*;

    struct NoopUi;

    #[async_trait]
    impl LoginUi for NoopUi {
        async fn show_browser_url(&self, _url: &str) {}
        async fn show_device_code(&self, _verification_uri: &str, _user_code: &str) {}
        async fn prompt_api_key(&self, _provider: &ProviderId) -> Result<SecretString, AuthError> {
            Ok(SecretString::from("XLC-SENTINEL-SECRET".to_string()))
        }
    }

    fn adapter_with_home(home: PathBuf) -> ClaudeAuthAdapter {
        ClaudeAuthAdapter::new(reqwest::Client::new(), crate::ClaudeEndpoints::default())
            .with_home(home)
    }

    #[test]
    fn parses_claude_code_credentials_file() {
        let raw = r#"{"claudeAiOauth":{"accessToken":"XLC-SENTINEL-SECRET","refreshToken":"XLC-SENTINEL-REFRESH","expiresAt":4102444800000}}"#;
        let set = parse_claude_credentials_json(raw).unwrap();
        match set.secret {
            CredentialSecret::Bearer { expires_at, .. } => {
                assert!(expires_at.is_some());
            }
            other => panic!("expected Bearer, got {other:?}"),
        }
    }

    #[test]
    fn malformed_credentials_file_is_rejected() {
        let err = parse_claude_credentials_json("{}").unwrap_err();
        assert!(matches!(err, AuthError::OAuth(_)));
    }

    #[tokio::test]
    async fn discover_existing_finds_a_tempdir_fixture_not_the_real_home() {
        let dir = tempfile::tempdir().unwrap();
        let claude_dir = dir.path().join(".claude");
        tokio::fs::create_dir_all(&claude_dir).await.unwrap();
        tokio::fs::write(
            claude_dir.join(".credentials.json"),
            r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"b"}}"#,
        )
        .await
        .unwrap();

        let adapter = adapter_with_home(dir.path().to_path_buf());
        let found = adapter.discover_existing().await;
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].provider, provider_id());
    }

    #[tokio::test]
    async fn discover_existing_returns_empty_when_no_file_present() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = adapter_with_home(dir.path().to_path_buf());
        assert!(adapter.discover_existing().await.is_empty());
    }

    #[tokio::test]
    async fn import_reads_the_discovered_file() {
        let dir = tempfile::tempdir().unwrap();
        let claude_dir = dir.path().join(".claude");
        tokio::fs::create_dir_all(&claude_dir).await.unwrap();
        let creds_path = claude_dir.join(".credentials.json");
        tokio::fs::write(
            &creds_path,
            r#"{"claudeAiOauth":{"accessToken":"XLC-SENTINEL-SECRET","refreshToken":"r"}}"#,
        )
        .await
        .unwrap();
        let adapter = adapter_with_home(dir.path().to_path_buf());
        let found = DiscoveredCredential {
            provider: provider_id(),
            transport: TransportId::new("claude-subscription"),
            account_label: "Claude Code".into(),
            source: creds_path,
        };
        let set = adapter.import(&found).await.unwrap();
        assert_eq!(set.account.auth_kind, AuthKind::Subscription);
    }

    #[tokio::test]
    async fn login_with_api_key_uses_env_var_when_set() {
        // Uses `login_api_key_with_env` (not `std::env::set_var`, `unsafe` since edition 2024 and
        // forbidden workspace-wide) so this never touches the real process environment. Sentinel
        // value so a real key can never leak into test output/logs.
        let adapter =
            ClaudeAuthAdapter::new(reqwest::Client::new(), crate::ClaudeEndpoints::default());
        let set = adapter
            .login_api_key_with_env(&NoopUi, Some("XLC-SENTINEL-SECRET".to_string()))
            .await
            .unwrap();
        match set.secret {
            CredentialSecret::Header { header_name, .. } => {
                assert_eq!(header_name, consts::API_KEY_HEADER);
            }
            other => panic!("expected Header, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn login_reuse_existing_fails_clearly_when_nothing_discovered() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = adapter_with_home(dir.path().to_path_buf());
        let err = adapter
            .login(AuthMethod::ReuseExisting, &NoopUi)
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::OAuth(_)));
    }

    #[tokio::test]
    async fn device_code_login_is_not_implemented() {
        let adapter =
            ClaudeAuthAdapter::new(reqwest::Client::new(), crate::ClaudeEndpoints::default());
        let err = adapter
            .login(AuthMethod::DeviceCode, &NoopUi)
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::NotImplemented(_)));
    }

    #[tokio::test]
    async fn refresh_of_an_api_key_credential_is_a_noop() {
        let adapter =
            ClaudeAuthAdapter::new(reqwest::Client::new(), crate::ClaudeEndpoints::default());
        let set = CredentialSet {
            account: AccountInfo {
                provider: provider_id(),
                transport: TransportId::new("anthropic-api"),
                account_id: "api-key".into(),
                label: None,
                auth_kind: AuthKind::ApiKey,
                metadata: serde_json::json!({}),
            },
            secret: CredentialSecret::Header {
                header_name: consts::API_KEY_HEADER.into(),
                value: SecretString::from("XLC-SENTINEL-SECRET".to_string()),
            },
        };
        let refreshed = adapter.refresh(&set).await.unwrap();
        match refreshed.secret {
            CredentialSecret::Header { header_name, .. } => {
                assert_eq!(header_name, consts::API_KEY_HEADER);
            }
            other => panic!("expected Header, got {other:?}"),
        }
    }
}

#[cfg(all(test, feature = "claude-subscription"))]
mod json_token_tests {
    use super::split_code_and_state;

    #[tokio::test]
    async fn token_request_is_json_and_parses_response() {
        use secrecy::ExposeSecret;
        use wiremock::matchers::{body_json, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body =
            serde_json::json!({"grant_type": "authorization_code", "code": "c1", "state": "s1"});
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .and(header("content-type", "application/json"))
            .and(body_json(&body))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "XLC-SENTINEL-ACCESS",
                "refresh_token": "XLC-SENTINEL-REFRESH",
                "expires_in": 3600,
                "token_type": "Bearer",
                "account": {"uuid": "acct-1", "email_address": "user@example.com"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let url = format!("{}/v1/oauth/token", server.uri());
        let token = super::post_json_token(&reqwest::Client::new(), &url, &body)
            .await
            .unwrap();
        assert_eq!(token.access_token.expose_secret(), "XLC-SENTINEL-ACCESS");
        assert_eq!(token.expires_in, Some(std::time::Duration::from_secs(3600)));
        let creds = super::credential_set_from_token(
            xlightcli_protocol::TransportId::new("claude-subscription"),
            token,
        );
        assert_eq!(creds.account.account_id, "acct-1");
    }

    #[tokio::test]
    async fn token_error_does_not_echo_secrets() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_string(
                r#"{"error":"invalid_grant","echo":"Bearer XLC-SENTINEL-SECRET-abcdefghijklmnop"}"#,
            ))
            .mount(&server)
            .await;
        let err = super::post_json_token(
            &reqwest::Client::new(),
            &server.uri(),
            &serde_json::json!({}),
        )
        .await
        .unwrap_err();
        assert!(!err.to_string().contains("abcdefghijklmnop"), "{err}");
    }

    #[test]
    fn splits_code_hash_state() {
        assert_eq!(split_code_and_state("abc#xyz", "sent"), ("abc", "xyz"));
        assert_eq!(split_code_and_state("abc#", "sent"), ("abc", "sent"));
        assert_eq!(split_code_and_state("abc", "sent"), ("abc", "sent"));
    }
}
