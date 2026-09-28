// SPDX-License-Identifier: GPL-3.0-only
// Portions derived from OpenCodex (MIT) @ 3cc34e1181926b64331490fdcfee162ffb62fe73:
//   src/oauth/chatgpt.ts (`extractAccountId`, `credsFromToken`, `ChatGPTOAuthFlow`),
//   src/oauth/chatgpt-device.ts (device-code grant request/poll shape).
// See THIRD_PARTY.md.

//! `AuthAdapter` impl for Codex: ChatGPT OAuth (browser PKCE + loopback, device-code fallback),
//! `~/.codex/auth.json` discovery/import (D-017, read-only), API-key login for `openai-api`,
//! refresh. Endpoint/header confidence labels: `crate::consts` + `docs/providers/codex.md`.
//!
//! Only this module may call `secrecy::ExposeSecret` (Wave 2 brief): every other module in this
//! crate only ever touches a credential through `xlightcli_auth::CredentialHandle`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use time::OffsetDateTime;
use xlightcli_auth::oauth::{
    AuthorizationUrlParams, LoopbackServer, TokenResponse, build_authorization_url,
    exchange_code_for_token, generate_pkce, refresh_access_token,
};
use xlightcli_auth::{
    AccountInfo, AuthAdapter, AuthError, AuthMethod, CredentialSecret, CredentialSet,
    DiscoveredCredential, LoginUi,
};
use xlightcli_protocol::{AuthKind, ProviderId, TransportId};

use crate::CodexEndpoints;
use crate::consts;

const TRANSPORT_CHATGPT: &str = "chatgpt";
const TRANSPORT_OPENAI_API: &str = "openai-api";
/// Codex's browser-callback and manual-paste redirect flows use a 5-minute window (OpenCodex
/// `callback-server.ts` `DEFAULT_TIMEOUT`); ours matches.
const BROWSER_CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);

fn provider_id() -> ProviderId {
    ProviderId::new("codex")
}

/// `AuthAdapter` for both Codex transports (`chatgpt` subscription OAuth, `openai-api` key).
#[derive(Debug)]
pub(crate) struct CodexAuthAdapter {
    http: reqwest::Client,
    endpoints: CodexEndpoints,
    codex_home: PathBuf,
}

const METHODS: &[AuthMethod] = &[
    AuthMethod::ReuseExisting,
    AuthMethod::BrowserOAuth,
    AuthMethod::DeviceCode,
    AuthMethod::ApiKey,
];

impl CodexAuthAdapter {
    pub(crate) fn new(http: reqwest::Client, endpoints: CodexEndpoints) -> Self {
        Self {
            http,
            endpoints,
            codex_home: crate::default_codex_home(),
        }
    }

    #[cfg(test)]
    fn with_codex_home(mut self, home: PathBuf) -> Self {
        self.codex_home = home;
        self
    }

    fn auth_json_path(&self) -> PathBuf {
        self.codex_home.join("auth.json")
    }

    async fn login_browser(&self, ui: &dyn LoginUi) -> Result<CredentialSet, AuthError> {
        let pkce = generate_pkce();
        let state = random_state();
        let listener = LoopbackServer::bind(
            Some(consts::CHATGPT_OAUTH_CALLBACK_PORT),
            consts::CHATGPT_OAUTH_CALLBACK_PATH,
            consts::CHATGPT_OAUTH_REDIRECT_HOST,
        )
        .await?;
        let redirect_uri = listener.redirect_uri().to_string();
        let url =
            build_chatgpt_authorize_url(&self.endpoints, &pkce.challenge, &state, &redirect_uri)?;
        ui.show_browser_url(url.as_str()).await;
        let callback = listener
            .wait_for_callback(&state, BROWSER_CALLBACK_TIMEOUT)
            .await?;
        let token = exchange_code_for_token(
            &self.http,
            &self.endpoints.chatgpt_oauth_token_url,
            consts::CHATGPT_OAUTH_CLIENT_ID,
            &redirect_uri,
            &callback.code,
            &pkce.verifier,
        )
        .await?;
        chatgpt_credential_set_from_token(token)
    }

