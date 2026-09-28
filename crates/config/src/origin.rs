// SPDX-License-Identifier: GPL-3.0-only

//! `Origin` tracking for `config show --origin` / `core.config` (docs/PLAN.md §12.2,
//! docs/commands.md §2 `core.config`).
//!
//! **Scope decision (documented, Phase 1):** origins are tracked at **top-level section**
//! granularity (`default_provider`, `provider`, `permissions`, `tools`, ...), not per leaf key.
//! This is enough for `config show --origin` to say "which layer touched `[tools]`" without the
//! considerably larger bookkeeping needed for "which layer touched `tools.shell_timeout_secs`
//! specifically". Refine to leaf-level if a later wave needs it — the section names below
//! (`SECTION_NAMES`) are exactly the top-level [`crate::partial::PartialConfig`] field names, so
//! extending is additive.

use std::collections::BTreeMap;

/// Which layer last set a section, in the order layers are applied (PATTERNS.md §11,
/// docs/PLAN.md §12.2). `Ord` follows that same precedence order (`Default` lowest,
/// `Cli` highest) so origins can be compared/sorted meaningfully.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Origin {
    Default,
    Global,
    Project,
    ProjectLocal,
    Env,
    Cli,
}

impl std::fmt::Display for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Default => "default",
            Self::Global => "global",
            Self::Project => "project",
            Self::ProjectLocal => "project.local",
            Self::Env => "env",
            Self::Cli => "cli",
        };
        f.write_str(s)
    }
}

/// Section names tracked by [`OriginMap`] — exactly the top-level field names of
/// [`crate::partial::PartialConfig`].
pub const SECTION_NAMES: &[&str] = &[
    "default_provider",
    "provider",
    "experimental",
    "disabled_transports",
    "permissions",
    "agents",
    "concurrency",
    "budget",
    "context",
    "tools",
    "auth",
    "rules",
    "ui",
];

/// Records, per top-level config section, which layer last set it. Any section never recorded is
/// implicitly [`Origin::Default`] (see [`OriginMap::get`]).
#[derive(Debug, Clone, Default)]
pub struct OriginMap {
    sections: BTreeMap<&'static str, Origin>,
}

impl OriginMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `origin` for `section` if it isn't already `Some` in that layer's partial (callers
    /// only call this for sections the layer actually set).
    pub fn record(&mut self, section: &'static str, origin: Origin) {
        self.sections.insert(section, origin);
    }

    /// Origin of `section`; [`Origin::Default`] if no layer ever set it.
    pub fn get(&self, section: &str) -> Origin {
        self.sections
            .get(section)
            .copied()
            .unwrap_or(Origin::Default)
    }

    /// All sections with a non-default origin, in section-name order.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, Origin)> + '_ {
        self.sections.iter().map(|(k, v)| (*k, *v))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn unset_section_defaults_to_default_origin() {
        let map = OriginMap::new();
        assert_eq!(map.get("tools"), Origin::Default);
    }

    #[test]
    fn later_record_overwrites_earlier() {
        let mut map = OriginMap::new();
        map.record("tools", Origin::Global);
        map.record("tools", Origin::Cli);
        assert_eq!(map.get("tools"), Origin::Cli);
    }

    #[test]
    fn ordering_matches_layer_precedence() {
        assert!(Origin::Default < Origin::Global);
        assert!(Origin::Global < Origin::Project);
        assert!(Origin::Project < Origin::ProjectLocal);
        assert!(Origin::ProjectLocal < Origin::Env);
        assert!(Origin::Env < Origin::Cli);
    }
}
