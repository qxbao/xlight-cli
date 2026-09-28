// SPDX-License-Identifier: GPL-3.0-only

//! Status line (docs/PLAN.md §18.2 example: `~/repo · Codex/chatgpt · gpt-… · 3 agents (2
//! running) · ctx 41% · ask`). Also the target of `core.statusline` (a user script, Phase 6).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use xlightcli_protocol::{ModelId, ProviderId, RateLimitInfo, TransportId, Usage};
use xlightcli_runtime::{ExecutionMode, PermissionMode};

use crate::theme::{Theme, ThemeRole};

/// The data a status line needs. Also carries the two pieces of per-turn telemetry the Wave B
/// brief calls out that the Wave A shape didn't have room for yet: the latest `Usage` snapshot and
/// the latest `RateLimitInfo` (both `None` until the first turn reports one).
#[derive(Debug, Clone)]
pub struct StatusLineView {
    pub workspace_label: String,
    pub provider: ProviderId,
    pub transport: TransportId,
    pub model: ModelId,
    pub agents_total: u32,
    pub agents_running: u32,
    pub context_used_percent: u8,
    pub permission_mode: PermissionMode,
    pub execution_mode: ExecutionMode,
    pub last_usage: Option<Usage>,
    pub last_rate_limit: Option<RateLimitInfo>,
}

impl Default for StatusLineView {
    /// A placeholder shown before any session/turn has told us anything real (App::new): every
    /// field renders as `-`/`0` rather than leaving the whole status line blank.
    fn default() -> Self {
        Self {
            workspace_label: "-".to_string(),
            provider: ProviderId::new("-"),
            transport: TransportId::new("-"),
            model: ModelId::new("-"),
            agents_total: 0,
            agents_running: 0,
            context_used_percent: 0,
            permission_mode: PermissionMode::default(),
            execution_mode: ExecutionMode::default(),
            last_usage: None,
            last_rate_limit: None,
        }
    }
}

fn mode_label(mode: ExecutionMode) -> &'static str {
    match mode {
        ExecutionMode::Default => "default",
        ExecutionMode::AcceptEdits => "accept-edits",
        ExecutionMode::Plan => "plan",
    }
}

fn permission_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::ReadOnly => "read-only",
        PermissionMode::Strict => "strict",
        PermissionMode::Ask => "ask",
        PermissionMode::AutoEdit => "auto-edit",
        PermissionMode::FullAuto => "full-auto",
    }
}

impl StatusLineView {
    pub fn to_line(&self) -> String {
        let agents = format!(
            "{} agents ({} running)",
            self.agents_total, self.agents_running
        );
        let usage = self
            .last_usage
            .map(|u| format!(" · tok {}in/{}out", u.input_tokens, u.output_tokens))
            .unwrap_or_default();
        let rate_limit = self
            .last_rate_limit
            .as_ref()
            .and_then(|r| r.remaining)
            .map(|remaining| format!(" · rl {remaining}"))
            .unwrap_or_default();
        format!(
            "{} · {}/{} · {} · {} · ctx {}% · {} · {}{usage}{rate_limit}",
            self.workspace_label,
            self.provider,
            self.transport,
            self.model,
            agents,
            self.context_used_percent,
            mode_label(self.execution_mode),
            permission_label(self.permission_mode),
        )
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let line = Line::from(Span::styled(self.to_line(), theme.style(ThemeRole::Accent)));
        frame.render_widget(Paragraph::new(line), area);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn default_status_line_renders_placeholders() {
        let view = StatusLineView::default();
        let line = view.to_line();
        assert!(line.contains("- · -/- · - · 0 agents (0 running) · ctx 0% · default · ask"));
    }

    #[test]
    fn usage_and_rate_limit_are_appended_when_present() {
        let view = StatusLineView {
            last_usage: Some(Usage {
                input_tokens: 10,
                output_tokens: 5,
                ..Default::default()
            }),
            last_rate_limit: Some(RateLimitInfo {
                remaining: Some(42),
                ..Default::default()
            }),
            ..Default::default()
        };
        let line = view.to_line();
        assert!(line.contains("tok 10in/5out"));
        assert!(line.contains("rl 42"));
    }
}
