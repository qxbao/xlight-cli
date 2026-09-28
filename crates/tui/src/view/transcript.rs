// SPDX-License-Identifier: GPL-3.0-only

//! Transcript view (docs/PLAN.md §18.2): the scrollable message history for the active agent.
//! Populated from `UiEvent::TextDelta`/`ReasoningDelta`/`ToolCallStarted`/`ToolCallFinished`
//! (coalescing deltas per PATTERNS.md §3 — a running delta is appended to the last matching line
//! instead of pushing a new one every time). Reasoning blocks are collapsible (`toggle_last_reasoning`);
//! tool calls render as a card: name, the spooled `text_preview`, and the artifact path if any.

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
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
    /// Number of wrapped terminal rows above the tail currently being viewed. `0` means follow
    /// the tail automatically; a positive value means the user has manually scrolled upward.
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
        self.scroll_offset = self.scroll_offset.saturating_add(amount);
    }

    pub fn scroll_down(&mut self, amount: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
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
                    out.extend(render_markdown(&line.text, theme));
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
        let inner = block.inner(area);
        // `Paragraph::line_count` includes a configured block's borders in its result, while
        // scrolling is applied inside the block. Measure the unblocked text against the actual
        // inner width to get the exact same WordWrapper height used by rendering.
        let wrapped_height = Paragraph::new(text.clone())
            .wrap(Wrap { trim: false })
            .line_count(inner.width);
        let paragraph = Paragraph::new(text)
            .block(block)
            .wrap(Wrap { trim: false })
            .style(Style::default());
        // Ratatui's paragraph scroll is measured from the top, while our state is measured from
        // the tail so newly streamed output remains visible until the user scrolls up.
        let max_scroll = wrapped_height.saturating_sub(usize::from(inner.height));
        let scroll_from_top = max_scroll.saturating_sub(usize::from(self.scroll_offset));
        let paragraph = paragraph.scroll((scroll_from_top.min(usize::from(u16::MAX)) as u16, 0));
        frame.render_widget(paragraph, area);
    }
}

/// A compact Markdown-to-ratatui renderer for assistant output. The transcript retains the
/// original Markdown string; only the terminal representation gains headings, emphasis, lists,
/// quotes, links, and fenced code styling.
struct MarkdownRenderer {
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    has_content: bool,
    style: Style,
    style_stack: Vec<Style>,
    list_depth: usize,
    quote_depth: usize,
    item_open: bool,
    in_code_block: bool,
    link_destinations: Vec<String>,
    theme: Theme,
}

impl MarkdownRenderer {
    fn new(theme: &Theme) -> Self {
        Self {
            lines: Vec::new(),
            current: Vec::new(),
            has_content: false,
            style: Style::default(),
            style_stack: Vec::new(),
            list_depth: 0,
            quote_depth: 0,
            item_open: false,
            in_code_block: false,
            link_destinations: Vec::new(),
            theme: theme.clone(),
        }
    }

    fn ensure_prefix(&mut self) {
        if self.current.is_empty() {
            self.current.push(Span::raw("  "));
            if self.quote_depth > 0 {
                self.current.push(Span::styled(
                    format!("{}│ ", "  ".repeat(self.quote_depth.saturating_sub(1))),
                    self.theme.style(ThemeRole::Muted),
                ));
            }
        }
    }

    fn flush(&mut self) {
        if self.has_content {
            self.lines
                .push(Line::from(std::mem::take(&mut self.current)));
            self.has_content = false;
        } else {
            self.current.clear();
        }
    }

    fn push_text(&mut self, text: &str, style: Style) {
        for (index, segment) in text.split('\n').enumerate() {
            if index > 0 {
                self.flush();
            }
            if !segment.is_empty() {
                self.ensure_prefix();
                self.current.push(Span::styled(segment.to_string(), style));
                self.has_content = true;
            }
        }
    }

    fn start_style(&mut self, style: Style) {
        self.style_stack.push(self.style);
        self.style = style;
    }

    fn end_style(&mut self) {
        if let Some(style) = self.style_stack.pop() {
            self.style = style;
        }
    }

    fn start_item(&mut self) {
        self.flush();
        self.ensure_prefix();
        self.current
            .push(Span::raw("  ".repeat(self.list_depth.saturating_sub(1))));
        self.current
            .push(Span::styled("• ", self.theme.style(ThemeRole::Accent)));
        self.has_content = true;
        self.item_open = true;
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush();
        if self.lines.is_empty() {
            self.lines.push(Line::from("  "));
        }
        self.lines
    }
}

