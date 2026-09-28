// SPDX-License-Identifier: GPL-3.0-only

//! Transcript view (docs/PLAN.md §18.2): the scrollable message history for the active agent.
//! Populated from `UiEvent::TextDelta`/`ReasoningDelta`/`ToolCallStarted`/`ToolCallFinished`
//! (coalescing deltas per PATTERNS.md §3 — a running delta is appended to the last matching line
//! instead of pushing a new one every time). Reasoning blocks are collapsible (`toggle_last_reasoning`);
//! tool calls render as a card: name, the spooled `text_preview`, and the artifact path if any.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use xlightcli_protocol::{AgentId, ToolCallId};
use xlightcli_runtime::{NoticeLevel, ToolCallSummary};

use crate::theme::{Theme, ThemeRole};

/// What kind of transcript entry a [`TranscriptLine`] is. Kept flat (as opposed to a richer
/// per-kind struct) so `TranscriptView`'s coalescing logic can match on it directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptRole {
    User,
    Assistant,
    Reasoning,
    ToolCall,
    Notice,
    Error,
}

/// One rendered entry of the transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptLine {
    pub role: TranscriptRole,
    pub text: String,
    pub agent_id: Option<AgentId>,
    pub tool_call_id: Option<ToolCallId>,
    pub artifact_path: Option<String>,
    pub finished: bool,
    /// Reasoning-only: whether the block is rendered collapsed (just a one-line summary).
    pub collapsed: bool,
}

