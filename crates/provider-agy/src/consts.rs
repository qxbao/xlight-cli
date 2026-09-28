// SPDX-License-Identifier: GPL-3.0-only
// Portions derived from OpenCodex (MIT) @ 3cc34e1181926b64331490fdcfee162ffb62fe73:
// src/oauth/google-antigravity.ts, src/adapters/client-fingerprint.ts
// See THIRD_PARTY.md.

//! Endpoint / header / OAuth-client constants for the `agy` provider (PATTERNS.md §5).
//!
//! Every constant below is labeled per `docs/providers/agy.md`'s confidence scale (H/M/U) with the
//! date it was recorded. **Nothing here has been verified against a live account** (docs/PLAN.md
//! §6 live-test checklist, `docs/providers/agy.md`). The `antigravity` section is additionally
//! ported from OpenCodex (MIT) reference source, cited per-item; see `THIRD_PARTY.md`.

/// `gemini-api` (stable, Google-documented public API — H, ai.google.dev, 2026-09-28).
pub(crate) mod gemini_api {
    /// Default Generative Language API host. Overridable via [`GEMINI_BASE_URL_ENV`].
    pub(crate) const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com";
    pub(crate) const API_VERSION: &str = "v1beta";
    /// Env var holding the API key (docs/import.md: agy credential row).
    pub(crate) const API_KEY_ENV: &str = "GEMINI_API_KEY";
    /// Env var overriding [`DEFAULT_BASE_URL`] (docs/providers/agy.md transport table).
    pub(crate) const BASE_URL_ENV: &str = "GOOGLE_GEMINI_BASE_URL";
    /// Header the API key travels in (`x-goog-api-key`), matching
    /// `xlightcli_auth::CredentialSecret::Header`.
    pub(crate) const API_KEY_HEADER: &str = "x-goog-api-key";
}

/// `antigravity` (**experimental**, Cloud Code Assist, D-002). **UNVERIFIED as of 2026-09-28** —
/// every value here is ported from OpenCodex (MIT) `@ 3cc34e1181926b64331490fdcfee162ffb62fe73`
/// (docs/PLAN.md §4.5), not independently confirmed against a live Google account. Re-check with a
/// live spike before shipping (docs/providers/agy.md checklist).
pub(crate) mod antigravity {
    /// Env var holding the OAuth client id of the Antigravity desktop client.
    ///
    /// The id/secret pair is deliberately **not** shipped in this repo: it belongs to Google's
    /// Antigravity client, not to xlightcli, and GitHub push protection flags it as a leaked
    /// Google OAuth credential. Users who opt into this experimental transport supply it
    /// themselves; see `docs/providers/agy.md` ("OAuth client") for where the public values come
    /// from.
    pub(crate) const OAUTH_CLIENT_ID_ENV: &str = "XLIGHTCLI_ANTIGRAVITY_OAUTH_CLIENT_ID";
    /// Env var holding the matching OAuth client secret (a "Desktop app" client secret: public,
    /// not a user secret, but still never committed; see `OAUTH_CLIENT_ID_ENV`).
    pub(crate) const OAUTH_CLIENT_SECRET_ENV: &str = "XLIGHTCLI_ANTIGRAVITY_OAUTH_CLIENT_SECRET";

    pub(crate) const GOOGLE_AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
    pub(crate) const GOOGLE_TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
    pub(crate) const GOOGLE_REVOKE_ENDPOINT: &str = "https://oauth2.googleapis.com/revoke";
    pub(crate) const GOOGLE_USERINFO_ENDPOINT: &str =
        "https://www.googleapis.com/oauth2/v2/userinfo";

    /// Cloud Code Assist production API (`streamGenerateContent`, `loadCodeAssist`).
    pub(crate) const CCA_PROD_BASE_URL: &str = "https://cloudcode-pa.googleapis.com";
    /// Cloud Code Assist "daily" API (quota, onboarding) — a distinct host from the prod API in
    /// the ported source; kept separate rather than assumed identical.
    pub(crate) const CCA_DAILY_BASE_URL: &str = "https://daily-cloudcode-pa.googleapis.com";
    pub(crate) const CCA_API_VERSION: &str = "v1internal";

    pub(crate) const SCOPES: &[&str] = &[
        "https://www.googleapis.com/auth/cloud-platform",
        "https://www.googleapis.com/auth/userinfo.email",
        "https://www.googleapis.com/auth/userinfo.profile",
        "https://www.googleapis.com/auth/cclog",
        "https://www.googleapis.com/auth/experimentsandconfigs",
    ];

    /// Loopback redirect port the official client registers with Google (fixed, not ephemeral —
    /// Google OAuth clients must pre-register their redirect URI).
    pub(crate) const CALLBACK_PORT: u16 = 51121;
    pub(crate) const CALLBACK_PATH: &str = "/callback";
    pub(crate) const CALLBACK_HOST: &str = "127.0.0.1";

