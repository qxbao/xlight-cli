// SPDX-License-Identifier: GPL-3.0-only

//! OAuth toolkit shared by every provider adapter (docs/PLAN.md §6, §4.5 port map).
//!
//! PKCE generation and the authorization URL builder are plain, IO-free logic. The loopback
//! callback server, device-code flow and token exchange/refresh HTTP calls are IO-heavy but still
//! provider-agnostic: adapters supply endpoints/client id, this module handles the wire mechanics
//! (RFC 6749 token requests, RFC 8628 device flow, a minimal single-shot HTTP/1.1 loopback
//! server). Errors never include a token value or a full response body — only a redacted excerpt
//! (docs/PLAN.md §14).

use std::collections::HashMap;
use std::time::Duration;

use rand::RngExt;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use sha2::Digest;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use url::Url;

use crate::error::AuthError;

/// PKCE (RFC 7636) verifier + S256 challenge pair. `Debug` is safe to derive: `SecretString`
/// redacts itself.
#[derive(Debug)]
pub struct PkceCodes {
    pub verifier: SecretString,
    /// Base64url (no padding) of `SHA256(verifier)`.
    pub challenge: String,
    /// Always `"S256"` — plain-text PKCE is not supported (security rules, docs/PLAN.md §14).
    pub challenge_method: &'static str,
}

const PKCE_VERIFIER_LEN: usize = 64;
const PKCE_CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";

/// Generates a new random PKCE verifier/challenge pair (RFC 7636 §4.1: 43-128 chars from the
/// unreserved character set; we use 64).
pub fn generate_pkce() -> PkceCodes {
    let mut rng = rand::rng();
    let verifier: String = (0..PKCE_VERIFIER_LEN)
        .map(|_| {
            let idx = rng.random_range(0..PKCE_CHARSET.len());
            PKCE_CHARSET[idx] as char
        })
        .collect();
    let challenge = challenge_from_verifier(&verifier);
    PkceCodes {
        verifier: SecretString::from(verifier),
        challenge,
        challenge_method: "S256",
    }
}

fn challenge_from_verifier(verifier: &str) -> String {
    use base64::Engine;
    let digest = sha2::Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// Inputs for `build_authorization_url`. `extra_params` covers provider-specific fields (e.g.
/// `resource`, `prompt`) without growing this struct per provider.
#[derive(Debug)]
pub struct AuthorizationUrlParams<'a> {
    pub authorize_endpoint: &'a str,
    pub client_id: &'a str,
    pub redirect_uri: &'a str,
    pub scope: &'a str,
    pub state: &'a str,
    pub code_challenge: &'a str,
    pub code_challenge_method: &'a str,
    pub extra_params: &'a [(&'a str, &'a str)],
}

/// Builds the `response_type=code` authorization URL for a PKCE browser flow.
pub fn build_authorization_url(params: AuthorizationUrlParams<'_>) -> Result<Url, AuthError> {
    let mut url = Url::parse(params.authorize_endpoint)
        .map_err(|e| AuthError::OAuth(format!("invalid authorize endpoint: {e}")))?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", params.client_id)
            .append_pair("redirect_uri", params.redirect_uri)
            .append_pair("scope", params.scope)
            .append_pair("state", params.state)
            .append_pair("code_challenge", params.code_challenge)
            .append_pair("code_challenge_method", params.code_challenge_method);
        for (key, value) in params.extra_params {
            query.append_pair(key, value);
        }
    }
    Ok(url)
}

/// Result of a completed loopback (or device-code) redirect: the authorization code plus the
/// `state` the caller must verify against what it sent.
#[derive(Debug, Clone)]
pub struct AuthorizationCallback {
    pub code: SecretString,
    pub state: String,
}