    async fn login_device_code(&self, ui: &dyn LoginUi) -> Result<CredentialSet, AuthError> {
        let device = self.request_device_user_code().await?;
        ui.show_device_code(consts::CHATGPT_DEVICE_VERIFICATION_URL, &device.user_code)
            .await;
        let grant = self.poll_device_grant(&device).await?;
        let token = exchange_code_for_token(
            &self.http,
            &self.endpoints.chatgpt_oauth_token_url,
            consts::CHATGPT_OAUTH_CLIENT_ID,
            consts::CHATGPT_DEVICE_REDIRECT_URI,
            &grant.code,
            &grant.verifier,
        )
        .await?;
        chatgpt_credential_set_from_token(token)
    }

    async fn login_api_key(&self, ui: &dyn LoginUi) -> Result<CredentialSet, AuthError> {
        let key = match std::env::var("OPENAI_API_KEY") {
            Ok(value) if !value.is_empty() => SecretString::from(value),
            _ => ui.prompt_api_key(&provider_id()).await?,
        };
        Ok(CredentialSet {
            account: AccountInfo {
                provider: provider_id(),
                transport: TransportId::new(TRANSPORT_OPENAI_API),
                account_id: "api-key".into(),
                label: None,
                auth_kind: AuthKind::ApiKey,
                metadata: serde_json::json!({}),
            },
            secret: CredentialSecret::Bearer {
                access_token: key,
                refresh_token: None,
                expires_at: None,
            },
        })
    }

    /// Step 1 of the device grant (OpenCodex `requestUserCode`): POST `{client_id}` to the
    /// usercode endpoint, get back a `device_auth_id` + short `user_code` to show the user.
    async fn request_device_user_code(&self) -> Result<DeviceUserCode, AuthError> {
        let resp = self
            .http
            .post(&self.endpoints.chatgpt_device_usercode_url)
            .json(&serde_json::json!({ "client_id": consts::CHATGPT_OAUTH_CLIENT_ID }))
            .send()
            .await
            .map_err(|e| AuthError::OAuth(format!("device usercode request failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(AuthError::OAuth(format!(
                "device usercode request failed: HTTP {}",
                resp.status()
            )));
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AuthError::OAuth(format!("device usercode response invalid: {e}")))?;
        parse_device_user_code(&body)
    }

    /// Step 2 (OpenCodex `pollForGrant`): poll until the user finishes at the verification page.
    /// 403/404 means "still pending" — any other non-2xx is terminal. Bounded by the grant's own
    /// 15-minute TTL, not by the caller's timeout.
    async fn poll_device_grant(&self, device: &DeviceUserCode) -> Result<DeviceGrant, AuthError> {
        let deadline = tokio::time::Instant::now() + consts::CHATGPT_DEVICE_FLOW_TTL;
        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(AuthError::OAuth(
                    "ChatGPT device authorization expired".into(),
                ));
            }
            let resp = self
                .http
                .post(&self.endpoints.chatgpt_device_token_url)
                .json(&serde_json::json!({
                    "device_auth_id": device.device_auth_id,
                    "user_code": device.user_code,
                }))
                .send()
                .await
                .map_err(|e| AuthError::OAuth(format!("device poll request failed: {e}")))?;
            match resp.status().as_u16() {
                403 | 404 => {
                    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                    if remaining.is_zero() {
                        continue; // loop head will report the expiry error
                    }
                    tokio::time::sleep(device.interval.min(remaining)).await;
                }
                200 => {
                    let body: serde_json::Value = resp.json().await.map_err(|e| {
                        AuthError::OAuth(format!("device poll response invalid: {e}"))
                    })?;
                    return parse_device_grant(&body);
                }
                status => {
                    return Err(AuthError::OAuth(format!(
                        "device authorization poll failed: HTTP {status}"
                    )));
                }
            }
        }
    }
}

// --- `~/.codex/auth.json` (read-only, D-017 — import & own, never write back) ---

#[derive(Debug, Deserialize)]
struct AuthFile {
    #[serde(rename = "OPENAI_API_KEY", default)]
    openai_api_key: Option<String>,
    #[serde(default)]
    tokens: Option<AuthFileTokens>,
}

