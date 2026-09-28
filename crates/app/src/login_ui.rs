// SPDX-License-Identifier: GPL-3.0-only

//! `LoginUi` impl for a plain terminal (docs/PLAN.md §6, Wave 2 brief): prints the browser
//! URL/device code, best-effort opens a browser (the `open` crate — note INV-1's
//! `ProcessLauncher` doesn't exist until Phase 1, so this is a narrow, deliberate exception, not a
//! general spawn path), and reads an API key from stdin without echoing it (`rpassword`) or from
//! `XLIGHTCLI_API_KEY_<PROVIDER>` if set.

use async_trait::async_trait;
use secrecy::SecretString;
use xlightcli_auth::{AuthError, LoginUi};
use xlightcli_protocol::ProviderId;

#[derive(Debug, Default)]
pub struct TerminalLoginUi;

#[async_trait]
impl LoginUi for TerminalLoginUi {
    async fn show_browser_url(&self, url: &str) {
        crate::output::info(&format!("Open this URL to log in:\n  {url}"));
        if let Err(err) = open::that(url) {
            crate::output::warn(&format!(
                "couldn't open a browser automatically ({err}); open the URL above manually"
            ));
        }
    }

    async fn show_device_code(&self, verification_uri: &str, user_code: &str) {
        crate::output::info(&format!(
            "Go to {verification_uri} and enter code: {user_code}"
        ));
    }

    async fn prompt_api_key(&self, provider: &ProviderId) -> Result<SecretString, AuthError> {
        let env_name = env_var_name(provider);
        if let Ok(value) = std::env::var(&env_name)
            && !value.is_empty()
        {
            return Ok(SecretString::from(value));
        }
        let prompt = format!("Enter API key for {provider} (or set {env_name} and re-run): ");
        tokio::task::spawn_blocking(move || rpassword::prompt_password(prompt))
            .await
            .map_err(|join_err| AuthError::OAuth(format!("stdin read task panicked: {join_err}")))?
            .map(SecretString::from)
            .map_err(|io_err| {
                AuthError::OAuth(format!("failed to read API key from stdin: {io_err}"))
            })
    }
}

fn env_var_name(provider: &ProviderId) -> String {
    format!(
        "XLIGHTCLI_API_KEY_{}",
        provider.as_str().to_uppercase().replace(['-', '.'], "_")
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn env_var_name_is_upper_snake_case() {
        assert_eq!(
            env_var_name(&ProviderId::new("codex")),
            "XLIGHTCLI_API_KEY_CODEX"
        );
        assert_eq!(
            env_var_name(&ProviderId::new("claude-subscription")),
            "XLIGHTCLI_API_KEY_CLAUDE_SUBSCRIPTION"
        );
    }
}