/// Convenience one-shot: binds an ephemeral loopback listener, waits for the callback, and
/// verifies `state` — for callers that don't need to know the `redirect_uri` in advance (most
/// providers register a fixed one, in which case use `LoopbackServer::bind` directly instead so
/// the authorization URL can be built with the right port).
pub async fn run_loopback_callback_server(
    expected_state: &str,
    timeout: Duration,
) -> Result<AuthorizationCallback, AuthError> {
    let server = LoopbackServer::bind(None, "/callback", "127.0.0.1").await?;
    server.wait_for_callback(expected_state, timeout).await
}

/// Single-shot loopback redirect listener, split into bind + wait so the caller knows the
/// `redirect_uri` before building the authorization URL (providers register fixed ports/paths).
///
/// Always binds `127.0.0.1`; `redirect_host` only affects the advertised URI (some providers
/// register `localhost` instead of the IP literal). Security rules (docs/PLAN.md §14): binds
/// loopback only, accepts exactly one request, verifies `state`, closes immediately after.
#[derive(Debug)]
pub struct LoopbackServer {
    redirect_uri: String,
    listener: TcpListener,
    callback_path: String,
}

impl LoopbackServer {
    /// `port: None` picks an ephemeral port (OS-assigned, port `0`).
    pub async fn bind(
        port: Option<u16>,
        callback_path: &str,
        redirect_host: &str,
    ) -> Result<Self, AuthError> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port.unwrap_or(0)))
            .await
            .map_err(|e| match (e.kind(), port) {
                (std::io::ErrorKind::AddrInUse, Some(p)) => AuthError::OAuth(format!(
                    "OAuth callback port {p} is already in use by another process"
                )),
                _ => AuthError::OAuth(format!("failed to bind loopback listener: {e}")),
            })?;
        let actual_port = listener
            .local_addr()
            .map_err(|e| AuthError::OAuth(format!("failed to read loopback local address: {e}")))?
            .port();
        Ok(Self {
            redirect_uri: format!("http://{redirect_host}:{actual_port}{callback_path}"),
            listener,
            callback_path: callback_path.to_owned(),
        })
    }

    /// e.g. `http://localhost:1455/auth/callback`.
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Accepts exactly one request on `callback_path`, verifies `state` (constant-time compare),
    /// replies with a small "you can close this tab" page, then closes — regardless of whether
    /// the request itself succeeded, so the listener never lingers.
    pub async fn wait_for_callback(
        self,
        expected_state: &str,
        timeout: Duration,
    ) -> Result<AuthorizationCallback, AuthError> {
        let Self {
            listener,
            callback_path,
            ..
        } = self;
        let callback =
            match tokio::time::timeout(timeout, accept_one(listener, &callback_path)).await {
                Ok(result) => result?,
                Err(_) => {
                    return Err(AuthError::OAuth(
                        "timed out waiting for the OAuth loopback callback".into(),
                    ));
                }
            };
        if !constant_time_eq(&callback.state, expected_state) {
            return Err(AuthError::OAuth(
                "OAuth loopback callback state mismatch (possible CSRF)".into(),
            ));
        }
        Ok(callback)
    }
}

const SUCCESS_HTML: &str =
    "<!doctype html><html><body><p>Login complete — you can close this tab.</p></body></html>";

async fn accept_one(
    listener: TcpListener,
    callback_path: &str,
) -> Result<AuthorizationCallback, AuthError> {
    let (mut stream, _) = listener
        .accept()
        .await
        .map_err(|e| AuthError::OAuth(format!("loopback accept failed: {e}")))?;

    let request_line = read_request_line(&mut stream).await?;
    let (method, target) = parse_request_line(&request_line)?;

    if method != "GET" {
        write_response(&mut stream, 405, "Method Not Allowed").await;
        return Err(AuthError::OAuth(format!(
            "loopback callback received non-GET method {method:?}"
        )));
    }

    let url = Url::parse(&format!("http://loopback.invalid{target}"))
        .map_err(|e| AuthError::OAuth(format!("malformed loopback request target: {e}")))?;
    if url.path() != callback_path {
        write_response(&mut stream, 404, "Not Found").await;
        return Err(AuthError::OAuth(format!(
            "loopback callback received unexpected path {:?}",
            url.path()
        )));
    }

    let params: HashMap<String, String> = url.query_pairs().into_owned().collect();
    if let Some(error) = params.get("error") {
        write_response(&mut stream, 400, "Authorization failed").await;
        return Err(AuthError::OAuth(format!(
            "authorization server returned error: {error}"
        )));
    }
    let code = params
        .get("code")
        .cloned()
        .ok_or_else(|| AuthError::OAuth("loopback callback missing `code`".into()))?;
    let state = params
        .get("state")
        .cloned()
        .ok_or_else(|| AuthError::OAuth("loopback callback missing `state`".into()))?;

    write_response(&mut stream, 200, SUCCESS_HTML).await;
    Ok(AuthorizationCallback {
        code: SecretString::from(code),
        state,
    })
}