    /// Pinned fallback Antigravity IDE language-server version, matching the bundled LS this
    /// User-Agent was decompiled from. Source: `src/adapters/client-fingerprint.ts`.
    pub(crate) const IDE_VERSION: &str = "2.5.5";
    const IDE_CLIENT_NAME: &str = "aidev_client";
    const IDE_OS_TYPE: &str = "windows";
    const IDE_ARCH: &str = "amd64";
    /// Env var override for the request `User-Agent`, mirroring OpenCodex's
    /// `GOOGLE_ANTIGRAVITY_USER_AGENT`.
    pub(crate) const USER_AGENT_ENV: &str = "GOOGLE_ANTIGRAVITY_USER_AGENT";

    /// Builds the real Antigravity IDE `User-Agent` header value
    /// (`antigravity/ide/{ver} (os_type=…; arch=…; aidev_client; auth_method=oauth)`),
    /// decompiled from the IDE's Go language server per the ported source's comment. **This is a
    /// deliberate client-impersonation fingerprint** (R-10): the Cloud Code Assist backend 404s
    /// CLI-shaped User-Agents for newer agent models, so only this exact family unlocks them.
    /// That is exactly why this transport is experimental and off by default (D-002) — see the
    /// module doc on `transport_antigravity`.
    pub(crate) fn request_user_agent() -> String {
        let env_override = std::env::var(USER_AGENT_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty());
        request_user_agent_from(env_override.as_deref())
    }

    /// Pure core of [`request_user_agent`], split out so tests can exercise the override branch
    /// without mutating the process environment (the workspace forbids `unsafe_code`, and
    /// `std::env::set_var`/`remove_var` require `unsafe` since Rust 2024 — PATTERNS.md §1).
    pub(super) fn request_user_agent_from(env_override: Option<&str>) -> String {
        if let Some(v) = env_override {
            return v.to_string();
        }
        format!(
            "antigravity/ide/{IDE_VERSION} (os_type={IDE_OS_TYPE}; arch={IDE_ARCH}; {IDE_CLIENT_NAME}; auth_method=oauth)"
        )
    }

    /// Literal constant sent in the CCA envelope's `userAgent` **body field** — distinct from the
    /// HTTP `User-Agent` header above (`request_user_agent()`). CLIProxyAPI (and this ported
    /// source) hardcode this value; only the HTTP header carries the versioned client string.
    /// Only used by `transport_antigravity` (feature `antigravity-subscription`).
    #[cfg(feature = "antigravity-subscription")]
    pub(crate) const ENVELOPE_USER_AGENT_FIELD: &str = "antigravity";
    #[cfg(feature = "antigravity-subscription")]
    pub(crate) const ENVELOPE_REQUEST_TYPE: &str = "agent";

    /// Explicit-override env var for the Cloud Code Assist project id (`AgyEndpoints::
    /// antigravity_project_id`'s `Default`). The primary channel is now
    /// `xlightcli_auth::AccountInfo::metadata[PROJECT_ID_METADATA_KEY]`, persisted there by
    /// `AgyAuthAdapter::login_browser_oauth` after `loadCodeAssist`/`onboardUser` discovery — this
    /// env var only matters when a caller wants to force a different project than the one on the
    /// credential (or when working with a `CredentialHandle::for_tests` handle that has none). See
    /// `docs/providers/agy.md`.
    pub(crate) const PROJECT_ID_ENV: &str = "GOOGLE_ANTIGRAVITY_PROJECT_ID";

    /// Key under `AccountInfo.metadata` (a JSON object) holding the discovered Cloud Code Assist
    /// project id. Shared between `auth.rs` (writer, at login/refresh) and
    /// `transport_antigravity.rs` (reader, at call time).
    pub(crate) const PROJECT_ID_METADATA_KEY: &str = "antigravity_project_id";
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::antigravity;

    // Exercises the pure `request_user_agent_from` core directly rather than mutating the
    // process-wide env var: the workspace forbids `unsafe_code`, and `std::env::set_var` requires
    // `unsafe` since Rust 2024, so a real env-var-mutation test isn't an option here.
    #[test]
    fn user_agent_defaults_when_no_override() {
        assert_eq!(
            antigravity::request_user_agent_from(None),
            "antigravity/ide/2.5.5 (os_type=windows; arch=amd64; aidev_client; auth_method=oauth)"
        );
    }

    #[test]
    fn user_agent_env_override_wins() {
        assert_eq!(
            antigravity::request_user_agent_from(Some("custom-ua/1.0")),
            "custom-ua/1.0"
        );
    }
}
