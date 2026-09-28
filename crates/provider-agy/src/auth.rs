// SPDX-License-Identifier: GPL-3.0-only
// Portions derived from OpenCodex (MIT) @ 3cc34e1181926b64331490fdcfee162ffb62fe73:
// src/oauth/google-antigravity.ts, src/oauth/account-import/google-antigravity-adapter.ts,
// src/adapters/client-fingerprint.ts
// See THIRD_PARTY.md.

//! `AuthAdapter` impl for Antigravity/Gemini: API key entry for `gemini-api`; Google OAuth (PKCE)
//! for the `antigravity` transport; discovery/import (D-017) — currently a documented no-op, see
//! below; refresh/revoke.
//!
//! OAuth flow (PKCE, loopback callback, `loadCodeAssist`/`onboardUser` project discovery, token
//! exchange/refresh) is ported (shape, not code) from OpenCodex (MIT)
//! `@ 3cc34e1181926b64331490fdcfee162ffb62fe73` `src/oauth/google-antigravity.ts`. See
//! `THIRD_PARTY.md`. The PKCE/authorization-URL/loopback plumbing itself calls into
//! `xlightcli_auth::oauth`, which is filled in concurrently by the `auth` crate owner — this
//! module's logic is written against that crate's fixed public signatures (`docs/CONTRACTS.md`
//! §2) and will start working end to end once those stubs are implemented, with no changes here.

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use time::OffsetDateTime;
use xlightcli_auth::{
    AccountInfo, AuthAdapter, AuthError, AuthMethod, CredentialSecret, CredentialSet,
    DiscoveredCredential, LoginUi, oauth,
};
use xlightcli_protocol::{AuthKind, ProviderId, TransportId};

use crate::consts::{antigravity, gemini_api};

const METHODS: &[AuthMethod] = &[AuthMethod::ApiKey, AuthMethod::BrowserOAuth];

#[derive(Debug)]
pub(crate) struct AgyAuthAdapter {
    http: reqwest::Client,
}

impl AgyAuthAdapter {
    pub(crate) fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    async fn login_api_key(&self, ui: &dyn LoginUi) -> Result<CredentialSet, AuthError> {
        // docs/import.md: `GEMINI_API_KEY` is a recognized credential source for agy — honor it
        // before falling back to an interactive prompt.
        let key = match std::env::var(gemini_api::API_KEY_ENV) {
            Ok(v) if !v.is_empty() => SecretString::from(v),
            _ => ui.prompt_api_key(&ProviderId::new("agy")).await?,
        };
        Ok(CredentialSet {
            account: AccountInfo {
                provider: ProviderId::new("agy"),
                transport: TransportId::new("gemini-api"),
                account_id: "api-key".into(),
                label: None,
                auth_kind: AuthKind::ApiKey,
                metadata: serde_json::json!({}),
            },
            secret: CredentialSecret::Header {
                header_name: gemini_api::API_KEY_HEADER.into(),
                value: key,
            },
        })
    }

    async fn login_browser_oauth(&self, ui: &dyn LoginUi) -> Result<CredentialSet, AuthError> {
        let client = OAuthClient::from_env()?;
        let pkce = oauth::generate_pkce();
        let state = uuid::Uuid::new_v4().to_string();
        let server = oauth::LoopbackServer::bind(
            Some(antigravity::CALLBACK_PORT),
            antigravity::CALLBACK_PATH,
            antigravity::CALLBACK_HOST,
        )
        .await?;
        let redirect_uri = server.redirect_uri().to_string();
        let scope = antigravity::SCOPES.join(" ");
        let auth_url = oauth::build_authorization_url(oauth::AuthorizationUrlParams {
            authorize_endpoint: antigravity::GOOGLE_AUTH_ENDPOINT,
            client_id: &client.id,
            redirect_uri: &redirect_uri,
            scope: &scope,
            state: &state,
            code_challenge: &pkce.challenge,
            code_challenge_method: pkce.challenge_method,
            extra_params: &[("access_type", "offline"), ("prompt", "consent")],
        })?;
        ui.show_browser_url(auth_url.as_str()).await;
        let callback = server
            .wait_for_callback(&state, std::time::Duration::from_secs(300))
            .await?;
        let token = oauth::exchange_code_for_token_with_secret(
            &self.http,
            antigravity::GOOGLE_TOKEN_ENDPOINT,
            &client.id,
            Some(&client.secret),
            &redirect_uri,
            &callback.code,
            &pkce.verifier,
        )
        .await?;

        let project_id = self.discover_project(&token.access_token).await;
        if project_id.is_none() {
            tracing::warn!(
                "agy: antigravity login could not discover a Cloud Code Assist project id; set {} \
                 (or AgyEndpoints::antigravity_project_id) manually (see docs/providers/agy.md)",
                antigravity::PROJECT_ID_ENV
            );
        }
        let email = self.fetch_userinfo_email(&token.access_token).await;
        let expires_at = token
            .expires_in
            .and_then(|d| time::Duration::try_from(d).ok())
            .map(|d| OffsetDateTime::now_utc() + d);

        Ok(CredentialSet {
            account: AccountInfo {
                provider: ProviderId::new("agy"),
                transport: TransportId::new("antigravity"),
                account_id: email.clone().unwrap_or_else(|| "unknown".into()),
                label: email,
                auth_kind: AuthKind::Subscription,
                metadata: project_id_metadata(project_id),
            },
            secret: CredentialSecret::Bearer {
                access_token: token.access_token,
                refresh_token: token.refresh_token,
                expires_at,
            },
        })
    }