async fn read_request_line(stream: &mut TcpStream) -> Result<String, AuthError> {
    let mut reader = BufReader::new(&mut *stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .map_err(|e| AuthError::OAuth(format!("loopback read failed: {e}")))?;
    // Drain the remaining request headers (we don't need them) so the client isn't left with a
    // half-written request when we start writing the response.
    loop {
        let mut header_line = String::new();
        let n = reader.read_line(&mut header_line).await.unwrap_or(0);
        if n == 0 || header_line.trim().is_empty() {
            break;
        }
    }
    Ok(line)
}

fn parse_request_line(line: &str) -> Result<(String, String), AuthError> {
    let mut parts = line.trim_end().split(' ');
    let method = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AuthError::OAuth("empty loopback request line".into()))?
        .to_owned();
    let target = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AuthError::OAuth("malformed loopback request line (no target)".into()))?
        .to_owned();
    Ok((method, target))
}

async fn write_response(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    // Best-effort: the browser tab already has what it needs from the status/body; a write or
    // shutdown failure here isn't something the login flow can act on.
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Byte-length-revealing (not fully side-channel-free) but good-enough constant-time-per-byte
/// comparison for the OAuth `state` anti-CSRF nonce — defense in depth, not a cryptographic
/// secret comparison (docs/PLAN.md §14).
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Device-code flow session as returned by the provider's device authorization endpoint.
#[derive(Debug, Clone)]
pub struct DeviceCodeSession {
    pub device_code: SecretString,
    pub user_code: String,
    pub verification_uri: String,
    pub interval: Duration,
    pub expires_in: Duration,
}

#[derive(Debug, Deserialize)]
struct DeviceAuthorizationResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_in: u64,
    #[serde(default)]
    interval: Option<u64>,
}

/// Starts a device-code flow (RFC 8628 §3.1/3.2).
pub async fn start_device_code_flow(
    http: &reqwest::Client,
    device_authorization_endpoint: &str,
    client_id: &str,
    scope: &str,
) -> Result<DeviceCodeSession, AuthError> {
    let response = http
        .post(device_authorization_endpoint)
        .form(&[("client_id", client_id), ("scope", scope)])
        .send()
        .await
        .map_err(|e| AuthError::OAuth(format!("device authorization request failed: {e}")))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| AuthError::OAuth(format!("reading device authorization response: {e}")))?;
    if !status.is_success() {
        return Err(AuthError::OAuth(format!(
            "device authorization endpoint returned {status}: {}",
            redact_excerpt(&body)
        )));
    }
    let parsed: DeviceAuthorizationResponse = serde_json::from_str(&body)
        .map_err(|e| AuthError::OAuth(format!("parsing device authorization response: {e}")))?;
    Ok(DeviceCodeSession {
        device_code: SecretString::from(parsed.device_code),
        user_code: parsed.user_code,
        verification_uri: parsed
            .verification_uri_complete
            .unwrap_or(parsed.verification_uri),
        interval: Duration::from_secs(parsed.interval.unwrap_or(5)),
        expires_in: Duration::from_secs(parsed.expires_in),
    })
}