#[derive(Debug, Deserialize)]
struct AuthFileTokens {
    #[serde(default)]
    id_token: Option<String>,
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
}

fn read_auth_file(path: &Path) -> Option<AuthFile> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[async_trait]
impl AuthAdapter for CodexAuthAdapter {
    fn methods(&self) -> &[AuthMethod] {
        METHODS
    }

    async fn discover_existing(&self) -> Vec<DiscoveredCredential> {
        let path = self.auth_json_path();
        let Some(file) = read_auth_file(&path) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        if let Some(tokens) = &file.tokens {
            let account_label = tokens
                .account_id
                .clone()
                .or_else(|| {
                    extract_chatgpt_account_id(tokens.id_token.as_deref(), &tokens.access_token)
                })
                .unwrap_or_else(|| "ChatGPT account".to_string());
            found.push(DiscoveredCredential {
                provider: provider_id(),
                transport: TransportId::new(TRANSPORT_CHATGPT),
                account_label,
                source: path.clone(),
            });
        }
        if file.openai_api_key.is_some() {
            found.push(DiscoveredCredential {
                provider: provider_id(),
                transport: TransportId::new(TRANSPORT_OPENAI_API),
                account_label: "OPENAI_API_KEY".to_string(),
                source: path,
            });
        }
        found
    }

    async fn import(&self, found: &DiscoveredCredential) -> Result<CredentialSet, AuthError> {
        let file = read_auth_file(&found.source).ok_or_else(|| {
            AuthError::StoreUnavailable(format!("cannot read {}", found.source.display()))
        })?;
        if found.transport.as_str() == TRANSPORT_OPENAI_API {
            let key = file.openai_api_key.ok_or_else(|| {
                AuthError::StoreUnavailable("OPENAI_API_KEY missing from auth.json".into())
            })?;
            return Ok(CredentialSet {
                account: AccountInfo {
                    provider: provider_id(),
                    transport: TransportId::new(TRANSPORT_OPENAI_API),
                    account_id: "api-key".into(),
                    label: Some("OPENAI_API_KEY (imported from ~/.codex/auth.json)".into()),
                    auth_kind: AuthKind::ApiKey,
                    metadata: serde_json::json!({}),
                },
                secret: CredentialSecret::Bearer {
                    access_token: SecretString::from(key),
                    refresh_token: None,
                    expires_at: None,
                },
            });
        }
        let tokens = file
            .tokens
            .ok_or_else(|| AuthError::StoreUnavailable("no ChatGPT tokens in auth.json".into()))?;
        let account_id = tokens
            .account_id
            .clone()
            .or_else(|| {
                extract_chatgpt_account_id(tokens.id_token.as_deref(), &tokens.access_token)
            })
            .ok_or_else(|| AuthError::OAuth("could not determine ChatGPT account id".into()))?;
        Ok(CredentialSet {
            account: AccountInfo {
                provider: provider_id(),
                transport: TransportId::new(TRANSPORT_CHATGPT),
                account_id,
                label: None,
                auth_kind: AuthKind::Subscription,
                metadata: serde_json::json!({}),
            },
            secret: CredentialSecret::Bearer {
                access_token: SecretString::from(tokens.access_token),
                refresh_token: tokens.refresh_token.map(SecretString::from),
                // `auth.json`'s `last_refresh` isn't a reliable expiry (U); treat as unknown and
                // rely on reactive refresh (401 → refresh once) until proactive refresh lands.
                expires_at: None,
            },
        })
    }

    async fn login(
        &self,
        method: AuthMethod,
        ui: &dyn LoginUi,
    ) -> Result<CredentialSet, AuthError> {
        match method {
            AuthMethod::ReuseExisting => Err(AuthError::OAuth(
                "AuthMethod::ReuseExisting goes through AuthAdapter::import, not login".into(),
            )),
            AuthMethod::BrowserOAuth => self.login_browser(ui).await,
            AuthMethod::DeviceCode => self.login_device_code(ui).await,
            AuthMethod::ApiKey => self.login_api_key(ui).await,
        }
    }

