// SPDX-License-Identifier: GPL-3.0-only

//! Phase 1 core command ids (docs/commands.md §2, this Wave A brief's explicit list): `/help
//! /exit /clear /resume /model /context /compact /diff /permissions /config /status /login
//! /logout` plus `/mode` (Shift+Tab cycle, D-025). Every one of these is `mode: Core` — none is
//! transport-gated.
//!
//! Bodies live in `RuntimeHandle::run_command` (Wave B); this module only owns the static
//! metadata (`CommandDefinition`) each resolves to.

use xlightcli_protocol::{CapabilityMode, CommandId};
use xlightcli_provider::CommandDefinition;

/// One core command's static metadata, before it's turned into a `CommandDefinition` (which needs
/// an owned `CommandId`, not const-constructible from a `&'static str` pair).
#[derive(Debug)]
pub struct CoreCommandSpec {
    pub name: &'static str,
    pub alias: &'static str,
    pub summary: &'static str,
}

pub fn definition(spec: &CoreCommandSpec) -> CommandDefinition {
    CommandDefinition {
        id: CommandId::new("core", spec.name),
        alias: spec.alias,
        mode: CapabilityMode::Core,
        requires_transport: None,
        summary: spec.summary,
    }
}

/// Phase 1 core commands (docs/commands.md §2). Order matches the Wave A brief's list.
pub const CORE_COMMANDS: &[CoreCommandSpec] = &[
    CoreCommandSpec {
        name: "help",
        alias: "help",
        summary: "Show help: general, commands, shortcuts.",
    },
    CoreCommandSpec {
        name: "exit",
        alias: "exit",
        summary: "Exit xlightcli (confirms if an agent is still running).",
    },
    CoreCommandSpec {
        name: "clear",
        alias: "clear",
        summary: "Start a new conversation; the previous session can still be resumed.",
    },
    CoreCommandSpec {
        name: "resume",
        alias: "resume",
        summary: "Pick a previous session for this workspace.",
    },
    CoreCommandSpec {
        name: "model",
        alias: "model",
        summary: "Pick a model, or run one prompt with a different model then return.",
    },
    CoreCommandSpec {
        name: "context",
        alias: "context",
        summary: "Show a breakdown of context window usage.",
    },
    CoreCommandSpec {
        name: "compact",
        alias: "compact",
        summary: "Summarize the conversation to free up context.",
    },
    CoreCommandSpec {
        name: "diff",
        alias: "diff",
        summary: "Show a diff view of the working tree (including untracked files).",
    },
    CoreCommandSpec {
        name: "permissions",
        alias: "permissions",
        summary: "Show/edit allow-ask-deny rules by scope.",
    },
    CoreCommandSpec {
        name: "config",
        alias: "config",
        summary: "Show or edit the layered configuration.",
    },
    CoreCommandSpec {
        name: "status",
        alias: "status",
        summary: "Show version, model, account, transport, connectivity, token usage.",
    },
    CoreCommandSpec {
        name: "login",
        alias: "login",
        summary: "Log into a provider (browser OAuth, device code, or API key).",
    },
    CoreCommandSpec {
        name: "logout",
        alias: "logout",
        summary: "Log out of a provider (revokes and removes the stored credential).",
    },
    CoreCommandSpec {
        name: "mode",
        alias: "mode",
        summary: "Cycle execution mode: default -> accept-edits -> plan (also bound to Shift+Tab).",
    },
];
