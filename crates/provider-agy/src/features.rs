// SPDX-License-Identifier: GPL-3.0-only

//! `ProviderFeaturePack` impl for `agy` (docs/commands.md §3.1).
//!
//! Phase 0: every command is *declared* here (so `/provider info` and command discovery see the
//! full agy command surface) but `execute` always returns `CommandResult::Unavailable` with a
//! reason — never a faked result (INV-10). `boost`/`teamwork`/`grill-me`/`browser` are Compatible
//! recipes that need multi-agent/worktree/browser-tool infrastructure landing in later phases;
//! `credits`/`remote-control`/`voice`/`feedback` are genuinely Unsupported upstream features (paid
//! plan, reverse tunnel, consumer-only OAuth scope, Google-internal feedback); `changelog` and
//! `planning` are Core-ish aliases xlightcli itself will serve once `runtime::CommandRegistry`
//! exists (Phase 2).

use async_trait::async_trait;
use xlightcli_protocol::{CapabilityMode, CommandId};
use xlightcli_provider::{
    CommandContext, CommandDefinition, CommandError, CommandResult, ProviderCommand,
    ProviderFeaturePack,
};

#[derive(Debug)]
pub(crate) struct AgyFeaturePack;

fn command(
    name: &'static str,
    alias: &'static str,
    mode: CapabilityMode,
    summary: &'static str,
) -> CommandDefinition {
    CommandDefinition {
        id: CommandId::new("agy", name),
        alias,
        mode,
        requires_transport: None,
        summary,
    }
}

#[async_trait]
impl ProviderFeaturePack for AgyFeaturePack {
    fn commands(&self) -> Vec<CommandDefinition> {
        vec![
            command(
                "boost",
                "/boost",
                CapabilityMode::Compatible,
                "Orchestrator plans verifiable subtasks, runs isolated subagents in parallel, merges and re-tests",
            ),
            command(
                "teamwork",
                "/teamwork",
                CapabilityMode::Compatible,
                "Scoped multi-role team (orchestrator/explorers/workers/critic/auditor) for larger tasks",
            ),
            command(
                "grill-me",
                "/grill-me",
                CapabilityMode::Compatible,
                "Interview the user on architecture/error-handling/perf/compat before writing code",
            ),
            command(
                "browser",
                "/browser",
                CapabilityMode::Compatible,
                "Sandboxed browser subagent (needs a CDP browser tool)",
            ),
            command(
                "credits",
                "/credits",
                CapabilityMode::Unsupported,
                "AI/G1 credit balance (paid-plan feature; no public balance endpoint found)",
            ),
            command(
                "remote-control",
                "/remote-control",
                CapabilityMode::Unsupported,
                "Reverse tunnel to the antigravity.google dashboard",
            ),
            command(
                "voice",
                "/voice",
                CapabilityMode::Unsupported,
                "Dictation (consumer-only OAuth scope)",
            ),
            command(
                "feedback",
                "/feedback",
                CapabilityMode::Unsupported,
                "Send feedback to Google",
            ),
            command(
                "changelog",
                "/changelog",
                CapabilityMode::Core,
                "xlightcli release notes",
            ),
            command(
                "planning",
                "/planning",
                CapabilityMode::Core,
                "Legacy alias for /plan",
            ),
        ]
    }

    async fn execute(
        &self,
        cmd: ProviderCommand,
        _ctx: CommandContext,
    ) -> Result<CommandResult, CommandError> {
        let reason = match cmd.id.as_str() {
            "agy.boost" => "planned: Phase 4-5 (needs multi-agent + worktree orchestration)".to_string(),
            "agy.teamwork" => "planned: Phase 5+ (needs the full multi-role team recipe)".to_string(),
            "agy.grill-me" => "planned: Phase 2 (needs runtime::CommandRegistry to start a turn)".to_string(),
            "agy.browser" => "planned: Phase 6+ (needs a CDP browser tool)".to_string(),
            "agy.credits" => "unsupported: no public credit-balance endpoint has been found for xlightcli to call".to_string(),
            "agy.remote-control" => "unsupported: xlightcli does not implement a reverse tunnel to antigravity.google".to_string(),
            "agy.voice" => "unsupported: dictation requires a consumer-only OAuth scope xlightcli does not request".to_string(),
            "agy.feedback" => "unsupported: use the xlightcli issue tracker instead of Google's in-app feedback".to_string(),
            "agy.changelog" => "planned: Phase 2 (xlightcli release notes viewer via runtime::CommandRegistry)".to_string(),
            "agy.planning" => "alias for core.plan, not yet wired to runtime::CommandRegistry (Phase 2)".to_string(),
            other => format!("agy: unknown command {other}"),
        };
        Ok(CommandResult::Unavailable { reason })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use xlightcli_protocol::SessionId;

    use super::*;

    #[test]
    fn declares_every_agy_command_from_docs_commands_md() {
        let ids: Vec<String> = AgyFeaturePack
            .commands()
            .iter()
            .map(|c| c.id.as_str().to_string())
            .collect();
        for expected in [
            "agy.boost",
            "agy.teamwork",
            "agy.grill-me",
            "agy.browser",
            "agy.credits",
            "agy.remote-control",
            "agy.voice",
            "agy.feedback",
            "agy.changelog",
            "agy.planning",
        ] {
            assert!(
                ids.contains(&expected.to_string()),
                "missing command {expected}"
            );
        }
    }

    #[tokio::test]
    async fn unsupported_commands_are_unavailable_not_faked() {
        let pack = AgyFeaturePack;
        let ctx = CommandContext {
            session: SessionId::new(),
        };
        let result = pack
            .execute(
                ProviderCommand {
                    id: CommandId::new("agy", "credits"),
                    raw_args: String::new(),
                },
                ctx,
            )
            .await
            .unwrap();
        match result {
            CommandResult::Unavailable { reason } => assert!(reason.contains("unsupported")),
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn planned_commands_are_unavailable_with_a_phase_reason() {
        let pack = AgyFeaturePack;
        let ctx = CommandContext {
            session: SessionId::new(),
        };
        let result = pack
            .execute(
                ProviderCommand {
                    id: CommandId::new("agy", "boost"),
                    raw_args: "do the thing".into(),
                },
                ctx,
            )
            .await
            .unwrap();
        match result {
            CommandResult::Unavailable { reason } => assert!(reason.contains("planned")),
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }
}
