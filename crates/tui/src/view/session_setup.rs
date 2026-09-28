// SPDX-License-Identifier: GPL-3.0-only

//! Initial provider, transport, and model picker. The runtime supplies canonical choices; this
//! view only owns keyboard selection and rendering, so no credential reaches the TUI.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use xlightcli_protocol::{ProviderId, TransportId};

use crate::theme::{Theme, ThemeRole};
use crate::view::centered_rect;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupStage {
    Provider,
    Transport {
        provider: ProviderId,
    },
    Model {
        provider: ProviderId,
        transport: TransportId,
    },
}

#[derive(Debug, Clone)]
pub struct SessionSetupView {
    pub stage: SetupStage,
    pub options: Vec<(String, String)>,
    pub selected: usize,
    /// Typed model ID; catalog rows are suggestions, not an allowlist.
    pub model_id_input: String,
}

impl SessionSetupView {
    pub fn new(stage: SetupStage, options: Vec<(String, String)>) -> Self {
        Self {
            stage,
            options,
            selected: 0,
            model_id_input: String::new(),
        }
    }

    pub fn select_next(&mut self) {
        if !self.options.is_empty() {
            self.selected = (self.selected + 1) % self.options.len();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.options.is_empty() {
            self.selected = (self.selected + self.options.len() - 1) % self.options.len();
        }
    }

    pub fn selected_id(&self) -> Option<&str> {
        self.options.get(self.selected).map(|(id, _)| id.as_str())
    }

    pub fn typed_model_id(&self) -> Option<&str> {
        let value = self.model_id_input.trim();
        (!value.is_empty() && !value.chars().any(char::is_whitespace)).then_some(value)
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let popup = centered_rect(70, 60, area);
        frame.render_widget(Clear, popup);
        let title = match self.stage {
            SetupStage::Provider => "New session: choose provider",
            SetupStage::Transport { .. } => "New session: choose transport",
            SetupStage::Model { .. } => "New session: choose model",
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .style(theme.style(ThemeRole::Foreground));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);
        let (list_area, input_area) = if matches!(self.stage, SetupStage::Model { .. }) {
            let areas = ratatui::layout::Layout::vertical([
                ratatui::layout::Constraint::Min(1),
                ratatui::layout::Constraint::Length(2),
            ])
            .split(inner);
            (areas[0], Some(areas[1]))
        } else {
            (inner, None)
        };
        if let Some(input_area) = input_area {
            let prompt = if self.model_id_input.is_empty() {
                "Type model ID, or select a suggestion above".to_string()
            } else {
                format!("Model ID: {}", self.model_id_input)
            };
            frame.render_widget(
                Paragraph::new(prompt).style(theme.style(ThemeRole::Accent)),
                input_area,
            );
        }
        let items: Vec<ListItem<'_>> = self
            .options
            .iter()
            .map(|(id, label)| {
                ListItem::new(Line::from(vec![
                    Span::styled(id.clone(), theme.style(ThemeRole::Accent)),
                    Span::raw("  "),
                    Span::styled(label.clone(), theme.style(ThemeRole::Muted)),
                ]))
            })
            .collect();
        if items.is_empty() {
            frame.render_widget(
                Paragraph::new("No catalog available; type a model ID below"),
                list_area,
            );
            return;
        }
        let list = List::new(items)
            .highlight_style(theme.style(ThemeRole::Accent))
            .highlight_symbol("> ");
        let mut state = ListState::default();
        state.select(Some(self.selected));
        frame.render_stateful_widget(list, list_area, &mut state);
    }
}