/// A token response as returned by an OAuth token endpoint (authorization-code exchange, device
/// code poll, or refresh).
#[derive(Debug, Clone)]
pub struct TokenResponse {
    pub access_token: SecretString,
    pub refresh_token: Option<SecretString>,
    pub expires_in: Option<Duration>,
    pub token_type: String,
    /// Full decoded JSON body, for provider-specific extra fields (e.g. `id_token`).
    pub raw: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct RawTokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    #[serde(default = "default_token_type")]
    token_type: String,
}

fn default_token_type() -> String {
    "Bearer".to_owned()
}

fn parse_token_response(body: &str) -> Result<TokenResponse, AuthError> {
    let raw: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| AuthError::OAuth(format!("parsing token response: {e}")))?;
    let typed: RawTokenResponse = serde_json::from_value(raw.clone())
        .map_err(|e| AuthError::OAuth(format!("parsing token response fields: {e}")))?;
    Ok(TokenResponse {
        access_token: SecretString::from(typed.access_token),
        refresh_token: typed.refresh_token.map(SecretString::from),
        expires_in: typed.expires_in.map(Duration::from_secs),
        token_type: typed.token_type,
        raw,
    })
}

fn parse_oauth_error(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get("error")?
        .as_str()
        .map(str::to_owned)
}

/// Polls a device-code token endpoint (RFC 8628 §3.4/3.5): loops on `authorization_pending`,
/// backs off on `slow_down`, and stops on `expired_token` / `access_denied` or success.
pub async fn poll_device_code_token(
    http: &reqwest::Client,
    token_endpoint: &str,
    session: &DeviceCodeSession,
    client_id: &str,
) -> Result<TokenResponse, AuthError> {
    let mut interval = session.interval.max(Duration::from_secs(1));
    let deadline = tokio::time::Instant::now() + session.expires_in;

    loop {
        tokio::time::sleep(interval).await;
        if tokio::time::Instant::now() >= deadline {
            return Err(AuthError::OAuth(
                "device code expired before authorization completed".into(),
            ));
        }

        let response = http
            .post(token_endpoint)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", session.device_code.expose_secret()),
                ("client_id", client_id),
            ])
            .send()
            .await
            .map_err(|e| AuthError::OAuth(format!("device token poll request failed: {e}")))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| AuthError::OAuth(format!("reading device token poll response: {e}")))?;

        if status.is_success() {
            return parse_token_response(&body);
        }

        match parse_oauth_error(&body).as_deref() {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                interval += Duration::from_secs(5);
                continue;
            }
            Some("expired_token") => return Err(AuthError::OAuth("device code expired".into())),
            Some("access_denied") => {
                return Err(AuthError::OAuth(
                    "user denied the device authorization request".into(),
                ));
            }
            _ => {
                return Err(AuthError::OAuth(format!(
                    "device token endpoint returned {status}: {}",
                    redact_excerpt(&body)
                )));
            }
        }
    }
}

/// Exchanges an authorization code (+ PKCE verifier) for a token (RFC 6749 §4.1.3 + RFC 7636
/// §4.5). Thin wrapper over [`exchange_code_for_token_with_secret`] with `client_secret: None` —
/// kept so existing callers (a public-client PKCE flow needs no secret) don't have to change.
pub async fn exchange_code_for_token(
    http: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    code: &SecretString,
    pkce_verifier: &SecretString,
) -> Result<TokenResponse, AuthError> {
    exchange_code_for_token_with_secret(
        http,
        token_endpoint,
        client_id,
        None,
        redirect_uri,
        code,
        pkce_verifier,
    )
    .await
}