fn render_markdown(markdown: &str, theme: &Theme) -> Vec<Line<'static>> {
    let mut renderer = MarkdownRenderer::new(theme);
    for event in Parser::new(markdown) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    renderer.flush();
                    let style = match level {
                        HeadingLevel::H1 | HeadingLevel::H2 => renderer
                            .theme
                            .style(ThemeRole::Accent)
                            .add_modifier(Modifier::BOLD),
                        _ => renderer.style.add_modifier(Modifier::BOLD),
                    };
                    renderer.start_style(style);
                }
                Tag::Emphasis => {
                    renderer.start_style(renderer.style.add_modifier(Modifier::ITALIC))
                }
                Tag::Strong => renderer.start_style(renderer.style.add_modifier(Modifier::BOLD)),
                Tag::Strikethrough => {
                    renderer.start_style(renderer.style.add_modifier(Modifier::CROSSED_OUT));
                }
                Tag::CodeBlock(_) => {
                    renderer.flush();
                    renderer.in_code_block = true;
                    renderer.start_style(renderer.theme.style(ThemeRole::Muted));
                }
                Tag::List(_) => renderer.list_depth += 1,
                Tag::Item => renderer.start_item(),
                Tag::BlockQuote(_) => {
                    renderer.flush();
                    renderer.quote_depth += 1;
                }
                Tag::Link { dest_url, .. } => {
                    renderer.link_destinations.push(dest_url.to_string());
                    renderer.start_style(
                        renderer
                            .theme
                            .style(ThemeRole::Accent)
                            .add_modifier(Modifier::UNDERLINED),
                    );
                }
                Tag::Image { dest_url, .. } => {
                    renderer.link_destinations.push(dest_url.to_string());
                    renderer.start_style(renderer.theme.style(ThemeRole::Muted));
                    renderer.push_text("[image: ", renderer.style);
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Heading(_) | TagEnd::Paragraph => renderer.flush(),
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => renderer.end_style(),
                TagEnd::CodeBlock => {
                    renderer.flush();
                    renderer.in_code_block = false;
                    renderer.end_style();
                }
                TagEnd::List(_) => {
                    renderer.flush();
                    renderer.list_depth = renderer.list_depth.saturating_sub(1);
                }
                TagEnd::Item => {
                    renderer.flush();
                    renderer.item_open = false;
                }
                TagEnd::BlockQuote(_) => {
                    renderer.flush();
                    renderer.quote_depth = renderer.quote_depth.saturating_sub(1);
                }
                TagEnd::Link => {
                    renderer.end_style();
                    if let Some(destination) = renderer.link_destinations.pop() {
                        renderer.push_text(
                            &format!(" ({destination})"),
                            renderer.theme.style(ThemeRole::Muted),
                        );
                    }
                }
                TagEnd::Image => {
                    renderer.push_text("]", renderer.style);
                    renderer.end_style();
                    if let Some(destination) = renderer.link_destinations.pop() {
                        renderer.push_text(
                            &format!(" ({destination})"),
                            renderer.theme.style(ThemeRole::Muted),
                        );
                    }
                }
                _ => {}
            },
            Event::Text(text) => {
                if renderer.in_code_block {
                    renderer.push_text(&format!("    {text}"), renderer.style);
                } else {
                    renderer.push_text(&text, renderer.style);
                }
            }
            Event::Code(code) => renderer.push_text(
                &format!("`{code}`"),
                renderer.theme.style(ThemeRole::Accent),
            ),
            Event::SoftBreak | Event::HardBreak => renderer.flush(),
            Event::Rule => {
                renderer.flush();
                renderer.push_text("────────────────", renderer.theme.style(ThemeRole::Muted));
                renderer.flush();
            }
            Event::TaskListMarker(done) => renderer.push_text(
                if done { "[x] " } else { "[ ] " },
                renderer.theme.style(ThemeRole::Accent),
            ),
            Event::Html(html) | Event::InlineHtml(html) => {
                renderer.push_text(&html, renderer.style)
            }
            _ => {}
        }
    }
    renderer.finish()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
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

    #[test]
    fn scrolling_uses_distance_from_tail_and_can_return_to_follow_mode() {
        let mut view = TranscriptView::new();
        view.push_notice("one");
        view.push_notice("two");
        assert_eq!(view.scroll_offset, 0);
        view.scroll_up(10);
        assert_eq!(view.scroll_offset, 10);
        view.scroll_down(4);
        assert_eq!(view.scroll_offset, 6);
        view.scroll_down(99);
        assert_eq!(view.scroll_offset, 0);
    }

    #[test]
    fn render_at_tail_keeps_the_last_line_visible_and_scroll_up_reveals_history() {
        let mut view = TranscriptView::new();
        for index in 0..20 {
            view.push_notice(format!("line-{index}"));
        }
        let theme = Theme::default();
        let backend = TestBackend::new(30, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| view.render(frame, frame.area(), &theme))
            .unwrap();
        let tail = terminal.backend().buffer().content();
        assert!(tail.iter().any(|cell| cell.symbol() == "l"));
        let rendered_tail = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered_tail.contains("line-19"));

        view.scroll_up(20);
        terminal
            .draw(|frame| view.render(frame, frame.area(), &theme))
            .unwrap();
        let rendered_head = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered_head.contains("line-0"));
    }

    #[test]
    fn render_at_tail_keeps_a_wrapped_tool_preview_end_visible() {
        let mut view = TranscriptView::new();
        view.push_notice("before preview");
        let agent = AgentId::new();
        let call_id = ToolCallId::new("long-preview");
        view.start_tool_call(agent, call_id.clone(), "read_file");
        view.finish_tool_call(
            agent,
            &call_id,
            &ToolCallSummary {
                name: "read_file".to_string(),
                text_preview: format!("{}TAIL-PREVIEW", "x".repeat(250)),
                artifact: None,
            },
        );
        let theme = Theme::default();
        let backend = TestBackend::new(30, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| view.render(frame, frame.area(), &theme))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        // The terminal may wrap the marker across rows, but its final suffix must still render.
        assert!(rendered.contains("IL-PREVIEW"));
    }

    #[test]
    fn markdown_renderer_removes_markup_and_preserves_structure() {
        let lines = render_markdown(
            "# Title\n\nA **bold** `code` [link](https://example.test).\n\n- first\n- second\n\n```rust\nlet x = 1;\n```",
            &Theme::default(),
        );
        let rendered = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(rendered.contains("Title"));
        assert!(rendered.contains("bold"));
        assert!(rendered.contains("`code`"));
        assert!(rendered.contains("• first"));
        assert!(rendered.contains("https://example.test"));
        assert!(rendered.contains("let x = 1;"));
        assert!(!rendered.contains("**bold**"));
    }
}
