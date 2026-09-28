// SPDX-License-Identifier: GPL-3.0-only
// Portions derived from OpenCodex (MIT) @ 3cc34e1181926b64331490fdcfee162ffb62fe73:
// src/oauth/chatgpt.ts, src/oauth/chatgpt-device.ts. See THIRD_PARTY.md.

//! Endpoint / header / OAuth-client constants for the Codex provider (PATTERNS.md §5).
//!
//! Every constant below is commented with a confidence label matching `docs/providers/codex.md`
//! (**H** = read directly at the cited commit, **M** = inferred from adjacent H-confidence code,
//! **U** = not independently confirmed — needs a live spike). Update both places together.
//!
//! Sources read 2026-09-28:
//! - `openai/codex` (Apache-2.0) @ `1cc7e2361237ce7244430ee1d581c77f95c57ac8`,
//!   `codex-rs/core/src/client.rs` (confirms the `/responses` path, the request body shape —
//!   `store: false`, `stream: true`, `include: ["reasoning.encrypted_content"]`, `tool_choice:
//!   "auto"` — and the `x-codex-*` header constants).
//! - OpenCodex (MIT) @ `3cc34e1181926b64331490fdcfee162ffb62fe73`, `src/oauth/chatgpt.ts` +
//!   `src/oauth/chatgpt-device.ts` (ChatGPT OAuth client id/endpoints, device-code grant).

// Several headers below (secondary window, `OpenAI-Beta`, installation id) are recorded for the
// live verification checklist (`docs/providers/codex.md`) but not wired into a request/response
// yet — sending an unverified header *value* risks breaking real requests, and the primary-window
// parsing already covers the `AgentEvent::RateLimit` requirement (Wave 2 brief item 4).
#![allow(dead_code)]

use std::time::Duration;

// --- ChatGPT OAuth (browser PKCE + loopback) ---
// H: OpenCodex `src/oauth/chatgpt.ts` — this is the same public PKCE client Codex CLI registers.
pub(crate) const CHATGPT_OAUTH_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
pub(crate) const CHATGPT_OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub(crate) const CHATGPT_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub(crate) const CHATGPT_OAUTH_SCOPE: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";
pub(crate) const CHATGPT_OAUTH_CALLBACK_PORT: u16 = 1455;
pub(crate) const CHATGPT_OAUTH_CALLBACK_PATH: &str = "/auth/callback";
pub(crate) const CHATGPT_OAUTH_REDIRECT_HOST: &str = "localhost";

// --- ChatGPT device-code grant ---
// H: OpenCodex `src/oauth/chatgpt-device.ts`. Not RFC 8628: the poll response returns an
// authorization code plus a SERVER-generated PKCE verifier, spent at the ordinary token endpoint
// with `grant_type=authorization_code` (never `device_code`/`urn:ietf:params:oauth:grant-type:
// device_code`).
pub(crate) const CHATGPT_DEVICE_USERCODE_URL: &str =
    "https://auth.openai.com/api/accounts/deviceauth/usercode";
pub(crate) const CHATGPT_DEVICE_TOKEN_URL: &str =
    "https://auth.openai.com/api/accounts/deviceauth/token";
pub(crate) const CHATGPT_DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
pub(crate) const CHATGPT_DEVICE_VERIFICATION_URL: &str = "https://auth.openai.com/codex/device";
pub(crate) const CHATGPT_DEVICE_FLOW_TTL: Duration = Duration::from_secs(15 * 60);
pub(crate) const CHATGPT_DEVICE_DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(5);
pub(crate) const CHATGPT_DEVICE_MIN_POLL_INTERVAL: Duration = Duration::from_secs(1);

// --- ChatGPT backend (`chatgpt` transport, subscription) ---
// H (verified 2026-09-28): openai/codex @ 44fe510 `codex-rs/model-provider-info/src/lib.rs`
// `CHATGPT_CODEX_BASE_URL = "https://chatgpt.com/backend-api/codex"` + `/responses`
// (`codex-rs/core/src/client.rs`). Live-confirmed: `/backend-api/responses` returns 404.
pub(crate) const RESPONSES_PATH: &str = "/codex/responses";
// M: base host per docs/providers/codex.md (team Phase 0 spike, not re-derived here); kept
// overridable via `CodexEndpoints` so a live check can correct it without a code change.
pub(crate) const CHATGPT_BACKEND_BASE: &str = "https://chatgpt.com/backend-api";
// U: docs/providers/codex.md lists both `/wham/usage` and `/api/codex/usage` as candidates; not
// independently re-verified in this pass.
pub(crate) const CHATGPT_USAGE_PATH: &str = "/wham/usage";

// --- OpenAI API (`openai-api` transport, api-key) ---
// H: public, stable API surface (platform.openai.com/docs/api-reference/responses).
pub(crate) const OPENAI_API_BASE: &str = "https://api.openai.com/v1";

// --- Headers ---
// H: `codex-rs/core/src/client.rs` header name constants.
pub(crate) const HEADER_OPENAI_BETA: &str = "OpenAI-Beta";
pub(crate) const HEADER_X_CODEX_INSTALLATION_ID: &str = "x-codex-installation-id";
// U: header name/shape for the `originator` client-identification value; codex-rs applies it via
// `codex_login::default_client::add_originator_header`, whose exact header name we could not
// re-derive from `client.rs` alone in this pass.
pub(crate) const HEADER_ORIGINATOR: &str = "originator";
// H: forwarded by OpenCodex `src/adapters/openai-responses/passthrough.ts` for ChatGPT-auth
// requests; value = the ChatGPT account id (non-secret, from `AccountInfo::account_id`).
pub(crate) const HEADER_CHATGPT_ACCOUNT_ID: &str = "chatgpt-account-id";
pub(crate) const ORIGINATOR: &str = "xlightcli";

// U: quota header names per docs/providers/codex.md (team spike, not independently re-verified).
pub(crate) const HEADER_X_CODEX_PRIMARY_USED_PERCENT: &str = "x-codex-primary-used-percent";
pub(crate) const HEADER_X_CODEX_PRIMARY_WINDOW_MINUTES: &str = "x-codex-primary-window-minutes";
pub(crate) const HEADER_X_CODEX_PRIMARY_RESET_AT: &str = "x-codex-primary-reset-at";
pub(crate) const HEADER_X_CODEX_SECONDARY_USED_PERCENT: &str = "x-codex-secondary-used-percent";
pub(crate) const HEADER_X_CODEX_SECONDARY_WINDOW_MINUTES: &str = "x-codex-secondary-window-minutes";
pub(crate) const HEADER_X_CODEX_SECONDARY_RESET_AT: &str = "x-codex-secondary-reset-at";
pub(crate) const HEADER_X_CODEX_RATE_LIMIT_REACHED_TYPE: &str = "x-codex-rate-limit-reached-type";