    async fn refresh(&self, current: &CredentialSet) -> Result<CredentialSet, AuthError> {
        if current.account.transport.as_str() != TRANSPORT_CHATGPT {
            return Err(AuthError::OAuth(
                "openai-api credential is a static key; it does not refresh".into(),
            ));
        }
        let CredentialSecret::Bearer {
            refresh_token: Some(refresh_token),
            ..
        } = &current.secret
        else {
            return Err(AuthError::RefreshRejected);
        };
        let token = refresh_access_token(
            &self.http,
            &self.endpoints.chatgpt_oauth_token_url,
            consts::CHATGPT_OAUTH_CLIENT_ID,
            refresh_token,
        )
        .await?;
        let account_id = current.account.account_id.clone();
        Ok(credential_set_from_token(
            TransportId::new(TRANSPORT_CHATGPT),
            AuthKind::Subscription,
            account_id,
            token,
        ))
    }

    async fn revoke(&self, _current: &CredentialSet) -> Result<(), AuthError> {
        // Codex/OpenAI expose no public revoke endpoint we could confirm (U); forgetting the
        // credential locally (AuthBroker's job) is all that's available here.
        Ok(())
    }
}

// --- pure helpers (unit-tested without network) ---

/// Builds the ChatGPT authorization URL for the browser PKCE flow. Extra params match OpenCodex's
/// `ChatGPTOAuthFlow::generateAuthUrl` (MIT, see file header): `codex_cli_simplified_flow` opts
/// into the simplified consent screen Codex CLI uses, `id_token_add_organizations` asks for the
/// `organizations` claim used as an account-id fallback, `originator` identifies this client.
fn build_chatgpt_authorize_url(
    endpoints: &CodexEndpoints,
    code_challenge: &str,
    state: &str,
    redirect_uri: &str,
) -> Result<url::Url, AuthError> {
    build_authorization_url(AuthorizationUrlParams {
        authorize_endpoint: &endpoints.chatgpt_oauth_authorize_url,
        client_id: consts::CHATGPT_OAUTH_CLIENT_ID,
        redirect_uri,
        scope: consts::CHATGPT_OAUTH_SCOPE,
        state,
        code_challenge,
        code_challenge_method: "S256",
        extra_params: &[
            ("codex_cli_simplified_flow", "true"),
            ("id_token_add_organizations", "true"),
            ("originator", consts::ORIGINATOR),
        ],
    })
}

fn random_state() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[derive(Debug, PartialEq, Eq)]
struct DeviceUserCode {
    device_auth_id: String,
    user_code: String,
    interval: Duration,
}

struct DeviceGrant {
    code: SecretString,
    verifier: SecretString,
}

fn parse_device_user_code(body: &serde_json::Value) -> Result<DeviceUserCode, AuthError> {
    let device_auth_id = body.get("device_auth_id").and_then(|v| v.as_str());
    // Upstream accepts both spellings (OpenCodex comment: "the alias must not be rejected").
    let user_code = body
        .get("user_code")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("usercode").and_then(|v| v.as_str()));
    match (device_auth_id, user_code) {
        (Some(device_auth_id), Some(user_code)) => Ok(DeviceUserCode {
            device_auth_id: device_auth_id.to_string(),
            user_code: user_code.to_string(),
            interval: normalize_interval(body.get("interval")),
        }),
        _ => Err(AuthError::OAuth(
            "device authorization response missing required fields".into(),
        )),
    }
}

fn parse_device_grant(body: &serde_json::Value) -> Result<DeviceGrant, AuthError> {
    let code = body.get("authorization_code").and_then(|v| v.as_str());
    let verifier = body.get("code_verifier").and_then(|v| v.as_str());
    match (code, verifier) {
        (Some(code), Some(verifier)) => Ok(DeviceGrant {
            code: SecretString::from(code.to_string()),
            verifier: SecretString::from(verifier.to_string()),
        }),
        _ => Err(AuthError::OAuth(
            "device authorization response missing required fields".into(),
        )),
    }
}

