// SPDX-License-Identifier: GPL-3.0-only

//! Permissions (docs/PLAN.md §7.3, docs/commands.md §4): execution mode, permission mode/rules,
//! and the `PermissionGate` boundary a tool calls into *before* its side effect.
//!
//! `PermissionMode`/`ExecutionMode` are re-exported from `xlightcli_config` (the schema owns the
//! persisted shape; `tools` is where they're actually *used*, per the Wave A brief) so there is
//! exactly one definition of each, not two types that could drift apart.

use std::path::Path;

use async_trait::async_trait;
pub use xlightcli_config::{ExecutionMode, PermissionMode, PermissionRule, RuleEffect};

use crate::tool::ToolError;

/// One action a tool is about to perform, matched against `PermissionRule`s (docs/commands.md
/// §4: `read_file`, `write_file`, `command`, `read_url`, `mcp`, `unsandboxed`).
#[derive(Debug, Clone, Copy)]
pub enum PermissionAction<'a> {
    ReadFile(&'a Path),
    WriteFile(&'a Path),
    Command(&'a str),
    ReadUrl(&'a str),
    Mcp(&'a str),
    /// Running a tool/process outside any sandbox (Phase 6+ OS sandbox, docs/PLAN.md §7.3) —
    /// distinct from `Command` so a policy can allow ordinary shell commands while still asking
    /// before anything unsandboxed.
    Unsandboxed,
}

impl PermissionAction<'_> {
    /// The `action` half of `action(target)` (docs/PLAN.md §7.3).
    pub fn action_name(&self) -> &'static str {
        match self {
            Self::ReadFile(_) => "read_file",
            Self::WriteFile(_) => "write_file",
            Self::Command(_) => "command",
            Self::ReadUrl(_) => "read_url",
            Self::Mcp(_) => "mcp",
            Self::Unsandboxed => "unsandboxed",
        }
    }

    /// The candidate string matched against a rule's `target` pattern.
    pub fn target(&self) -> &str {
        match self {
            Self::ReadFile(p) | Self::WriteFile(p) => p.to_str().unwrap_or_default(),
            Self::Command(c) | Self::ReadUrl(c) | Self::Mcp(c) => c,
            Self::Unsandboxed => "",
        }
    }

    /// Whether this action only reads (never writes/executes) — used by `PermissionMode`'s
    /// built-in defaults for modes that don't have an explicit rule match.
    pub fn is_read_only(&self) -> bool {
        matches!(self, Self::ReadFile(_) | Self::ReadUrl(_))
    }
}

/// Result of evaluating a [`PermissionAction`] against the engine's rules/mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    /// The caller (a `PermissionGate` impl, typically in `runtime`) must prompt the user; `reason`
    /// is a short, human-readable explanation for the prompt.
    Ask {
        reason: String,
    },
    Deny {
        reason: String,
    },
}

/// A pending "ask" decision, shaped for a UI prompt (`runtime`'s `PermissionGate` impl sends this
/// to the TUI and awaits the user's answer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRequest {
    pub action: String,
    pub target: String,
    pub tool_call_id: Option<xlightcli_protocol::ToolCallId>,
    pub reason: String,
}

/// Evaluates `action(target)` rules with **deny > ask > allow** precedence (docs/PLAN.md §7.3),
/// falling back to a mode-driven default when nothing matches.
#[derive(Debug, Clone)]
pub struct PermissionEngine {
    mode: PermissionMode,
    rules: Vec<PermissionRule>,
}

impl PermissionEngine {
    pub fn new(mode: PermissionMode, rules: Vec<PermissionRule>) -> Self {
        Self { mode, rules }
    }

    /// Builds an engine straight from a config section (`cfg.rules()` already parses the raw
    /// `action(target)` strings, docs/PLAN.md §12.4).
    pub fn from_config(cfg: &xlightcli_config::PermissionsConfig) -> Self {
        Self::new(cfg.mode, cfg.rules())
    }

    pub fn mode(&self) -> PermissionMode {
        self.mode
    }

    /// Matches `pattern` against `candidate`: `*` / glob syntax by default, or a `regex:<pattern>`
    /// prefix for a full regular expression. An invalid pattern never matches (fails closed).
    fn matches(pattern: &str, candidate: &str) -> bool {
        if let Some(re) = pattern.strip_prefix("regex:") {
            return regex_lite::Regex::new(re).is_ok_and(|r| r.is_match(candidate));
        }
        globset::Glob::new(pattern).is_ok_and(|g| g.compile_matcher().is_match(candidate))
    }

