// SPDX-License-Identifier: GPL-3.0-only

//! `AuthAdapter` — provider-specific auth flows (docs/PLAN.md §6.1).
//!
//! Implemented once per provider in `provider-<x>::auth`. `AuthBroker` is the sole owner of
//! storage, refresh scheduling and locking around whatever an adapter returns here.

use std::path::PathBuf;

use async_trait::async_trait;
use xlightcli_protocol::{ProviderId, TransportId};

use crate::credential::CredentialSet;
use crate::error::AuthError;
use crate::login_ui::LoginUi;

/// How a user can obtain a credential for a provider/transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthMethod {
    /// Reuse a credential found by `discover_existing` (import & own, D-017).
    ReuseExisting,
    /// Browser PKCE + loopback redirect.
    BrowserOAuth,
    /// OAuth device-code flow (fallback when no local browser/loopback is available).
    DeviceCode,
    /// Paste a plain API key.
    ApiKey,
}

/// A credential belonging to another CLI's config/keychain, found by `discover_existing`.
/// Carries only metadata — no secret bytes. `AuthAdapter::import` reads the actual file/keychain
/// entry and returns a full `CredentialSet`.
#[derive(Debug, Clone)]
pub struct DiscoveredCredential {
    pub provider: ProviderId,
    pub transport: TransportId,
    pub account_label: String,
    /// Where it was found (e.g. `~/.codex/auth.json`), shown to the user before import.
    pub source: PathBuf,
}

#[async_trait]
pub trait AuthAdapter: Send + Sync {
    /// Auth methods this provider supports, in the order they should be offered (docs/PLAN.md
    /// §6.3: reuse existing first, then browser OAuth, falling back to device code / API key).
    fn methods(&self) -> &[AuthMethod];

    /// Read-only scan of the official CLI's known credential/config locations (D-017). Must
    /// never write anything and must never spawn the official CLI (INV-1).
    async fn discover_existing(&self) -> Vec<DiscoveredCredential>;

    /// Imports and **owns** a discovered credential: copies it into xlightcli's secret store.
    /// xlightcli refreshes it independently from then on (D-017) and never writes back to the
    /// original file.
    async fn import(&self, found: &DiscoveredCredential) -> Result<CredentialSet, AuthError>;

    async fn login(&self, method: AuthMethod, ui: &dyn LoginUi)
    -> Result<CredentialSet, AuthError>;

    async fn refresh(&self, current: &CredentialSet) -> Result<CredentialSet, AuthError>;

    async fn revoke(&self, current: &CredentialSet) -> Result<(), AuthError>;
}
