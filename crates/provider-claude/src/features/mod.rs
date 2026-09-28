// SPDX-License-Identifier: GPL-3.0-only

//! `ProviderFeaturePack` impl for Claude (docs/commands.md §3.2). Phase 0 only wires up
//! `claude.insights`, which itself reports `Unavailable` (see `insights.rs`) — the pack exists so
//! the command is discoverable (`/claude:insights` resolves and explains *why* it can't run yet)
//! rather than silently missing.

mod insights;

use async_trait::async_trait;
use xlightcli_provider::{
    CommandContext, CommandDefinition, CommandError, CommandResult, ProviderCommand,
    ProviderFeaturePack,
};

#[derive(Debug, Default)]
pub(crate) struct ClaudeFeaturePack;

#[async_trait]
impl ProviderFeaturePack for ClaudeFeaturePack {
    fn commands(&self) -> Vec<CommandDefinition> {
        vec![insights::definition()]
    }

    async fn execute(
        &self,
        cmd: ProviderCommand,
        ctx: CommandContext,
    ) -> Result<CommandResult, CommandError> {
        if cmd.id == insights::definition().id {
            return insights::execute(cmd, ctx).await;
        }
        Ok(CommandResult::Unavailable {
            reason: format!("claude has no command `{}`", cmd.id),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use xlightcli_protocol::SessionId;

    use super::*;

    #[test]
    fn commands_lists_insights() {
        let pack = ClaudeFeaturePack;
        let commands = pack.commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].alias, "insights");
    }

    #[tokio::test]
    async fn execute_routes_unknown_command_to_unavailable() {
        let pack = ClaudeFeaturePack;
        let result = pack
            .execute(
                ProviderCommand {
                    id: xlightcli_protocol::CommandId::new("claude", "nope"),
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
