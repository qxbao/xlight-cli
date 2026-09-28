// SPDX-License-Identifier: GPL-3.0-only
// Portions derived from OpenCodex (MIT) @ 3cc34e1181926b64331490fdcfee162ffb62fe73:
// src/oauth/anthropic.ts, src/oauth/local-token-detect.ts, src/adapters/client-fingerprint.ts.
// See THIRD_PARTY.md.

//! Endpoint/header/client-id constants for both Claude transports (PATTERNS.md §5).
//!
//! Every constant is commented with a confidence label (H/M/U, docs/providers/claude.md
//! convention) and the date it was recorded. **H** = public, documented API; **M** = observed in
//! a working third-party implementation but not independently live-verified by us; **U**
//! = unconfirmed / community-sourced. Anything below H must be re-checked at the Phase 0/1 live
//! spike (`docs/providers/claude.md` checklist) before being trusted in production.

/// `anthropic-api` base URL. UNVERIFIED-by-us-live but **H**: public docs (docs.anthropic.com /
/// platform.claude.com), 2026-09-28.
pub(crate) const ANTHROPIC_API_BASE_URL: &str = "https://api.anthropic.com";

/// Messages API path. **H**, public docs, 2026-09-28.
pub(crate) const MESSAGES_PATH: &str = "/v1/messages";

/// Model listing path. **H**, public docs, 2026-09-28.
pub(crate) const MODELS_PATH: &str = "/v1/models";

/// `anthropic-version` pinned by this adapter (docs/PLAN.md §15 version gating). **H**, public
/// docs, 2026-09-28. Bump only after re-verifying the wire shape against the new version.
pub(crate) const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Header carrying the API key for `anthropic-api`. **H**, public docs, 2026-09-28.
pub(crate) const API_KEY_HEADER: &str = "x-api-key";

pub(crate) const ANTHROPIC_VERSION_HEADER: &str = "anthropic-version";
#[cfg(feature = "claude-subscription")]
pub(crate) const ANTHROPIC_BETA_HEADER: &str = "anthropic-beta";

/// Default `max_tokens` when `TurnRequest::max_output_tokens` is unset. **M** — chosen to match
/// the common default used by third-party Anthropic proxies (OpenCodex
/// `adapters/anthropic.ts::DEFAULT_MAX_TOKENS`); Anthropic itself requires an explicit value.
pub(crate) const DEFAULT_MAX_TOKENS: u32 = 8192;

/// Minimum `thinking.budget_tokens` accepted by the Messages API. **H**, public docs, 2026-09-28.
pub(crate) const MIN_THINKING_BUDGET_TOKENS: u32 = 1024;

/// Extra room reserved above the thinking budget so `max_tokens > budget_tokens` always holds
/// (Anthropic 400s otherwise). **M**, same rationale as OpenCodex `OUTPUT_HEADROOM`.
pub(crate) const THINKING_OUTPUT_HEADROOM: u32 = 4096;

// ---------------------------------------------------------------------------------------------
// `claude-subscription` (experimental, D-002) — OAuth ("Claude Pro/Max") wire details.
//
// Source: OpenCodex (MIT) @ 3cc34e1181926b64331490fdcfee162ffb62fe73, `src/oauth/anthropic.ts` +
// `src/adapters/anthropic.ts` (docs/PLAN.md §4.5 port map). OpenCodex is a working third-party
// proxy, not an Anthropic-published spec, so everything below is **M** (observed-working,
// community-sourced) unless noted otherwise — re-verify at the live OAuth spike
// (docs/providers/claude.md checklist) before relying on it in a release build.
// ---------------------------------------------------------------------------------------------

/// OAuth authorization endpoint (`claude.ai`, not `api.anthropic.com`). **M**, 2026-09-28.
/// Unconditionally compiled: `ClaudeEndpoints::default()` (always public, regardless of the
/// `claude-subscription` feature — the endpoints struct's shape must not depend on features)
/// needs a default value here even in builds where the subscription transport itself is absent.
pub(crate) const OAUTH_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";

