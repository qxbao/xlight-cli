// SPDX-License-Identifier: GPL-3.0-only

//! `ExperimentalFlags` (docs/PLAN.md §15, D-002). Kept in its own module (rather than folded into
//! `schema`) since it predates Phase 1 and other crates (`provider-*`) already import it from
//! here; `schema::Config::experimental` uses this same type.

use serde::{Deserialize, Serialize};

/// Runtime opt-in for experimental (subscription) transports. Only ever read from the **global**
/// config layer (docs/PLAN.md §12.4): project config can never flip these on
/// (`ConfigLoader` strips `experimental` from project/project.local layers unconditionally).
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn experimental_flags_default_to_disabled() {
        let flags = ExperimentalFlags::default();
        assert!(!flags.claude_subscription);
        assert!(!flags.antigravity_subscription);
    }

    #[test]
    fn roundtrips_through_toml() {
        let flags = ExperimentalFlags {
            claude_subscription: true,
            antigravity_subscription: false,
        };
        let text = toml::to_string(&flags).unwrap();
        let back: ExperimentalFlags = toml::from_str(&text).unwrap();
        assert_eq!(back, flags);
    }
}