impl TranscriptLine {
    fn new(role: TranscriptRole, text: impl Into<String>) -> Self {
        Self {
            role,
            text: text.into(),
            agent_id: None,
            tool_call_id: None,
            artifact_path: None,
            finished: true,
            collapsed: false,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TranscriptView {
    pub lines: Vec<TranscriptLine>,
    pub scroll_offset: u16,
}

impl TranscriptView {
    pub fn new() -> Self {
        Self::default()
    }

    /// Back-compat convenience used by tests: pushes a plain notice line.
    pub fn push_line(&mut self, text: impl Into<String>) {
        self.push_notice(text);
    }

    pub fn push_user(&mut self, text: impl Into<String>) {
        self.lines
            .push(TranscriptLine::new(TranscriptRole::User, text));
    }

    pub fn push_notice(&mut self, text: impl Into<String>) {
        self.lines
            .push(TranscriptLine::new(TranscriptRole::Notice, text));
    }

    pub fn push_notice_leveled(&mut self, level: NoticeLevel, text: impl Into<String>) {
        let role = match level {
            NoticeLevel::Error => TranscriptRole::Error,
            _ => TranscriptRole::Notice,
        };
        self.lines.push(TranscriptLine::new(role, text));
    }

    pub fn push_error(&mut self, text: impl Into<String>) {
        self.lines
            .push(TranscriptLine::new(TranscriptRole::Error, text));
    }

    /// Appends a `TextDelta`: coalesced into the last `Assistant` line for `agent_id` if that's
    /// still the last line (PATTERNS.md §3), otherwise starts a new one.
    pub fn push_assistant_delta(&mut self, agent_id: AgentId, text: &str) {
        if let Some(last) = self.lines.last_mut()
            && last.role == TranscriptRole::Assistant
            && last.agent_id == Some(agent_id)
        {
            last.text.push_str(text);
            return;
        }
        let mut line = TranscriptLine::new(TranscriptRole::Assistant, text);
        line.agent_id = Some(agent_id);
        self.lines.push(line);
    }

    /// Appends a `ReasoningDelta`: same coalescing rule as assistant text, new blocks start
    /// expanded (`collapsed: false`).
    pub fn push_reasoning_delta(&mut self, agent_id: AgentId, text: &str) {
        if let Some(last) = self.lines.last_mut()
            && last.role == TranscriptRole::Reasoning
            && last.agent_id == Some(agent_id)
        {
            last.text.push_str(text);
            return;
        }
        let mut line = TranscriptLine::new(TranscriptRole::Reasoning, text);
        line.agent_id = Some(agent_id);
        self.lines.push(line);
    }

    pub fn start_tool_call(&mut self, agent_id: AgentId, call_id: ToolCallId, name: &str) {
        let mut line = TranscriptLine::new(TranscriptRole::ToolCall, name);
        line.agent_id = Some(agent_id);
        line.tool_call_id = Some(call_id);
        line.finished = false;
        self.lines.push(line);
    }

    /// Fills in the card started by `start_tool_call` for the same `call_id`, or — if the
    /// `ToolCallStarted` event was missed — appends a fresh, already-finished card.
    pub fn finish_tool_call(
        &mut self,
        agent_id: AgentId,
        call_id: &ToolCallId,
        summary: &ToolCallSummary,
    ) {
        if let Some(line) = self.lines.iter_mut().rev().find(|l| {
            l.role == TranscriptRole::ToolCall && l.tool_call_id.as_ref() == Some(call_id)
        }) {
            line.text = format!("{}: {}", summary.name, summary.text_preview);
            line.artifact_path = summary
                .artifact
                .as_ref()
                .map(|a| a.path.display().to_string());
            line.finished = true;
            return;
        }
        let mut line = TranscriptLine::new(
            TranscriptRole::ToolCall,
            format!("{}: {}", summary.name, summary.text_preview),
        );
        line.agent_id = Some(agent_id);
        line.tool_call_id = Some(call_id.clone());
        line.artifact_path = summary
            .artifact
            .as_ref()
            .map(|a| a.path.display().to_string());
        line.finished = true;
        self.lines.push(line);
    }

    /// Toggles the collapsed state of the most recent reasoning block, if any (`ctrl+r`, bound in
    /// `App::on_key`). Returns whether a block was found and toggled.
    pub fn toggle_last_reasoning(&mut self) -> bool {
        if let Some(line) = self
            .lines
            .iter_mut()
            .rev()
            .find(|l| l.role == TranscriptRole::Reasoning)
        {
            line.collapsed = !line.collapsed;
            true
        } else {
            false
        }
    }

    pub fn scroll_up(&mut self, amount: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
    }

    pub fn scroll_down(&mut self, amount: u16) {
        // Rendering clamps this to the actual content height; unbounded here (no frame size to
        // clamp against without threading `Rect` through every event handler).
        self.scroll_offset = self.scroll_offset.saturating_add(amount);
    }

    /// Renders every line into styled `ratatui::text::Line`s (one transcript line may still wrap
    /// to several terminal rows — `Paragraph`'s job, not this function's).
    fn to_text(&self, theme: &Theme) -> Text<'static> {
        let mut out = Vec::with_capacity(self.lines.len());
        for line in &self.lines {
            match line.role {
                TranscriptRole::User => {
                    out.push(Line::from(vec![
                        Span::styled("> ", theme.style(ThemeRole::Accent)),
                        Span::raw(line.text.clone()),
                    ]));
                }
                TranscriptRole::Assistant => {
                    for segment in line.text.split('\n') {
                        out.push(Line::from(format!("  {segment}")));
                    }
                }
                TranscriptRole::Reasoning => {
                    let style = theme.style(ThemeRole::Muted);
                    if line.collapsed {
                        let summary: String = line.text.chars().take(60).collect();
                        out.push(Line::from(Span::styled(
                            format!("  \u{25b8} reasoning (collapsed): {summary}..."),
                            style,
                        )));
                    } else {
                        out.push(Line::from(Span::styled("  \u{25be} reasoning:", style)));
                        for segment in line.text.split('\n') {
                            out.push(Line::from(Span::styled(format!("    {segment}"), style)));
                        }
                    }
                }
                TranscriptRole::ToolCall => {
                    let marker = if line.finished {
                        "\u{2713}"
                    } else {
                        "\u{25cb}"
                    };
                    let style = theme.style(ThemeRole::Accent);
                    out.push(Line::from(Span::styled(
                        format!("  [{marker}] {}", line.text),
                        style,
                    )));
                    if let Some(artifact) = &line.artifact_path {
                        out.push(Line::from(Span::styled(
                            format!("      artifact: {artifact}"),
                            theme.style(ThemeRole::Muted),
                        )));
                    }
                }
                TranscriptRole::Notice => {
                    out.push(Line::from(Span::styled(
                        format!("  \u{2022} {}", line.text),
                        theme.style(ThemeRole::Muted),
                    )));
                }
                TranscriptRole::Error => {
                    out.push(Line::from(Span::styled(
                        format!("  ! {}", line.text),
                        theme.style(ThemeRole::Danger),
                    )));
                }
            }
        }
        Text::from(out)
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Transcript")
            .style(theme.style(ThemeRole::Foreground));
        let text = self.to_text(theme);
        let paragraph = Paragraph::new(text)
            .block(block)
            .wrap(Wrap { trim: false })
            .style(Style::default())
            .scroll((self.scroll_offset, 0));
        frame.render_widget(paragraph, area);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;
    use xlightcli_protocol::ToolCallId;

    use super::*;

    #[test]
    fn assistant_deltas_from_the_same_agent_coalesce() {
        let mut view = TranscriptView::new();
        let agent = AgentId::new();
        view.push_assistant_delta(agent, "Hel");
        view.push_assistant_delta(agent, "lo");
        assert_eq!(view.lines.len(), 1);
        assert_eq!(view.lines[0].text, "Hello");
    }

    #[test]
    fn assistant_deltas_from_a_different_agent_start_a_new_line() {
        let mut view = TranscriptView::new();
        view.push_assistant_delta(AgentId::new(), "a");
        view.push_assistant_delta(AgentId::new(), "b");
        assert_eq!(view.lines.len(), 2);
    }

    #[test]
    fn tool_call_started_then_finished_updates_the_same_line() {
        let mut view = TranscriptView::new();
        let agent = AgentId::new();
        let call_id = ToolCallId::new("call-1");
        view.start_tool_call(agent, call_id.clone(), "read_file");
        assert_eq!(view.lines.len(), 1);
        assert!(!view.lines[0].finished);

        view.finish_tool_call(
            agent,
            &call_id,
            &ToolCallSummary {
                name: "read_file".to_string(),
                text_preview: "42 lines".to_string(),
                artifact: None,
            },
        );
        assert_eq!(view.lines.len(), 1);
        assert!(view.lines[0].finished);
        assert!(view.lines[0].text.contains("42 lines"));
    }

    #[test]
    fn toggle_last_reasoning_flips_collapsed() {
        let mut view = TranscriptView::new();
        let agent = AgentId::new();
        view.push_reasoning_delta(agent, "thinking...");
        assert!(!view.lines[0].collapsed);
        assert!(view.toggle_last_reasoning());
        assert!(view.lines[0].collapsed);
    }

    #[test]
    fn toggle_last_reasoning_is_false_when_none_exists() {
        let mut view = TranscriptView::new();
        assert!(!view.toggle_last_reasoning());
    }
}