/// OAuth token endpoint (authorization-code exchange + refresh). **M**, 2026-09-28.
pub(crate) const OAUTH_TOKEN_URL: &str = "https://api.anthropic.com/v1/oauth/token";

/// Public OAuth client id used by Claude Code itself (base64 `OWQxYzI1MGEtZTYxYi00NGQ5LTg4ZWQt
/// NTk0NGQxOTYyZjVl`, decoded once here so the constant is grep-able). **M**, sourced from
/// OpenCodex; not a secret (public native-app client ids are not confidential per OAuth 2.0 for
/// native apps), but still only used for the experimental transport.
pub(crate) const OAUTH_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// OAuth scopes requested by Claude Code. **M**, 2026-09-28. Only referenced by the
/// `claude-subscription` login flow.
#[cfg(feature = "claude-subscription")]
pub(crate) const OAUTH_SCOPES: &str = "org:create_api_key user:profile user:inference";

/// Loopback callback port Claude Code's own OAuth client registers. **M**, 2026-09-28 — a fixed
/// port (not ephemeral) because the redirect URI must match what's registered upstream.
pub(crate) const OAUTH_CALLBACK_PORT: u16 = 54545;

pub(crate) const OAUTH_CALLBACK_PATH: &str = "/callback";

/// `anthropic-beta` value required on every OAuth-authenticated Messages call — without it the
/// API rejects the OAuth bearer token. **M**, 2026-09-28.
#[cfg(feature = "claude-subscription")]
pub(crate) const OAUTH_BETA_HEADER_VALUE: &str = "claude-code-20250219,oauth-2025-04-20";

/// First `system` block required on every OAuth-authenticated request (Anthropic's OAuth tokens
/// are scoped to first-party Claude Code use and reject requests missing this framing). **M**,
/// verbatim from OpenCodex `CLAUDE_CODE_SYSTEM_INSTRUCTION`, 2026-09-28. Referenced
/// unconditionally by `wire::request` (whose `oauth_mode` flag is a runtime bool, not a cargo
/// feature) even though only `transport_subscription` ever sets that flag to `true`.
pub(crate) const OAUTH_SYSTEM_INSTRUCTION: &str =
    "You are a Claude agent, built on Anthropic's Claude Agent SDK.";

/// Tool names sent over an OAuth-authenticated request must not collide with Anthropic's own
/// built-in tool names and (per OpenCodex) are rejected if "unprefixed" custom names are used;
/// this prefix is added to every non-builtin tool name and stripped from `tool_use.name` on the
/// way back. **M**, 2026-09-28.
pub(crate) const OAUTH_TOOL_NAME_PREFIX: &str = "custom_";

/// Anthropic builtin tool names exempt from `OAUTH_TOOL_NAME_PREFIX`. **M**, 2026-09-28.
pub(crate) const OAUTH_BUILTIN_TOOL_NAMES: &[&str] =
    &["web_search", "code_execution", "text_editor", "computer"];

/// Claude Code credential file, relative to the config dir (`CLAUDE_CONFIG_DIR` env override,
/// else `~/.claude`) (docs/import.md, docs/providers/claude.md). **H** on Linux (0600 file);
/// macOS additionally checks Keychain service `"Claude Code-credentials"` first — **not**
/// implemented here (no keyring dependency in this crate; Phase 2 `auth::store` territory), so
/// discovery on macOS only finds this file as a fallback, same as Claude Code itself when the
/// Keychain is unavailable. `auth::discover_existing`/`import` compile and run regardless of the
/// `claude-subscription` feature (finding/importing a credential doesn't require the transport
/// that would use it), so these stay unconditional too.
pub(crate) const CREDENTIALS_FILENAME: &str = ".credentials.json";

pub(crate) const CLAUDE_CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

