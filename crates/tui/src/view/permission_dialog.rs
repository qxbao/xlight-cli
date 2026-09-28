// SPDX-License-Identifier: GPL-3.0-only

//! Permission dialog (docs/PLAN.md §7.3): shown on `UiEvent::PermissionRequested`, answered via
//! `RuntimeHandle::respond_to_permission`. Keyboard driven: Up/Down (or `j`/`k`) move the
//! selection among the three choices, Enter confirms, `a`/`A`/`d`/`Esc` are direct shortcuts.
//!
//! **Known gap (Wave B):** `xlightcli_runtime::PermissionResponse` only has `Allow`/`Deny` today
//! — there is no "always allow this rule" variant, and no `RuntimeHandle` call to persist a new
//! `PermissionRule` from the TUI. "Always allow" therefore answers this one request with `Allow`
//! and surfaces a notice that the rule itself wasn't persisted, rather than silently pretending it
//! was (INV-10) — wiring a real persist path is follow-up work once `RuntimeHandle` grows one.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use xlightcli_protocol::ToolCallId;
use xlightcli_runtime::PermissionRequest;

use crate::theme::{Theme, ThemeRole};

/// The three choices offered for a permission request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionChoice {
    AllowOnce,
    AlwaysAllow,
    Deny,
}

pub const PERMISSION_CHOICES: [PermissionChoice; 3] = [
    PermissionChoice::AllowOnce,
    PermissionChoice::AlwaysAllow,
    PermissionChoice::Deny,
];

impl PermissionChoice {
    pub fn label(self) -> &'static str {
        match self {
            Self::AllowOnce => "Allow once",
            Self::AlwaysAllow => "Always allow this rule",
            Self::Deny => "Deny",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PermissionDialogView {
    pub tool_call_id: ToolCallId,
    pub request: PermissionRequest,
    pub selected: usize,
}

impl PermissionDialogView {
    pub fn new(tool_call_id: ToolCallId, request: PermissionRequest) -> Self {
        Self {
            tool_call_id,
            request,
            selected: 0,
        }
    }

    pub fn select_next(&mut self) {
        self.selected = (self.selected + 1) % PERMISSION_CHOICES.len();
    }

    pub fn select_prev(&mut self) {
        self.selected = (self.selected + PERMISSION_CHOICES.len() - 1) % PERMISSION_CHOICES.len();
    }

    pub fn selected_choice(&self) -> PermissionChoice {
        PERMISSION_CHOICES[self.selected]
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let popup = centered_rect(60, 40, area);
        frame.render_widget(Clear, popup);

        let block = Block::default()
            .borders(Borders::ALL)
            .title("Permission requested")
            .style(theme.style(ThemeRole::Warning));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(3)])
            .split(inner);

        let detail = Paragraph::new(format!(
            "{} {}\n{}",
            self.request.action, self.request.target, self.request.reason
        ))
        .wrap(Wrap { trim: true });
        frame.render_widget(detail, chunks[0]);

        let items: Vec<ListItem<'_>> = PERMISSION_CHOICES
            .iter()
            .map(|choice| ListItem::new(Line::from(Span::raw(choice.label()))))
            .collect();
        let list = List::new(items)
            .highlight_style(theme.style(ThemeRole::Accent))
            .highlight_symbol("> ");
        let mut state = ListState::default();
        state.select(Some(self.selected));
        frame.render_stateful_widget(list, chunks[1], &mut state);
    }
}

/// A centered `Rect` covering `percent_x`% width / `percent_y`% height of `area` — the standard
/// ratatui popup-centering recipe, used by every overlay in this crate.
pub fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    fn sample() -> PermissionDialogView {
        PermissionDialogView::new(
            ToolCallId::new("call-1"),
            PermissionRequest {
                action: "write_file".to_string(),
                target: "src/main.rs".to_string(),
                tool_call_id: None,
                reason: "no matching allow rule".to_string(),
            },
        )
    }

    #[test]
    fn selection_wraps_forward_and_back() {
        let mut view = sample();
        assert_eq!(view.selected_choice(), PermissionChoice::AllowOnce);
        view.select_prev();
        assert_eq!(view.selected_choice(), PermissionChoice::Deny);
        view.select_next();
        view.select_next();
        assert_eq!(view.selected_choice(), PermissionChoice::AlwaysAllow);
    }
}
