// SPDX-License-Identifier: GPL-3.0-only

//! Canonical fragment produced by a `provider::ConfigImporter` (docs/PLAN.md §4.1, D-027).
//!
//! Kept intentionally minimal for Phase 0: `config`/`mcp` don't merge this in yet (that's
//! Phase 3, `docs/import.md`). The shape only needs to be stable enough that adapter crates can
//! start returning real data without another protocol change.

use serde::{Deserialize, Serialize};

/// Provider-agnostic result of importing another CLI's config/credentials (read-only, D-017).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConfigFragment {
    /// Raw sections keyed by target config table (e.g. `"mcp"`, `"rules"`), left as JSON until
    /// `config::merge` understands the shape (Phase 3).
    #[serde(default)]
    pub sections: serde_json::Map<String, serde_json::Value>,
    /// Human-readable summary of what was found/imported, shown by `/import`.
    pub summary: String,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn config_fragment_roundtrips_through_json() {
        let mut fragment = ConfigFragment {
            summary: "imported 1 MCP server".into(),
            ..Default::default()
        };
        fragment.sections.insert(
            "mcp".into(),
            serde_json::json!({"github": {"transport": "stdio"}}),
        );
        let json = serde_json::to_string(&fragment).unwrap();
        let back: ConfigFragment = serde_json::from_str(&json).unwrap();
        assert_eq!(back, fragment);
    }
}
