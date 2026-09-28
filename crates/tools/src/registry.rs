// SPDX-License-Identifier: GPL-3.0-only

//! `ToolRegistry` — the set of tools available to a turn (PATTERNS.md §7). `runtime`'s
//! `ContextManager` reads [`ToolRegistry::definitions`] to build a `TurnRequest`'s `tools` field;
//! the tool executor reads [`ToolRegistry::get`] to dispatch a `ToolUse` call.

use std::collections::BTreeMap;
use std::sync::Arc;

use xlightcli_protocol::ToolDefinition;

use crate::tool::Tool;

/// A named collection of tools. Cheap to clone (`Arc`'d contents); `runtime` holds one per active
/// agent profile (docs/PLAN.md §7.2: an agent's `tools` list can be restricted, e.g.
/// `["read_file", "grep", "git_diff"]` for a read-only reviewer profile).
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRegistry")
            .field("tools", &self.tools.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `tool`, keyed by its definition's `name`. Registering the same name twice
    /// replaces the previous entry (last write wins) rather than erroring — callers that need to
    /// detect a duplicate should check [`Self::get`] first.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.definition().name.clone();
        self.tools.insert(name, tool);
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// `ToolDefinition`s for every registered tool, in name order — what `ContextManager` puts on
    /// a `TurnRequest`.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .map(|t| t.definition().clone())
            .collect()
    }

    /// A registry restricted to `names` (docs/PLAN.md §7.2 per-agent-profile tool lists). Unknown
    /// names are silently skipped — callers that need to validate a profile's tool list should
    /// check `self.contains(name)` themselves and surface a config error there.
    pub fn subset(&self, names: &[impl AsRef<str>]) -> Self {
        let mut subset = Self::new();
        for name in names {
            if let Some(tool) = self.tools.get(name.as_ref()) {
                subset
                    .tools
                    .insert(name.as_ref().to_string(), Arc::clone(tool));
            }
        }
        subset
    }

    /// Registers every Phase-1 built-in tool (`crate::builtin`, docs/PLAN.md §7.2). Bodies are
    /// stubs (Wave A) — see the `builtin` module doc.
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        crate::builtin::register_all(&mut registry);
        registry
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn with_builtins_registers_every_phase_1_tool() {
        let registry = ToolRegistry::with_builtins();
        for name in [
            "read_file",
            "write_file",
            "edit_file",
            "list_dir",
            "glob",
            "grep",
            "shell",
            "git_status",
            "git_diff",
        ] {
            assert!(registry.contains(name), "missing built-in tool {name:?}");
        }
        assert_eq!(registry.len(), 9);
    }

    #[test]
    fn subset_keeps_only_named_tools() {
        let registry = ToolRegistry::with_builtins();
        let subset = registry.subset(&["read_file", "grep"]);
        assert_eq!(subset.len(), 2);
        assert!(subset.contains("read_file"));
        assert!(!subset.contains("shell"));
    }
}
