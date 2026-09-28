// SPDX-License-Identifier: GPL-3.0-only

//! `CredentialSet` / `CredentialSecret` / `AccountInfo` (docs/PLAN.md §6, PATTERNS.md §4).
//!
//! Secrets are wrapped in `secrecy::SecretString`, which already zeroizes on drop and redacts
//! itself in `Debug` output — deriving `Debug` on the types below is safe by construction, no
//! manual redaction needed. Neither type derives `Serialize`: the only place allowed to turn a
//! secret into bytes for storage is `crate::store` (PATTERNS.md §4).

use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use xlightcli_protocol::{AuthKind, ProviderId, TransportId};

/// Non-secret account metadata: safe to log, persist in SQLite (`accounts` table,
/// docs/PLAN.md §11.2), and hand to callers via `CredentialHandle::account()`. `Serialize`/
/// `Deserialize` are safe here (unlike on `CredentialSecret`/`CredentialSet`) because nothing in
/// this struct is a secret; `store::StoredCredential` embeds it as-is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountInfo {
    pub provider: ProviderId,
    pub transport: TransportId,
    /// Upstream account identifier (opaque string — email, user id, ...).
    pub account_id: String,
    pub label: Option<String>,
    pub auth_kind: AuthKind,
    /// Non-secret, provider-specific metadata (e.g. agy's Cloud Code Assist project id,
    /// discovered during OAuth login) — never a secret (enforced by convention, not by type;
    /// adapters must not put tokens here, same rule as `xlightcli_storage::AccountRecord::metadata`
    /// which this is persisted into verbatim, docs/PLAN.md §11.2). `#[serde(default)]` so
    /// payloads persisted before this field existed still deserialize.
    ///
    /// Not `Eq` (unlike the rest of this struct pre-this-field) because `serde_json::Value`
    /// doesn't derive it — matches `AccountRecord`'s own choice for the same reason.
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// How a credential's secret is injected into an outgoing request. Different transports need
/// different header shapes (OpenAI/Codex: `Authorization: Bearer`; Anthropic: `x-api-key`), so
/// this is a small enum rather than a single hardcoded scheme.
#[derive(Debug, Clone)]
pub enum CredentialSecret {
    /// `Authorization: Bearer <token>`.
    Bearer {
        access_token: SecretString,
        refresh_token: Option<SecretString>,
        #[allow(dead_code)] // read by refresh/expiry logic once Wave 2 implements it
        expires_at: Option<OffsetDateTime>,
    },
    /// Arbitrary header-based API key, e.g. Anthropic's `x-api-key`.
    Header {
        header_name: String,
        value: SecretString,
    },
}

/// A full credential: metadata plus the live secret. Never leaves crate `auth` in this shape —
/// callers outside the crate only ever see a `CredentialHandle` (INV-4).
#[derive(Debug, Clone)]
pub struct CredentialSet {
    pub account: AccountInfo,
    pub secret: CredentialSecret,
}