    fn matching_rules<'a>(&'a self, action: &PermissionAction<'_>) -> Vec<&'a PermissionRule> {
        let action_name = action.action_name();
        let target = action.target();
        self.rules
            .iter()
            .filter(|rule| rule.action == action_name && Self::matches(&rule.target, target))
            .collect()
    }

    /// The mode's default when no rule matches (docs/PLAN.md §7.3, docs/commands.md §4).
    fn mode_default(&self, action: &PermissionAction<'_>) -> PermissionDecision {
        let allow = || PermissionDecision::Allow;
        let ask = || PermissionDecision::Ask {
            reason: format!(
                "{} requires approval under permission mode {:?}",
                action.action_name(),
                self.mode
            ),
        };
        let deny = || PermissionDecision::Deny {
            reason: format!(
                "{} is not permitted under permission mode {:?}",
                action.action_name(),
                self.mode
            ),
        };
        match self.mode {
            PermissionMode::ReadOnly => {
                if action.is_read_only() {
                    allow()
                } else {
                    deny()
                }
            }
            PermissionMode::Strict => {
                if action.is_read_only() {
                    allow()
                } else {
                    ask()
                }
            }
            PermissionMode::Ask => {
                if action.is_read_only() {
                    allow()
                } else {
                    ask()
                }
            }
            PermissionMode::AutoEdit => match action {
                PermissionAction::ReadFile(_)
                | PermissionAction::ReadUrl(_)
                | PermissionAction::WriteFile(_) => allow(),
                PermissionAction::Unsandboxed => deny(),
                _ => ask(),
            },
            PermissionMode::FullAuto => allow(),
        }
    }

    /// Evaluates one action. Precedence: any matching `deny` rule wins outright; else any
    /// matching `ask`; else any matching `allow`; else the mode's default.
    pub fn evaluate(&self, action: &PermissionAction<'_>) -> PermissionDecision {
        let matches = self.matching_rules(action);
        if let Some(rule) = matches.iter().find(|r| r.effect == RuleEffect::Deny) {
            return PermissionDecision::Deny {
                reason: format!("denied by rule {}", rule.to_raw()),
            };
        }
        if let Some(rule) = matches.iter().find(|r| r.effect == RuleEffect::Ask) {
            return PermissionDecision::Ask {
                reason: format!("rule {} requires approval", rule.to_raw()),
            };
        }
        if matches.iter().any(|r| r.effect == RuleEffect::Allow) {
            return PermissionDecision::Allow;
        }
        self.mode_default(action)
    }
}

/// The async boundary a tool calls through before its side effect (PATTERNS.md §7). Implemented
/// by `runtime`: routes `Ask` to a UI permission dialog and awaits the user's answer; `Allow`
/// returns immediately; `Deny` returns a `ToolError` without ever calling the UI.
#[async_trait]
pub trait PermissionGate: Send + Sync + std::fmt::Debug {
    async fn check(&self, action: PermissionAction<'_>) -> Result<(), ToolError>;
}

/// Test/dev-only gate: evaluates a [`PermissionEngine`] synchronously and never prompts —
/// `Ask` is resolved immediately per `ask_policy`. **Never** wire this into the real TUI; it
/// exists so built-in-tool and `ToolContext` tests don't need a fake UI.
#[derive(Debug)]
pub struct StaticPermissionGate {
    engine: PermissionEngine,
    ask_policy: AskPolicy,
}

/// How [`StaticPermissionGate`] resolves an `Ask` decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskPolicy {
    AutoAllow,
    AutoDeny,
}

impl StaticPermissionGate {
    pub fn new(engine: PermissionEngine, ask_policy: AskPolicy) -> Self {
        Self { engine, ask_policy }
    }
}

