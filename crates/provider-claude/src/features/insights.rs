// SPDX-License-Identifier: GPL-3.0-only

//! `claude.insights` (Compatible, D-021, docs/commands.md §3.2).
//!
//! Upstream (Claude Code `/insights`) pipeline, per `docs/providers/claude.md` (H for the
//! pipeline shape, U for exact layout details): reads up to 200 not-yet-analyzed sessions,
//! (1) computes per-session metrics (message/tool counts, languages touched, lines
//! added/removed, files touched, response latency, tool errors, MCP/web-search/subagent usage),
//! (2) has the model extract per-session "facets" (goal, category, outcome, friction,
//! satisfaction, one-line summary) — cached per session so re-runs are incremental — then
//! (3) has the model write a narrative report assembled into a **self-contained HTML** file:
//! stats, "At a Glance", "What You Work On", "How You Use Claude", "Where Things Go Wrong",
//! "Features to Try" (can suggest new rules-file entries), plus charts for tools/languages/
//! outcomes/friction.
//!
//! xlightcli's planned equivalent: source data is xlightcli's own SQLite session log (not a
//! transcript import), capped at 200 unanalyzed sessions per run; facets cached in a dedicated
//! table keyed by `session_id`; output written to
//! `$XDG_DATA_HOME/xlightcli/insights/<workspace-id>/insights-<unix-ts>.html` (D-021), with the
//! command returning that path. **None of this is implemented yet**: Phase 0 has no session
//! storage (`xlightcli-storage` is still an empty skeleton, CODEBASE.md §2), no facet cache
//! table, and no HTML report renderer — so `execute` below always returns
//! `CommandResult::Unavailable` with a reason naming the missing dependency, per INV-10 /
//! PATTERNS.md §12 ("an unavailable command ⇒ `Unavailable` — never fake a result").

use xlightcli_protocol::{CapabilityMode, CommandId};
use xlightcli_provider::{
    CommandContext, CommandDefinition, CommandError, CommandResult, ProviderCommand,
};

pub(crate) fn definition() -> CommandDefinition {
    CommandDefinition {
        id: CommandId::new("claude", "insights"),
        alias: "insights",
        mode: CapabilityMode::Compatible,
        requires_transport: None,
        summary: "Analyze session history and suggest workflow improvements",
    }
}

pub(crate) async fn execute(
    _cmd: ProviderCommand,
    _ctx: CommandContext,
) -> Result<CommandResult, CommandError> {
    Ok(CommandResult::Unavailable {
        reason: "claude.insights requires session history + a facet cache (Phase 2, \
                 xlightcli-storage §2) — not implemented in Phase 0"
            .to_string(),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use xlightcli_protocol::SessionId;

    use super::*;

    #[tokio::test]
    async fn insights_reports_unavailable_with_a_specific_reason_in_phase_0() {
        let cmd = ProviderCommand {
            id: definition().id,
            raw_args: String::new(),
        };
        let ctx = CommandContext {
            session: SessionId::new(),
        };
        let result = execute(cmd, ctx).await.unwrap();
        match result {
            CommandResult::Unavailable { reason } => {
                assert!(reason.contains("Phase 2") || reason.contains("session history"));
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    #[test]
    fn definition_uses_the_documented_namespaced_id_and_alias() {
        let def = definition();
        assert_eq!(def.id.as_str(), "claude.insights");
        assert_eq!(def.alias, "insights");
        assert_eq!(def.mode, CapabilityMode::Compatible);
    }
}
