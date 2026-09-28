// SPDX-License-Identifier: GPL-3.0-only

//! `ExperimentalFlags` and a minimal `Config` stub — just enough for
//! `provider::gate::TransportGate` to be exercised end to end in Phase 0 (docs/PLAN.md §15,
//! D-002). Full layered config (`layer`, `merge`, `schema`, `trust` — docs/PLAN.md §12) is a
//! later phase.

use serde::{Deserialize, Serialize};
use xlightcli_protocol::{ProviderId, TransportId};

/// Runtime opt-in for experimental (subscription) transports. Only ever read from the **global**
/// config layer (docs/PLAN.md §12.4): project config can never flip these on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentalFlags {
    #[serde(default)]
    pub claude_subscription: bool,
    #[serde(default)]
    pub antigravity_subscription: bool,
}

impl ExperimentalFlags {
    pub fn all_disabled() -> Self {
        Self::default()
    }
}

/// Minimal config stub. Grows into the full layered `Config` (paths, providers, agents, hooks,
/// ...) in Phase 1; for Phase 0 it only carries what `dev probe` and the transport gate need.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub default_provider: Option<ProviderId>,
    #[serde(default)]
    pub experimental: ExperimentalFlags,
    /// Kill switch (docs/PLAN.md §15): transport ids disabled regardless of opt-in state.
    #[serde(default)]
    pub disabled_transports: Vec<TransportId>,
}

impl Config {
    /// `true` when `transport` is not present in `disabled_transports`.
    pub fn transport_allowed(&self, transport_id: &TransportId) -> bool {
        !self.disabled_transports.iter().any(|t| t == transport_id)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn experimental_flags_default_to_disabled() {
        let flags = ExperimentalFlags::default();
        assert!(!flags.claude_subscription);
        assert!(!flags.antigravity_subscription);
    }

    #[test]
    fn config_roundtrips_through_toml() {
        let cfg = Config {
            default_provider: Some(ProviderId::new("codex")),
            experimental: ExperimentalFlags {
                claude_subscription: true,
                antigravity_subscription: false,
            },
            disabled_transports: vec![TransportId::new("antigravity")],
        };
        let text = toml::to_string(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn transport_allowed_respects_kill_switch() {
        let cfg = Config {
            disabled_transports: vec![TransportId::new("antigravity")],
            ..Default::default()
        };
        assert!(!cfg.transport_allowed(&TransportId::new("antigravity")));
        assert!(cfg.transport_allowed(&TransportId::new("gemini-api")));
    }
}