/// Same as [`exchange_code_for_token`], but additionally sends `client_secret` when `Some` (RFC
/// 6749 §2.3.1) — some OAuth providers register their official "installed app" client as
/// confidential despite using PKCE (e.g. Google's Desktop-app client type), and reject the
/// request without it.
pub async fn exchange_code_for_token_with_secret(
    http: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    client_secret: Option<&SecretString>,
    redirect_uri: &str,
    code: &SecretString,
    pkce_verifier: &SecretString,
) -> Result<TokenResponse, AuthError> {
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", code.expose_secret()),
        ("redirect_uri", redirect_uri),
        ("client_id", client_id),
        ("code_verifier", pkce_verifier.expose_secret()),
    ];
    if let Some(secret) = client_secret {
        form.push(("client_secret", secret.expose_secret()));
    }
    let response = http
        .post(token_endpoint)
        .form(&form)
        .send()
        .await
        .map_err(|e| AuthError::OAuth(format!("token exchange request failed: {e}")))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| AuthError::OAuth(format!("reading token exchange response: {e}")))?;
    if !status.is_success() {
        return Err(AuthError::OAuth(format!(
            "token endpoint returned {status}: {}",
            redact_excerpt(&body)
        )));
    }
    parse_token_response(&body)
}

/// Refreshes an access token (RFC 6749 §6). Thin wrapper over
/// [`refresh_access_token_with_secret`] with `client_secret: None`. Callers go through
/// `RefreshCoordinator` for single-flight behavior, not this function directly.
pub async fn refresh_access_token(
    http: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    refresh_token: &SecretString,
) -> Result<TokenResponse, AuthError> {
    refresh_access_token_with_secret(http, token_endpoint, client_id, None, refresh_token).await
}

/// Same as [`refresh_access_token`], but additionally sends `client_secret` when `Some` — see
/// [`exchange_code_for_token_with_secret`] for why some providers require this even for a PKCE
/// client.
pub async fn refresh_access_token_with_secret(
    http: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    client_secret: Option<&SecretString>,
    refresh_token: &SecretString,
) -> Result<TokenResponse, AuthError> {
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token.expose_secret()),
        ("client_id", client_id),
    ];
    if let Some(secret) = client_secret {
        form.push(("client_secret", secret.expose_secret()));
    }
    let response = http
        .post(token_endpoint)
        .form(&form)
        .send()
        .await
        .map_err(|e| AuthError::OAuth(format!("refresh request failed: {e}")))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| AuthError::OAuth(format!("reading refresh response: {e}")))?;
    if !status.is_success() {
        return Err(AuthError::OAuth(format!(
            "refresh endpoint returned {status}: {}",
            redact_excerpt(&body)
        )));
    }
    parse_token_response(&body)
}