    /// Best-effort `loadCodeAssist` → `onboardUser` discovery (single attempt; the ported source
    /// polls `onboardUser` up to 5 times with 429/5xx backoff — not modeled here, documented gap).
    async fn discover_project(&self, access_token: &SecretString) -> Option<String> {
        if let Some(id) = self.load_code_assist_project(access_token).await {
            return Some(id);
        }
        self.onboard_project(access_token).await
    }

    async fn load_code_assist_project(&self, access_token: &SecretString) -> Option<String> {
        let url = format!(
            "{}/{}:loadCodeAssist",
            antigravity::CCA_PROD_BASE_URL,
            antigravity::CCA_API_VERSION
        );
        let response = self
            .http
            .post(&url)
            .header(
                http::header::AUTHORIZATION,
                format!("Bearer {}", access_token.expose_secret()),
            )
            .header(http::header::USER_AGENT, antigravity::request_user_agent())
            .json(&serde_json::json!({ "metadata": { "ideType": "ANTIGRAVITY" } }))
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let body: Value = response.json().await.ok()?;
        extract_project_id(&body)
    }

    async fn onboard_project(&self, access_token: &SecretString) -> Option<String> {
        let url = format!(
            "{}/{}:onboardUser",
            antigravity::CCA_DAILY_BASE_URL,
            antigravity::CCA_API_VERSION
        );
        let response = self
            .http
            .post(&url)
            .header(
                http::header::AUTHORIZATION,
                format!("Bearer {}", access_token.expose_secret()),
            )
            .header(http::header::USER_AGENT, antigravity::request_user_agent())
            .json(&serde_json::json!({
                "tier_id": "free-tier",
                "metadata": {
                    "ide_type": "ANTIGRAVITY",
                    "ide_name": "antigravity",
                    "ide_version": antigravity::IDE_VERSION,
                }
            }))
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let body: Value = response.json().await.ok()?;
        if body.get("done").and_then(Value::as_bool) != Some(true) {
            return None;
        }
        extract_project_id(body.get("response")?)
    }

    async fn fetch_userinfo_email(&self, access_token: &SecretString) -> Option<String> {
        let response = self
            .http
            .get(antigravity::GOOGLE_USERINFO_ENDPOINT)
            .header(
                http::header::AUTHORIZATION,
                format!("Bearer {}", access_token.expose_secret()),
            )
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let body: Value = response.json().await.ok()?;
        body.get("email")
            .and_then(Value::as_str)
            .map(str::to_lowercase)
    }
}

/// The Antigravity OAuth client, read from the environment at login/refresh time
/// (`consts::antigravity::OAUTH_CLIENT_ID_ENV` / `OAUTH_CLIENT_SECRET_ENV`). Missing values fail
/// with a clear error instead of a confusing upstream `invalid_client` (INV-10).
struct OAuthClient {
    id: String,
    secret: SecretString,
}

impl OAuthClient {
    fn from_env() -> Result<Self, AuthError> {
        Self::from_values(
            std::env::var(antigravity::OAUTH_CLIENT_ID_ENV).ok(),
            std::env::var(antigravity::OAUTH_CLIENT_SECRET_ENV).ok(),
        )
    }