pub(crate) const CLAUDE_CONFIG_DIR_DEFAULT_RELATIVE: &str = ".claude";

// ---------------------------------------------------------------------------------------------
// Rate limit / quota headers.
// ---------------------------------------------------------------------------------------------

/// Standard per-request rate-limit headers, `anthropic-api`. **M** (docs.anthropic.com describes
/// the `anthropic-ratelimit-*` family; the exact bucket name below — "requests" — is the one most
/// consistently documented across examples as of 2026-09-28).
pub(crate) const RATELIMIT_REQUESTS_LIMIT_HEADER: &str = "anthropic-ratelimit-requests-limit";
pub(crate) const RATELIMIT_REQUESTS_REMAINING_HEADER: &str =
    "anthropic-ratelimit-requests-remaining";
pub(crate) const RATELIMIT_REQUESTS_RESET_HEADER: &str = "anthropic-ratelimit-requests-reset";

/// Client-fingerprint headers the real Claude Code CLI sends on every OAuth-authenticated
/// request. **M**, sourced from OpenCodex `adapters/client-fingerprint.ts` (`CLAUDE_CODE_HEADERS`,
/// pinned to "Claude Code 2.1.63 / @anthropic-ai/sdk 0.74.0" as of that commit), 2026-09-28.
///
/// Sending an OAuth bearer token with an otherwise-empty header set is itself a
/// non-first-party signature that upstream can flag — per OpenCodex's own comment on that
/// constant. Reproducing this fingerprint on the `claude-subscription` transport is exactly the
/// kind of "identity spoofing" INV-9 permits **only** for an experimental transport the user has
/// explicitly opted into (D-002 ToS acknowledgement) — never anywhere else in this codebase.
/// `X-Stainless-Arch`/`-OS` and the session/request ids are computed at request time
/// (`transport_subscription.rs`), not hardcoded here, since they depend on the running process.
// Header *names* are sent lowercase (HTTP header names are case-insensitive on the wire, and
// `http::HeaderMap::insert` accepts a `&'static str` key directly only when it's already
// lowercase — see `transport_subscription::common_headers`).
#[cfg(feature = "claude-subscription")]
pub(crate) const CLAUDE_CODE_STATIC_HEADERS: &[(&str, &str)] = &[
    ("x-app", "cli"),
    ("x-stainless-retry-count", "0"),
    ("x-stainless-runtime", "node"),
    ("x-stainless-lang", "js"),
    ("x-stainless-timeout", "600"),
    ("x-stainless-package-version", "0.74.0"),
];

#[cfg(feature = "claude-subscription")]
pub(crate) const CLAUDE_CODE_SESSION_ID_HEADER: &str = "x-claude-code-session-id";
#[cfg(feature = "claude-subscription")]
pub(crate) const CLIENT_REQUEST_ID_HEADER: &str = "x-client-request-id";

/// Subscription "unified" quota headers (5h / 7d rolling windows), `claude-subscription` only.
/// **U** — community-sourced (docs/providers/claude.md already labels this **U**); needs a live
/// spike against a real subscription account to confirm exact header names/semantics.
#[cfg(feature = "claude-subscription")]
pub(crate) const RATELIMIT_UNIFIED_5H_UTILIZATION_HEADER: &str =
    "anthropic-ratelimit-unified-5h-utilization";
#[cfg(feature = "claude-subscription")]
pub(crate) const RATELIMIT_UNIFIED_5H_RESET_HEADER: &str = "anthropic-ratelimit-unified-5h-reset";
#[cfg(feature = "claude-subscription")]
pub(crate) const RATELIMIT_UNIFIED_7D_UTILIZATION_HEADER: &str =
    "anthropic-ratelimit-unified-7d-utilization";
#[cfg(feature = "claude-subscription")]
pub(crate) const RATELIMIT_UNIFIED_7D_RESET_HEADER: &str = "anthropic-ratelimit-unified-7d-reset";