/// Redacts (`crate::redact::redact`) and truncates a response body for use in an error message —
/// never the raw body, never a bare token (docs/PLAN.md §14).
fn redact_excerpt(body: &str) -> String {
    const LIMIT: usize = 200;
    let redacted = crate::redact::redact(body);
    if redacted.chars().count() <= LIMIT {
        return redacted;
    }
    let truncated: String = redacted.chars().take(LIMIT).collect();
    format!("{truncated}… (truncated)")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;
    use secrecy::ExposeSecret;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[test]
    fn pkce_verifier_has_expected_length_and_charset() {
        let pkce = generate_pkce();
        let verifier = pkce.verifier.expose_secret();
        assert_eq!(verifier.len(), PKCE_VERIFIER_LEN);
        assert!(verifier.bytes().all(|b| PKCE_CHARSET.contains(&b)));
    }

    #[test]
    fn pkce_challenge_is_deterministic_function_of_verifier() {
        let challenge_a = challenge_from_verifier("fixed-test-verifier");
        let challenge_b = challenge_from_verifier("fixed-test-verifier");
        assert_eq!(challenge_a, challenge_b);
        // Base64url-no-pad output must never contain padding or URL-unsafe characters.
        assert!(!challenge_a.contains('='));
        assert!(!challenge_a.contains('+'));
        assert!(!challenge_a.contains('/'));
    }

    #[test]
    fn two_generated_pkce_pairs_differ() {
        let a = generate_pkce();
        let b = generate_pkce();
        assert_ne!(a.verifier.expose_secret(), b.verifier.expose_secret());
        assert_ne!(a.challenge, b.challenge);
    }

    #[test]
    fn authorization_url_contains_all_required_params() {
        let url = build_authorization_url(AuthorizationUrlParams {
            authorize_endpoint: "https://auth.example.com/authorize",
            client_id: "client-123",
            redirect_uri: "http://127.0.0.1:1455/callback",
            scope: "openid profile",
            state: "state-abc",
            code_challenge: "challenge-xyz",
            code_challenge_method: "S256",
            extra_params: &[("prompt", "consent")],
        })
        .unwrap();

        let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs.get("response_type").map(String::as_str), Some("code"));
        assert_eq!(
            pairs.get("client_id").map(String::as_str),
            Some("client-123")
        );
        assert_eq!(
            pairs.get("redirect_uri").map(String::as_str),
            Some("http://127.0.0.1:1455/callback")
        );
        assert_eq!(pairs.get("state").map(String::as_str), Some("state-abc"));
        assert_eq!(
            pairs.get("code_challenge").map(String::as_str),
            Some("challenge-xyz")
        );
        assert_eq!(
            pairs.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_eq!(pairs.get("prompt").map(String::as_str), Some("consent"));
    }

    #[test]
    fn invalid_authorize_endpoint_is_rejected() {
        let result = build_authorization_url(AuthorizationUrlParams {
            authorize_endpoint: "not a url",
            client_id: "c",
            redirect_uri: "http://127.0.0.1/cb",
            scope: "s",
            state: "st",
            code_challenge: "cc",
            code_challenge_method: "S256",
            extra_params: &[],
        });
        assert!(result.is_err());
    }

    #[test]
    fn constant_time_eq_matches_and_rejects() {
        assert!(constant_time_eq("same-state", "same-state"));
        assert!(!constant_time_eq("same-state", "different"));
        assert!(!constant_time_eq("short", "much-longer-value"));
    }

    #[test]
    fn redact_excerpt_hides_bearer_tokens_and_truncates() {
        let long_body = format!(
            "Authorization: Bearer XLC-SENTINEL-abcdefghijklmnop {}",
            "x".repeat(400)
        );
        let excerpt = redact_excerpt(&long_body);
        assert!(!excerpt.contains("SENTINEL"));
        assert!(excerpt.contains("(truncated)"));
    }

    // --- LoopbackServer ---------------------------------------------------------------------

    #[tokio::test]
    async fn loopback_server_returns_code_and_state() {
        let server = LoopbackServer::bind(None, "/callback", "127.0.0.1")
            .await
            .unwrap();
        let redirect_uri = server.redirect_uri().to_owned();

        let client_task = tokio::spawn(async move {
            let url = format!("{redirect_uri}?code=auth-code-123&state=expected-state");
            reqwest::get(url).await.unwrap()
        });

        let callback = server
            .wait_for_callback("expected-state", Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(callback.code.expose_secret(), "auth-code-123");
        assert_eq!(callback.state, "expected-state");

        let response = client_task.await.unwrap();
        assert!(response.status().is_success());
    }

    #[tokio::test]
    async fn loopback_server_rejects_state_mismatch() {
        let server = LoopbackServer::bind(None, "/callback", "127.0.0.1")
            .await
            .unwrap();
        let redirect_uri = server.redirect_uri().to_owned();

        tokio::spawn(async move {
            let url = format!("{redirect_uri}?code=auth-code-123&state=wrong-state");
            let _ = reqwest::get(url).await;
        });

        let err = server
            .wait_for_callback("expected-state", Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::OAuth(_)));
    }

    #[tokio::test]
    async fn loopback_server_surfaces_error_param() {
        let server = LoopbackServer::bind(None, "/callback", "127.0.0.1")
            .await
            .unwrap();
        let redirect_uri = server.redirect_uri().to_owned();

        tokio::spawn(async move {
            let url = format!("{redirect_uri}?error=access_denied&state=expected-state");
            let _ = reqwest::get(url).await;
        });

        let err = server
            .wait_for_callback("expected-state", Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::OAuth(_)));
    }

    #[tokio::test]
    async fn loopback_server_times_out_without_a_request() {
        let server = LoopbackServer::bind(None, "/callback", "127.0.0.1")
            .await
            .unwrap();
        let err = server
            .wait_for_callback("expected-state", Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::OAuth(_)));
    }

    // --- token endpoints (wiremock, no real network / loopback needed) --------------------------

    #[tokio::test]
    async fn exchange_code_for_token_parses_success_response() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "XLC-SENTINEL-ACCESS",
                "refresh_token": "XLC-SENTINEL-REFRESH",
                "expires_in": 3600,
                "token_type": "Bearer",
            })))
            .mount(&mock_server)
            .await;

        let http = reqwest::Client::new();
        let token = exchange_code_for_token(
            &http,
            &format!("{}/token", mock_server.uri()),
            "client-id",
            "http://127.0.0.1:1455/callback",
            &SecretString::from("auth-code".to_string()),
            &SecretString::from("verifier".to_string()),
        )
        .await
        .unwrap();

        assert_eq!(token.access_token.expose_secret(), "XLC-SENTINEL-ACCESS");
        assert_eq!(
            token.refresh_token.unwrap().expose_secret(),
            "XLC-SENTINEL-REFRESH"
        );
        assert_eq!(token.expires_in, Some(Duration::from_secs(3600)));
    }

    #[tokio::test]
    async fn exchange_code_for_token_surfaces_redacted_error_on_failure() {
        let mock_server = MockServer::start().await;
        // Deliberately not JSON: `redact()` matches on whitespace-separated words (PATTERNS.md
        // §14 defense-in-depth), so this exercises that path directly rather than relying on it
        // to reach into unescaped JSON string values.
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_string(
                "error=invalid_grant leaked= Bearer XLC-SENTINEL-LEAK-ME-abcdefgh1234 end",
            ))
            .mount(&mock_server)
            .await;

        let http = reqwest::Client::new();
        let err = exchange_code_for_token(
            &http,
            &format!("{}/token", mock_server.uri()),
            "client-id",
            "http://127.0.0.1:1455/callback",
            &SecretString::from("auth-code".to_string()),
            &SecretString::from("verifier".to_string()),
        )
        .await
        .unwrap_err();

        let message = err.to_string();
        assert!(!message.contains("XLC-SENTINEL-LEAK-ME"));
        assert!(message.contains("invalid_grant"));
    }

    #[tokio::test]
    async fn refresh_access_token_parses_success_response() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "XLC-SENTINEL-NEW-ACCESS",
                "expires_in": 1800,
            })))
            .mount(&mock_server)
            .await;

        let http = reqwest::Client::new();
        let token = refresh_access_token(
            &http,
            &format!("{}/token", mock_server.uri()),
            "client-id",
            &SecretString::from("old-refresh-token".to_string()),
        )
        .await
        .unwrap();
        assert_eq!(
            token.access_token.expose_secret(),
            "XLC-SENTINEL-NEW-ACCESS"
        );
        assert_eq!(token.token_type, "Bearer");
    }

    #[tokio::test]
    async fn exchange_code_for_token_with_secret_sends_client_secret_when_provided() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(wiremock::matchers::body_string_contains(
                "client_secret=XLC-SENTINEL-CLIENT-SECRET",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "XLC-SENTINEL-ACCESS",
                "expires_in": 3600,
            })))
            .mount(&mock_server)
            .await;

        let http = reqwest::Client::new();
        let token = exchange_code_for_token_with_secret(
            &http,
            &format!("{}/token", mock_server.uri()),
            "client-id",
            Some(&SecretString::from(
                "XLC-SENTINEL-CLIENT-SECRET".to_string(),
            )),
            "http://127.0.0.1:1455/callback",
            &SecretString::from("auth-code".to_string()),
            &SecretString::from("verifier".to_string()),
        )
        .await
        .unwrap();
        assert_eq!(token.access_token.expose_secret(), "XLC-SENTINEL-ACCESS");
    }

    #[tokio::test]
    async fn exchange_code_for_token_without_secret_never_sends_the_field() {
        let mock_server = MockServer::start().await;
        // No `.and(body_string_contains("client_secret"))` matcher needed: any request reaching
        // this mock at all (the plain `exchange_code_for_token` wrapper) must never include one.
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "XLC-SENTINEL-ACCESS",
                "expires_in": 3600,
            })))
            .mount(&mock_server)
            .await;

        let http = reqwest::Client::new();
        exchange_code_for_token(
            &http,
            &format!("{}/token", mock_server.uri()),
            "client-id",
            "http://127.0.0.1:1455/callback",
            &SecretString::from("auth-code".to_string()),
            &SecretString::from("verifier".to_string()),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn refresh_access_token_with_secret_sends_client_secret_when_provided() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(wiremock::matchers::body_string_contains(
                "client_secret=XLC-SENTINEL-CLIENT-SECRET",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "XLC-SENTINEL-NEW-ACCESS",
                "expires_in": 1800,
            })))
            .mount(&mock_server)
            .await;

        let http = reqwest::Client::new();
        let token = refresh_access_token_with_secret(
            &http,
            &format!("{}/token", mock_server.uri()),
            "client-id",
            Some(&SecretString::from(
                "XLC-SENTINEL-CLIENT-SECRET".to_string(),
            )),
            &SecretString::from("old-refresh-token".to_string()),
        )
        .await
        .unwrap();
        assert_eq!(
            token.access_token.expose_secret(),
            "XLC-SENTINEL-NEW-ACCESS"
        );
    }

    #[tokio::test]
    async fn start_device_code_flow_parses_response() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/device"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "XLC-SENTINEL-DEVICE-CODE",
                "user_code": "ABCD-EFGH",
                "verification_uri": "https://example.com/device",
                "expires_in": 600,
                "interval": 2,
            })))
            .mount(&mock_server)
            .await;

        let http = reqwest::Client::new();
        let session = start_device_code_flow(
            &http,
            &format!("{}/device", mock_server.uri()),
            "client-id",
            "openid",
        )
        .await
        .unwrap();
        assert_eq!(session.user_code, "ABCD-EFGH");
        assert_eq!(session.interval, Duration::from_secs(2));
        assert_eq!(
            session.device_code.expose_secret(),
            "XLC-SENTINEL-DEVICE-CODE"
        );
    }

    #[tokio::test]
    async fn poll_device_code_token_retries_through_authorization_pending() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "authorization_pending",
            })))
            .up_to_n_times(2)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "XLC-SENTINEL-DEVICE-ACCESS",
                "expires_in": 3600,
            })))
            .mount(&mock_server)
            .await;

        let http = reqwest::Client::new();
        let session = DeviceCodeSession {
            device_code: SecretString::from("dc".to_string()),
            user_code: "ABCD".into(),
            verification_uri: "https://example.com".into(),
            interval: Duration::from_millis(5),
            expires_in: Duration::from_secs(5),
        };
        let token = poll_device_code_token(
            &http,
            &format!("{}/token", mock_server.uri()),
            &session,
            "client-id",
        )
        .await
        .unwrap();
        assert_eq!(
            token.access_token.expose_secret(),
            "XLC-SENTINEL-DEVICE-ACCESS"
        );
    }

    #[tokio::test]
    async fn poll_device_code_token_surfaces_access_denied() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "access_denied",
            })))
            .mount(&mock_server)
            .await;

        let http = reqwest::Client::new();
        let session = DeviceCodeSession {
            device_code: SecretString::from("dc".to_string()),
            user_code: "ABCD".into(),
            verification_uri: "https://example.com".into(),
            interval: Duration::from_millis(5),
            expires_in: Duration::from_secs(5),
        };
        let err = poll_device_code_token(
            &http,
            &format!("{}/token", mock_server.uri()),
            &session,
            "client-id",
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AuthError::OAuth(_)));
    }
}