    fn from_values(id: Option<String>, secret: Option<String>) -> Result<Self, AuthError> {
        let non_empty = |v: Option<String>| v.filter(|v| !v.trim().is_empty());
        match (non_empty(id), non_empty(secret)) {
            (Some(id), Some(secret)) => Ok(Self {
                id: id.trim().to_owned(),
                secret: SecretString::from(secret.trim().to_owned()),
            }),
            _ => Err(AuthError::OAuth(format!(
                "the antigravity transport needs the Antigravity desktop OAuth client: set {} and \
                 {} (see docs/providers/agy.md, \"OAuth client\")",
                antigravity::OAUTH_CLIENT_ID_ENV,
                antigravity::OAUTH_CLIENT_SECRET_ENV
            ))),
        }
    }
}

/// Builds `AccountInfo.metadata` carrying the discovered project id, if any, under
/// `consts::antigravity::PROJECT_ID_METADATA_KEY`.
fn project_id_metadata(project_id: Option<String>) -> Value {
    match project_id {
        Some(id) => serde_json::json!({ antigravity::PROJECT_ID_METADATA_KEY: id }),
        None => serde_json::json!({}),
    }
}

/// Pulls a Cloud Code Assist project id out of a `loadCodeAssist`/`onboardUser` response shape.
fn extract_project_id(data: &Value) -> Option<String> {
    for key in ["cloudaicompanionProject", "projectId", "project"] {
        let Some(value) = data.get(key) else { continue };
        if let Some(s) = value.as_str().filter(|s| !s.is_empty()) {
            return Some(s.to_string());
        }
        if let Some(id) = value.get("id").and_then(Value::as_str) {
            return Some(id.to_string());
        }
    }
    None
}

#[async_trait]
impl AuthAdapter for AgyAuthAdapter {
    fn methods(&self) -> &[AuthMethod] {
        METHODS
    }

    async fn discover_existing(&self) -> Vec<DiscoveredCredential> {
        // docs/import.md: agy's own OAuth token storage location is unverified (U) — keyring or
        // file, and which, is unknown. OpenCodex's account-import adapter
        // (`oauth/account-import/google-antigravity-adapter.ts`) only proves importing a refresh
        // token handed in by an external "Cockpit" record, not reading a well-known local file —
        // there is nothing safe to discover from disk yet. Empty and documented, not guessed
        // (PATTERNS.md §5, INV-1: never spawn/read the official CLI's private state blind).
        Vec::new()
    }

    async fn import(&self, _found: &DiscoveredCredential) -> Result<CredentialSet, AuthError> {
        Err(AuthError::NotImplemented(
            "agy: discover_existing never returns candidates yet (credential location unverified, see docs/providers/agy.md)",
        ))
    }

    async fn login(
        &self,
        method: AuthMethod,
        ui: &dyn LoginUi,
    ) -> Result<CredentialSet, AuthError> {
        match method {
            AuthMethod::ApiKey => self.login_api_key(ui).await,
            AuthMethod::BrowserOAuth => self.login_browser_oauth(ui).await,
            AuthMethod::DeviceCode | AuthMethod::ReuseExisting => Err(AuthError::NotImplemented(
                "agy: only ApiKey (gemini-api) and BrowserOAuth (antigravity) login are supported",
            )),
        }
    }

    async fn refresh(&self, current: &CredentialSet) -> Result<CredentialSet, AuthError> {
        let CredentialSecret::Bearer {
            refresh_token: Some(refresh_token),
            ..
        } = &current.secret
        else {
            return Err(AuthError::RefreshRejected);
        };
        let client = OAuthClient::from_env()?;
        let token = oauth::refresh_access_token_with_secret(
            &self.http,
            antigravity::GOOGLE_TOKEN_ENDPOINT,
            &client.id,
            Some(&client.secret),
            refresh_token,
        )
        .await?;
        let expires_at = token
            .expires_in
            .and_then(|d| time::Duration::try_from(d).ok())
            .map(|d| OffsetDateTime::now_utc() + d);
        let refresh_token = token
            .refresh_token
            .clone()
            .unwrap_or_else(|| refresh_token.clone());

        // Re-discover the project on refresh, mirroring the ported source (a newly-onboarded
        // account only gets a project id at this point). NOTE: `AuthBroker::run_refresh`
        // (crates/auth/src/handle.rs) currently only takes `refreshed.secret`, not
        // `refreshed.account`, from what an adapter's `refresh()` returns — so this updated
        // metadata is *not yet* propagated into the live `AccountEntry`/`AccountIndex` by the
        // broker. Returned here anyway so the fix is a broker-side change only, not an adapter one,
        // once that's picked up (residual gap, see docs/providers/agy.md).
        let mut account = current.account.clone();
        if account
            .metadata
            .get(antigravity::PROJECT_ID_METADATA_KEY)
            .is_none()
            && let Some(project_id) = self.discover_project(&token.access_token).await
        {
            account.metadata = project_id_metadata(Some(project_id));
        }

        Ok(CredentialSet {
            account,
            secret: CredentialSecret::Bearer {
                access_token: token.access_token,
                refresh_token: Some(refresh_token),
                expires_at,
            },
        })
    }