/// Upstream sends `interval` as a number or a string depending on response (OpenCodex comment);
/// coerce, floor at 1s, and cap at the whole grant TTL so a hostile/corrupt value can't turn the
/// poll into a hot loop.
fn normalize_interval(raw: Option<&serde_json::Value>) -> Duration {
    let seconds = match raw {
        Some(serde_json::Value::Number(n)) => n.as_f64(),
        Some(serde_json::Value::String(s)) => s.parse::<f64>().ok(),
        _ => None,
    }
    .filter(|s| s.is_finite() && *s > 0.0)
    .unwrap_or_else(|| consts::CHATGPT_DEVICE_DEFAULT_POLL_INTERVAL.as_secs_f64());
    let clamped = seconds.clamp(
        consts::CHATGPT_DEVICE_MIN_POLL_INTERVAL.as_secs_f64(),
        consts::CHATGPT_DEVICE_FLOW_TTL.as_secs_f64(),
    );
    Duration::from_secs_f64(clamped)
}

/// Decodes a JWT's payload segment **without verifying its signature** — this is only ever used
/// as a client-side routing hint (which ChatGPT account a bearer token was minted for), never as a
/// trust boundary. The token's authenticity is established by upstream 200/401 responses.
fn decode_jwt_payload(token: &str) -> Option<serde_json::Value> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload_b64 = parts.next()?;
    parts.next()?; // need exactly 3 dot-separated segments (signature, unused/unverified here)
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Ported from OpenCodex `extractAccountId` (MIT, `src/oauth/chatgpt.ts`, see file header):
/// prefers `id_token`'s `chatgpt_account_id` claim, falls back to the `https://api.openai.com/auth`
/// namespaced claim, then to `organizations[0].id`; falls back to `access_token`'s claims if
/// `id_token` yields nothing.
fn extract_chatgpt_account_id(id_token: Option<&str>, access_token: &str) -> Option<String> {
    for token in [id_token, Some(access_token)].into_iter().flatten() {
        let Some(payload) = decode_jwt_payload(token) else {
            continue;
        };
        let Some(obj) = payload.as_object() else {
            continue;
        };
        if let Some(id) = obj.get("chatgpt_account_id").and_then(|v| v.as_str()) {
            return Some(id.to_string());
        }
        if let Some(id) = obj
            .get("https://api.openai.com/auth")
            .and_then(|v| v.as_object())
            .and_then(|ns| ns.get("chatgpt_account_id"))
            .and_then(|v| v.as_str())
        {
            return Some(id.to_string());
        }
        if let Some(id) = obj
            .get("organizations")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|o| o.get("id"))
            .and_then(|v| v.as_str())
        {
            return Some(id.to_string());
        }
    }
    None
}

fn chatgpt_credential_set_from_token(token: TokenResponse) -> Result<CredentialSet, AuthError> {
    let id_token = token.raw.get("id_token").and_then(|v| v.as_str());
    let account_id = extract_chatgpt_account_id(id_token, token.access_token.expose_secret())
        .ok_or_else(|| AuthError::OAuth("ChatGPT token response missing account id".into()))?;
    Ok(credential_set_from_token(
        TransportId::new(TRANSPORT_CHATGPT),
        AuthKind::Subscription,
        account_id,
        token,
    ))
}