#[async_trait]
impl PermissionGate for StaticPermissionGate {
    async fn check(&self, action: PermissionAction<'_>) -> Result<(), ToolError> {
        match self.engine.evaluate(&action) {
            PermissionDecision::Allow => Ok(()),
            PermissionDecision::Ask { reason } => match self.ask_policy {
                AskPolicy::AutoAllow => Ok(()),
                AskPolicy::AutoDeny => Err(ToolError::PermissionDenied(reason)),
            },
            PermissionDecision::Deny { reason } => Err(ToolError::PermissionDenied(reason)),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::Path;

    use pretty_assertions::assert_eq;

    use super::*;

    fn rule(effect: RuleEffect, raw: &str) -> PermissionRule {
        PermissionRule::parse(effect, raw).unwrap()
    }

    #[test]
    fn deny_beats_ask_beats_allow() {
        let engine = PermissionEngine::new(
            PermissionMode::Ask,
            vec![
                rule(RuleEffect::Allow, "command(*)"),
                rule(RuleEffect::Ask, "command(*)"),
                rule(RuleEffect::Deny, "command(rm -rf *)"),
            ],
        );
        let decision = engine.evaluate(&PermissionAction::Command("rm -rf /"));
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
    }

    #[test]
    fn ask_beats_allow_when_no_deny_matches() {
        let engine = PermissionEngine::new(
            PermissionMode::Ask,
            vec![
                rule(RuleEffect::Allow, "command(*)"),
                rule(RuleEffect::Ask, "command(*)"),
            ],
        );
        let decision = engine.evaluate(&PermissionAction::Command("ls"));
        assert!(matches!(decision, PermissionDecision::Ask { .. }));
    }

    #[test]
    fn read_only_mode_denies_writes_with_no_matching_rule() {
        let engine = PermissionEngine::new(PermissionMode::ReadOnly, Vec::new());
        let decision = engine.evaluate(&PermissionAction::WriteFile(Path::new("a.txt")));
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
        let read_decision = engine.evaluate(&PermissionAction::ReadFile(Path::new("a.txt")));
        assert_eq!(read_decision, PermissionDecision::Allow);
    }

    #[test]
    fn ask_mode_allows_reads_and_asks_for_writes_by_default() {
        let engine = PermissionEngine::new(PermissionMode::Ask, Vec::new());
        assert_eq!(
            engine.evaluate(&PermissionAction::ReadFile(Path::new("a.txt"))),
            PermissionDecision::Allow
        );
        assert!(matches!(
            engine.evaluate(&PermissionAction::WriteFile(Path::new("a.txt"))),
            PermissionDecision::Ask { .. }
        ));
    }

    #[test]
    fn full_auto_allows_everything() {
        let engine = PermissionEngine::new(PermissionMode::FullAuto, Vec::new());
        assert_eq!(
            engine.evaluate(&PermissionAction::Unsandboxed),
            PermissionDecision::Allow
        );
    }

    #[test]
    fn glob_target_matches_path() {
        let engine = PermissionEngine::new(
            PermissionMode::Ask,
            vec![rule(RuleEffect::Allow, "write_file(*.md)")],
        );
        assert_eq!(
            engine.evaluate(&PermissionAction::WriteFile(Path::new("README.md"))),
            PermissionDecision::Allow
        );
        assert!(matches!(
            engine.evaluate(&PermissionAction::WriteFile(Path::new("main.rs"))),
            PermissionDecision::Ask { .. }
        ));
    }

    #[test]
    fn regex_target_prefix_is_supported() {
        let engine = PermissionEngine::new(
            PermissionMode::Ask,
            vec![rule(RuleEffect::Deny, "command(regex:^rm\\s)")],
        );
        assert!(matches!(
            engine.evaluate(&PermissionAction::Command("rm -rf /")),
            PermissionDecision::Deny { .. }
        ));
        // No rule matches "ls -la"; falls through to the mode default (`Ask` mode asks for any
        // non-read-only action with no explicit rule, per `mode_default`).
        assert!(matches!(
            engine.evaluate(&PermissionAction::Command("ls -la")),
            PermissionDecision::Ask { .. }
        ));
    }

    #[tokio::test]
    async fn static_gate_auto_allow_resolves_ask_as_ok() {
        let engine = PermissionEngine::new(PermissionMode::Ask, Vec::new());
        let gate = StaticPermissionGate::new(engine, AskPolicy::AutoAllow);
        assert!(
            gate.check(PermissionAction::WriteFile(Path::new("a.txt")))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn static_gate_auto_deny_resolves_ask_as_err() {
        let engine = PermissionEngine::new(PermissionMode::Ask, Vec::new());
        let gate = StaticPermissionGate::new(engine, AskPolicy::AutoDeny);
        assert!(
            gate.check(PermissionAction::WriteFile(Path::new("a.txt")))
                .await
                .is_err()
        );
    }
}
