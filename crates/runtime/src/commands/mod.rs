// SPDX-License-Identifier: GPL-3.0-only

//! `CommandRegistry` + core command ids (docs/PLAN.md §5, PATTERNS.md §12, docs/commands.md §2).

pub mod core;

pub use core::CORE_COMMANDS;

/// Registered commands available to a session: core (this crate) first, then — once Phase 2
/// lands a real `ProviderFeaturePack` — the active provider's commands. Only core commands exist
/// in Phase 1.
#[derive(Debug, Clone)]
pub struct CommandRegistry {
    core: Vec<xlightcli_provider::CommandDefinition>,
}

impl CommandRegistry {
    pub fn with_core_commands() -> Self {
        Self {
            core: CORE_COMMANDS.iter().map(core::definition).collect(),
        }
    }

    /// Resolves an alias (without the leading `/`) to its `CommandDefinition`. Resolution order
    /// (docs/commands.md §1.7): core first; a provider `FeaturePack` would be checked next once
    /// Phase 2 exists.
    pub fn resolve(&self, alias: &str) -> Option<&xlightcli_provider::CommandDefinition> {
        self.core.iter().find(|def| def.alias == alias)
    }

    pub fn core_commands(&self) -> &[xlightcli_provider::CommandDefinition] {
        &self.core
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn every_phase_1_core_command_resolves_by_its_primary_alias() {
        let registry = CommandRegistry::with_core_commands();
        for spec in CORE_COMMANDS {
            let resolved = registry.resolve(spec.alias).unwrap_or_else(|| {
                panic!(
                    "core command {:?} did not resolve by alias {:?}",
                    spec.name, spec.alias
                )
            });
            assert_eq!(resolved.alias, spec.alias);
        }
    }

    #[test]
    fn unknown_alias_does_not_resolve() {
        let registry = CommandRegistry::with_core_commands();
        assert!(registry.resolve("not-a-command").is_none());
    }
}