fn credential_set_from_token(
    transport: TransportId,
    auth_kind: AuthKind,
    account_id: String,
    token: TokenResponse,
) -> CredentialSet {
    let expires_at = token
        .expires_in
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .map(|secs| OffsetDateTime::now_utc() + time::Duration::seconds(secs));
    CredentialSet {
        account: AccountInfo {
            provider: provider_id(),
            transport,
            account_id,
            label: None,
            auth_kind,
            metadata: serde_json::json!({}),
        },
        secret: CredentialSecret::Bearer {
            access_token: token.access_token,
            refresh_token: token.refresh_token,
            expires_at,
        },
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use base64::Engine;
    use pretty_assertions::assert_eq;

    use super::*;

    fn make_jwt(payload: serde_json::Value) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"{\"alg\":\"none\"}");
        let payload_b64 =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string());
        format!("{header}.{payload_b64}.sig")
    }

    // --- authorize URL building ---

    #[test]
    fn authorize_url_has_all_required_and_codex_specific_params() {
        let endpoints = CodexEndpoints::default();
        let url = build_chatgpt_authorize_url(
            &endpoints,
            "challenge-xyz",
            "state-abc",
            "http://localhost:1455/auth/callback",
        )
        .unwrap();
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(
            pairs.get("client_id").unwrap(),
            consts::CHATGPT_OAUTH_CLIENT_ID
        );
        assert_eq!(pairs.get("state").unwrap(), "state-abc");
        assert_eq!(pairs.get("code_challenge").unwrap(), "challenge-xyz");
        assert_eq!(pairs.get("code_challenge_method").unwrap(), "S256");
        assert_eq!(pairs.get("scope").unwrap(), consts::CHATGPT_OAUTH_SCOPE);
        assert_eq!(pairs.get("codex_cli_simplified_flow").unwrap(), "true");
        assert_eq!(pairs.get("id_token_add_organizations").unwrap(), "true");
        assert_eq!(pairs.get("originator").unwrap(), consts::ORIGINATOR);
        assert_eq!(
            pairs.get("redirect_uri").unwrap(),
            "http://localhost:1455/auth/callback"
        );
    }

    #[test]
    fn random_state_is_not_reused() {
        assert_ne!(random_state(), random_state());
    }

    // --- JWT account-id extraction ---

    #[test]
    fn extracts_top_level_chatgpt_account_id() {
        let jwt = make_jwt(serde_json::json!({"chatgpt_account_id": "acc-top"}));
        assert_eq!(
            extract_chatgpt_account_id(Some(&jwt), "unused"),
            Some("acc-top".to_string())
        );
    }

    #[test]
    fn extracts_namespaced_chatgpt_account_id_when_top_level_absent() {
        let jwt = make_jwt(serde_json::json!({
            "https://api.openai.com/auth": {"chatgpt_account_id": "acc-ns"}
        }));
        assert_eq!(
            extract_chatgpt_account_id(Some(&jwt), "unused"),
            Some("acc-ns".to_string())
        );
    }

    #[test]
    fn falls_back_to_first_organization_id() {
        let jwt = make_jwt(serde_json::json!({
            "organizations": [{"id": "org-1"}, {"id": "org-2"}]
        }));
        assert_eq!(
            extract_chatgpt_account_id(Some(&jwt), "unused"),
            Some("org-1".to_string())
        );
    }

    #[test]
    fn falls_back_to_access_token_when_id_token_has_no_markers() {
        let id_token = make_jwt(serde_json::json!({"sub": "irrelevant"}));
        let access_token = make_jwt(serde_json::json!({"chatgpt_account_id": "acc-from-access"}));
        assert_eq!(
            extract_chatgpt_account_id(Some(&id_token), &access_token),
            Some("acc-from-access".to_string())
        );
    }

    #[test]
    fn returns_none_when_neither_token_has_a_marker() {
        let jwt = make_jwt(serde_json::json!({"sub": "irrelevant"}));
        assert_eq!(extract_chatgpt_account_id(Some(&jwt), &jwt), None);
    }

    #[test]
    fn malformed_jwt_does_not_panic() {
        assert_eq!(
            extract_chatgpt_account_id(Some("not-a-jwt"), "also-not"),
            None
        );
    }

    // --- device grant parsing ---

    #[test]
    fn parses_user_code_field() {
        let body =
            serde_json::json!({"device_auth_id": "d1", "user_code": "ABCD-1234", "interval": 5});
        let parsed = parse_device_user_code(&body).unwrap();
        assert_eq!(parsed.device_auth_id, "d1");
        assert_eq!(parsed.user_code, "ABCD-1234");
        assert_eq!(parsed.interval, Duration::from_secs(5));
    }

    #[test]
    fn accepts_usercode_alias() {
        let body = serde_json::json!({"device_auth_id": "d1", "usercode": "ABCD-1234"});
        let parsed = parse_device_user_code(&body).unwrap();
        assert_eq!(parsed.user_code, "ABCD-1234");
    }

    #[test]
    fn missing_fields_are_rejected() {
        let body = serde_json::json!({"device_auth_id": "d1"});
        assert!(parse_device_user_code(&body).is_err());
    }

    #[test]
    fn interval_defaults_when_absent_or_non_numeric() {
        assert_eq!(
            normalize_interval(None),
            consts::CHATGPT_DEVICE_DEFAULT_POLL_INTERVAL
        );
        assert_eq!(
            normalize_interval(Some(&serde_json::json!("not-a-number"))),
            consts::CHATGPT_DEVICE_DEFAULT_POLL_INTERVAL
        );
    }

    #[test]
    fn interval_is_clamped_to_the_grant_ttl() {
        assert_eq!(
            normalize_interval(Some(&serde_json::json!(999_999))),
            consts::CHATGPT_DEVICE_FLOW_TTL
        );
        assert_eq!(
            normalize_interval(Some(&serde_json::json!(0))),
            consts::CHATGPT_DEVICE_DEFAULT_POLL_INTERVAL
        );
    }

    #[test]
    fn interval_accepts_string_form() {
        assert_eq!(
            normalize_interval(Some(&serde_json::json!("3"))),
            Duration::from_secs(3)
        );
    }

    #[test]
    fn parses_authorization_code_grant() {
        let body =
            serde_json::json!({"authorization_code": "code-1", "code_verifier": "verifier-1"});
        let grant = parse_device_grant(&body).unwrap();
        assert_eq!(grant.code.expose_secret(), "code-1");
        assert_eq!(grant.verifier.expose_secret(), "verifier-1");
    }

    #[test]
    fn device_grant_missing_fields_is_rejected() {
        let body = serde_json::json!({"authorization_code": "code-1"});
        assert!(parse_device_grant(&body).is_err());
    }

    // --- auth.json discovery/import (tempdir fixtures only — never the real user file) ---

    fn adapter_with_home(home: &Path) -> CodexAuthAdapter {
        CodexAuthAdapter::new(reqwest::Client::new(), CodexEndpoints::default())
            .with_codex_home(home.to_path_buf())
    }

    #[tokio::test]
    async fn discover_existing_is_empty_when_auth_json_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = adapter_with_home(dir.path());
        assert!(adapter.discover_existing().await.is_empty());
    }

    #[tokio::test]
    async fn discover_existing_finds_chatgpt_tokens() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("auth.json"),
            serde_json::json!({
                "tokens": {
                    "access_token": "XLC-SENTINEL-ACCESS",
                    "refresh_token": "XLC-SENTINEL-REFRESH",
                    "account_id": "acc-fixture"
                }
            })
            .to_string(),
        )
        .unwrap();
        let adapter = adapter_with_home(dir.path());
        let found = adapter.discover_existing().await;
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].transport, TransportId::new(TRANSPORT_CHATGPT));
        assert_eq!(found[0].account_label, "acc-fixture");
    }

    #[tokio::test]
    async fn discover_existing_finds_both_transports_when_both_present() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("auth.json"),
            serde_json::json!({
                "OPENAI_API_KEY": "sk-fixture",
                "tokens": {"access_token": "XLC-SENTINEL-ACCESS", "account_id": "acc-fixture"}
            })
            .to_string(),
        )
        .unwrap();
        let adapter = adapter_with_home(dir.path());
        let found = adapter.discover_existing().await;
        assert_eq!(found.len(), 2);
    }

    #[tokio::test]
    async fn import_reads_chatgpt_tokens_into_a_credential_set() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "tokens": {
                    "access_token": "XLC-SENTINEL-ACCESS",
                    "refresh_token": "XLC-SENTINEL-REFRESH",
                    "account_id": "acc-fixture"
                }
            })
            .to_string(),
        )
        .unwrap();
        let adapter = adapter_with_home(dir.path());
        let found = DiscoveredCredential {
            provider: provider_id(),
            transport: TransportId::new(TRANSPORT_CHATGPT),
            account_label: "acc-fixture".into(),
            source: path,
        };
        let set = adapter.import(&found).await.unwrap();
        assert_eq!(set.account.account_id, "acc-fixture");
        assert_eq!(set.account.auth_kind, AuthKind::Subscription);
        match set.secret {
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

    #[tokio::test]
    async fn import_falls_back_to_jwt_derived_account_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let id_token = make_jwt(serde_json::json!({"chatgpt_account_id": "acc-from-jwt"}));
        std::fs::write(
            &path,
            serde_json::json!({
                "tokens": {"access_token": "XLC-SENTINEL-ACCESS", "id_token": id_token}
            })
            .to_string(),
        )
        .unwrap();
        let adapter = adapter_with_home(dir.path());
        let found = DiscoveredCredential {
            provider: provider_id(),
            transport: TransportId::new(TRANSPORT_CHATGPT),
            account_label: "unused".into(),
            source: path,
        };
        let set = adapter.import(&found).await.unwrap();
        assert_eq!(set.account.account_id, "acc-from-jwt");
    }

    #[tokio::test]
    async fn import_reads_api_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        std::fs::write(
            &path,
            serde_json::json!({"OPENAI_API_KEY": "sk-fixture"}).to_string(),
        )
        .unwrap();
        let adapter = adapter_with_home(dir.path());
        let found = DiscoveredCredential {
            provider: provider_id(),
            transport: TransportId::new(TRANSPORT_OPENAI_API),
            account_label: "OPENAI_API_KEY".into(),
            source: path,
        };
        let set = adapter.import(&found).await.unwrap();
        assert_eq!(set.account.auth_kind, AuthKind::ApiKey);
        match set.secret {
            CredentialSecret::Bearer { access_token, .. } => {
                assert_eq!(access_token.expose_secret(), "sk-fixture");
            }
            CredentialSecret::Header { .. } => panic!("expected Bearer secret"),
        }
    }

    #[tokio::test]
    async fn refresh_rejects_openai_api_credential() {
        let adapter = CodexAuthAdapter::new(reqwest::Client::new(), CodexEndpoints::default());
        let current = CredentialSet {
            account: AccountInfo {
                provider: provider_id(),
                transport: TransportId::new(TRANSPORT_OPENAI_API),
                account_id: "api-key".into(),
                label: None,
                auth_kind: AuthKind::ApiKey,
                metadata: serde_json::json!({}),
            },
            secret: CredentialSecret::Bearer {
                access_token: SecretString::from("sk-fixture".to_string()),
                refresh_token: None,
                expires_at: None,
            },
        };
        let err = adapter.refresh(&current).await.unwrap_err();
        assert!(matches!(err, AuthError::OAuth(_)));
    }

    #[tokio::test]
    async fn refresh_rejects_credential_with_no_refresh_token() {
        let adapter = CodexAuthAdapter::new(reqwest::Client::new(), CodexEndpoints::default());
        let current = CredentialSet {
            account: AccountInfo {
                provider: provider_id(),
                transport: TransportId::new(TRANSPORT_CHATGPT),
                account_id: "acc".into(),
                label: None,
                auth_kind: AuthKind::Subscription,
                metadata: serde_json::json!({}),
            },
            secret: CredentialSecret::Bearer {
                access_token: SecretString::from("XLC-SENTINEL-ACCESS".to_string()),
                refresh_token: None,
                expires_at: None,
            },
        };
        let err = adapter.refresh(&current).await.unwrap_err();
        assert!(matches!(err, AuthError::RefreshRejected));
    }

    #[tokio::test]
    async fn login_reuse_existing_is_rejected() {
        struct NoopUi;
        #[async_trait::async_trait]
        impl LoginUi for NoopUi {
            async fn show_browser_url(&self, _url: &str) {}
            async fn show_device_code(&self, _verification_uri: &str, _user_code: &str) {}
            async fn prompt_api_key(
                &self,
                _provider: &ProviderId,
            ) -> Result<SecretString, AuthError> {
                Ok(SecretString::from("unused".to_string()))
            }
        }
        let adapter = CodexAuthAdapter::new(reqwest::Client::new(), CodexEndpoints::default());
        let err = adapter
            .login(AuthMethod::ReuseExisting, &NoopUi)
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::OAuth(_)));
    }
}
