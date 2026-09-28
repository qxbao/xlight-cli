// SPDX-License-Identifier: GPL-3.0-only

//! Env-var layer (docs/PLAN.md §12.2: `env XLIGHTCLI_*`).
//!
//! **Scope decision (documented, Phase 1):** this recognizes a fixed, small set of `XLIGHTCLI_*`
//! variables rather than a generic "flatten any `XLIGHTCLI_SECTION_FIELD`" mechanism. A generic
//! mapper would need a type-directed deserializer (numbers vs bools vs lists all need different
//! parsing) for comparatively little payoff at this phase; extend the `match` in
//! [`partial_from_env`] as new variables are needed. Unrecognized `XLIGHTCLI_*` variables are
//! ignored, not an error (so unrelated env vars sharing the prefix, e.g. from a shell profile,
//! never break config loading).

use xlightcli_protocol::ProviderId;

use crate::partial::{PartialConfig, PartialExperimentalFlags, PartialToolsConfig};
use crate::schema::AuthStoreKind;

fn is_truthy(v: &str) -> bool {
    matches!(v, "1" | "true" | "TRUE" | "yes")
}

/// Builds a [`PartialConfig`] from an iterator of `(key, value)` env vars (production callers
/// pass `std::env::vars()`; tests pass a fixed `Vec`).
pub fn partial_from_env(vars: impl Iterator<Item = (String, String)>) -> PartialConfig {
    let mut partial = PartialConfig::empty();
    for (key, value) in vars {
        match key.as_str() {
            "XLIGHTCLI_DEFAULT_PROVIDER" => {
                partial.default_provider = Some(ProviderId::new(value));
            }
            "XLIGHTCLI_EXPERIMENTAL_CLAUDE_SUBSCRIPTION" => {
                partial
                    .experimental
                    .get_or_insert_with(PartialExperimentalFlags::default)
                    .claude_subscription = Some(is_truthy(&value));
            }
            "XLIGHTCLI_EXPERIMENTAL_ANTIGRAVITY_SUBSCRIPTION" => {
                partial
                    .experimental
                    .get_or_insert_with(PartialExperimentalFlags::default)
                    .antigravity_subscription = Some(is_truthy(&value));
            }
            "XLIGHTCLI_PERMISSIONS_MODE" => {
                if let Ok(mode) = value.parse() {
                    partial
                        .permissions
                        .get_or_insert_with(Default::default)
                        .mode = Some(mode);
                }
            }
            "XLIGHTCLI_AUTH_STORE" => {
                let store = match value.as_str() {
                    "file" => Some(AuthStoreKind::File),
                    "keyring" => Some(AuthStoreKind::Keyring),
                    _ => None,
                };
                if let Some(store) = store {
                    partial.auth.get_or_insert_with(Default::default).store = Some(store);
                }
            }
            "XLIGHTCLI_TOOLS_SHELL_TIMEOUT_SECS" => {
                if let Ok(secs) = value.parse() {
                    partial
                        .tools
                        .get_or_insert_with(PartialToolsConfig::default)
                        .shell_timeout_secs = Some(secs);
                }
            }
            _ => {}
        }
    }
    partial
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::schema::PermissionMode;

    fn vars(pairs: &[(&str, &str)]) -> impl Iterator<Item = (String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn recognizes_default_provider() {
        let partial = partial_from_env(vars(&[("XLIGHTCLI_DEFAULT_PROVIDER", "codex")]));
        assert_eq!(partial.default_provider, Some(ProviderId::new("codex")));
    }

    #[test]
    fn recognizes_permission_mode() {
        let partial = partial_from_env(vars(&[("XLIGHTCLI_PERMISSIONS_MODE", "strict")]));
        assert_eq!(
            partial.permissions.unwrap().mode,
            Some(PermissionMode::Strict)
        );
    }

    #[test]
    fn unrecognized_variables_are_ignored() {
        let partial = partial_from_env(vars(&[("XLIGHTCLI_SOME_FUTURE_KEY", "x")]));
        assert_eq!(partial, PartialConfig::empty());
    }

    #[test]
    fn unrelated_variables_are_ignored() {
        let partial = partial_from_env(vars(&[("PATH", "/usr/bin")]));
        assert_eq!(partial, PartialConfig::empty());
    }
}
