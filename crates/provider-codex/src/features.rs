// SPDX-License-Identifier: GPL-3.0-only

//! `ProviderFeaturePack` impl for Codex. Per `docs/commands.md` §3.3, every Codex-specific
//! command is scheduled for Phase 2+ (`codex.archive`/`codex.delete`, Phase 2),
//! Phase 5 (`codex.worktree`) or Phase 6 (`codex.personality`, `codex.raw`) — none belong in
//! Phase 0. Returning an empty command list (rather than fake `Unavailable` entries for commands
//! that don't exist yet) matches INV-10: no faked behavior.

use async_trait::async_trait;
use xlightcli_provider::{
    CommandContext, CommandDefinition, CommandError, CommandResult, ProviderCommand,
    ProviderFeaturePack,
};

#[derive(Debug, Default)]
pub struct CodexFeaturePack;

#[async_trait]
impl ProviderFeaturePack for CodexFeaturePack {
    fn commands(&self) -> Vec<CommandDefinition> {
        Vec::new()
    }

    async fn execute(
        &self,
        cmd: ProviderCommand,
        _ctx: CommandContext,
    ) -> Result<CommandResult, CommandError> {
        Ok(CommandResult::Unavailable {
            reason: format!(
                "codex feature pack has no commands yet (Phase 0); requested `{}`",
                cmd.id
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use xlightcli_protocol::{CommandId, SessionId};

    use super::*;

    #[tokio::test]
    async fn no_commands_are_registered_yet() {
        let pack = CodexFeaturePack;
        assert!(pack.commands().is_empty());
    }

    #[tokio::test]
    async fn execute_reports_unavailable_instead_of_faking_a_result() {
        let pack = CodexFeaturePack;
        let result = pack
            .execute(
                ProviderCommand {
                    id: CommandId::new("codex", "personality"),
                    raw_args: String::new(),
                },
                CommandContext {
                    session: SessionId::new(),
                },
            )
            .await
            .unwrap();
        assert!(matches!(result, CommandResult::Unavailable { .. }));
    }
}
