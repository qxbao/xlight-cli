// SPDX-License-Identifier: GPL-3.0-only

//! `LoginUi` — user-facing prompts needed by OAuth/API-key login flows.
//!
//! Implemented by `app`/`tui`; `auth` and `provider-*` only depend on the trait, so adapters stay
//! UI-agnostic and testable without a terminal.

use async_trait::async_trait;
use secrecy::SecretString;
use xlightcli_protocol::ProviderId;

use crate::error::AuthError;

#[async_trait]
pub trait LoginUi: Send + Sync {
    /// Shows a URL the user must open in a browser to complete the PKCE loopback flow.
    async fn show_browser_url(&self, url: &str);

    /// Shows a device code the user must enter at `verification_uri`.
    async fn show_device_code(&self, verification_uri: &str, user_code: &str);

    /// Prompts the user to paste an API key for `provider`. The returned value is wrapped
    /// immediately by the caller; nothing outside this call site should hold it as a plain
    /// `String` for longer than necessary to construct the `SecretString`.
    async fn prompt_api_key(&self, provider: &ProviderId) -> Result<SecretString, AuthError>;
}