    async fn revoke(&self, current: &CredentialSet) -> Result<(), AuthError> {
        let token = match &current.secret {
            CredentialSecret::Bearer { access_token, .. } => access_token,
            CredentialSecret::Header { value, .. } => value,
        };
        // Best-effort: a revoke-endpoint failure shouldn't block the local logout (the secret
        // store entry is deleted by the caller regardless).
        if let Err(err) = self
            .http
            .post(antigravity::GOOGLE_REVOKE_ENDPOINT)
            .form(&[("token", token.expose_secret())])
            .send()
            .await
        {
            tracing::warn!(error = %err, "agy: token revocation request failed (best-effort, ignored)");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use async_trait::async_trait;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use xlightcli_auth::AuthError as AuthErrorT;

    use super::*;

    #[test]
    fn extract_project_id_reads_string_and_nested_id_forms() {
        assert_eq!(
            extract_project_id(&json!({"projectId": "proj-1"})),
            Some("proj-1".into())
        );
        assert_eq!(
            extract_project_id(&json!({"cloudaicompanionProject": {"id": "proj-2"}})),
            Some("proj-2".into())
        );
        assert_eq!(extract_project_id(&json!({})), None);
        assert_eq!(extract_project_id(&json!({"projectId": ""})), None);
    }

    struct FakeUi {
        api_key: &'static str,
    }

    #[async_trait]
    impl LoginUi for FakeUi {
        async fn show_browser_url(&self, _url: &str) {}
        async fn show_device_code(&self, _verification_uri: &str, _user_code: &str) {}
        async fn prompt_api_key(&self, _provider: &ProviderId) -> Result<SecretString, AuthErrorT> {
            Ok(SecretString::from(self.api_key.to_string()))
        }
    }

    #[tokio::test]
    async fn login_api_key_builds_a_header_credential_for_gemini_api() {
        let adapter = AgyAuthAdapter::new(reqwest::Client::new());
        let ui = FakeUi {
            api_key: "XLC-SENTINEL-SECRET",
        };
        let cred = adapter.login(AuthMethod::ApiKey, &ui).await.unwrap();
        assert_eq!(cred.account.transport, TransportId::new("gemini-api"));
        match cred.secret {
            CredentialSecret::Header { header_name, value } => {
                assert_eq!(header_name, "x-goog-api-key");
                assert_eq!(value.expose_secret(), "XLC-SENTINEL-SECRET");
            }
            other => panic!("expected Header secret, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn device_code_login_is_not_implemented() {
        let adapter = AgyAuthAdapter::new(reqwest::Client::new());
        let ui = FakeUi { api_key: "unused" };
        let err = adapter
            .login(AuthMethod::DeviceCode, &ui)
            .await
            .unwrap_err();
        assert!(matches!(err, AuthErrorT::NotImplemented(_)));
    }

    #[tokio::test]
    async fn discover_existing_is_empty() {
        let adapter = AgyAuthAdapter::new(reqwest::Client::new());
        assert!(adapter.discover_existing().await.is_empty());
    }

    #[test]
    fn methods_lists_api_key_and_browser_oauth() {
        let adapter = AgyAuthAdapter::new(reqwest::Client::new());
        assert_eq!(
            adapter.methods(),
            &[AuthMethod::ApiKey, AuthMethod::BrowserOAuth]
        );
    }
}

#[cfg(test)]
mod oauth_client_tests {
    use secrecy::ExposeSecret;

    use super::OAuthClient;

    #[test]
    fn missing_or_blank_values_fail_with_the_env_var_names() {
        for (id, secret) in [
            (None, None),
            (Some("id".to_string()), None),
            (Some("  ".to_string()), Some("s".to_string())),
        ] {
            let err = OAuthClient::from_values(id, secret)
                .err()
                .map(|e| e.to_string());
            let err = err.unwrap_or_default();
            assert!(
                err.contains("XLIGHTCLI_ANTIGRAVITY_OAUTH_CLIENT_ID"),
                "{err}"
            );
        }
    }

    #[test]
    fn values_are_trimmed() {
        let c = OAuthClient::from_values(Some(" id \n".into()), Some(" s ".into())).unwrap();
        assert_eq!(c.id, "id");
        assert_eq!(c.secret.expose_secret(), "s");
    }
}
