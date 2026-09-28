// SPDX-License-Identifier: GPL-3.0-only

//! Command palette (`core.mcp`-adjacent autocomplete UX, docs/PLAN.md §18.2): only shows commands
//! applicable to the active provider/transport (docs/commands.md §1.7 resolution order). Phase 1
//! only has core commands (no `FeaturePack` yet, CODEBASE.md §2), so filtering is a plain
//! alias/summary substring match against `RuntimeHandle::commands().core_commands()`.
//!
//! `entries` is built by the caller (`App`, which holds the `RuntimeHandle`) as owned
//! `(alias, summary)` pairs rather than this view depending on `xlightcli_provider::CommandDefinition`
//! directly — `tui` doesn't depend on `xlightcli-provider` (CODEBASE.md §3).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::theme::{Theme, ThemeRole};
use crate::view::permission_dialog::centered_rect;

#[derive(Debug, Clone, Default)]
pub struct CommandPaletteView {
    pub query: String,
    pub selected: usize,
    /// Indices into the `entries` slice passed to `recompute_matches`, kept as a separate list so
    /// arrow-key navigation only moves between filtered results.
    pub matches: Vec<usize>,
}

impl CommandPaletteView {
    /// Recomputes `matches` against `entries` (`(alias, summary)` pairs). Matching against the
    /// empty query shows every entry. Clamps `selected` back into range if the new match set is
    /// shorter than the old selection.
    pub fn recompute_matches(&mut self, entries: &[(String, String)]) {
        let query = self.query.to_ascii_lowercase();
        self.matches = entries
            .iter()
            .enumerate()
            .filter(|(_, (alias, summary))| {
                query.is_empty()
                    || alias.to_ascii_lowercase().contains(&query)
                    || summary.to_ascii_lowercase().contains(&query)
            })
            .map(|(i, _)| i)
            .collect();
        if self.selected >= self.matches.len() {
            self.selected = self.matches.len().saturating_sub(1);
        }
    }

    pub fn select_next(&mut self) {
        if !self.matches.is_empty() {
            self.selected = (self.selected + 1) % self.matches.len();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.matches.is_empty() {
            self.selected = (self.selected + self.matches.len() - 1) % self.matches.len();
        }
    }

    /// The alias of the currently-selected match, if any (`entries` must be the same slice last
    /// passed to `recompute_matches`).
    pub fn selected_alias<'a>(&self, entries: &'a [(String, String)]) -> Option<&'a str> {
        self.matches
            .get(self.selected)
            .and_then(|&i| entries.get(i))
            .map(|(alias, _)| alias.as_str())
    }

    pub fn render(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        entries: &[(String, String)],
    ) {
        let popup = centered_rect(60, 60, area);
        frame.render_widget(Clear, popup);

        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!("Commands  /{}", self.query))
            .style(theme.style(ThemeRole::Foreground));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let items: Vec<ListItem<'_>> = self
            .matches
            .iter()
            .filter_map(|&i| entries.get(i))
            .map(|(alias, summary)| {
                ListItem::new(Line::from(vec![
                    Span::styled(format!("/{alias}"), theme.style(ThemeRole::Accent)),
                    Span::raw("  "),
                    Span::styled(summary.clone(), theme.style(ThemeRole::Muted)),
                ]))
            })
            .collect();

        if items.is_empty() {
            frame.render_widget(Paragraph::new("(no matching command)"), inner);
            return;
        }

        let list = List::new(items)
            .highlight_style(theme.style(ThemeRole::Accent))
            .highlight_symbol("> ");
        let mut state = ListState::default();
        state.select(Some(self.selected));
        frame.render_stateful_widget(list, inner, &mut state);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    fn entries() -> Vec<(String, String)> {
        vec![
            ("help".to_string(), "Show help".to_string()),
            ("resume".to_string(), "Pick a session".to_string()),
            ("model".to_string(), "Pick a model".to_string()),
        ]
    }

    #[test]
    fn empty_query_matches_everything() {
        let mut view = CommandPaletteView::default();
        view.recompute_matches(&entries());
        assert_eq!(view.matches.len(), 3);
    }

    #[test]
    fn query_filters_by_alias_or_summary() {
        let mut view = CommandPaletteView {
            query: "mod".to_string(),
            ..Default::default()
        };
        view.recompute_matches(&entries());
        assert_eq!(view.selected_alias(&entries()), Some("model"));
    }

    #[test]
    fn selection_clamped_when_matches_shrink() {
        let mut view = CommandPaletteView::default();
        view.recompute_matches(&entries());
        view.selected = 2;
        view.query = "help".to_string();
        view.recompute_matches(&entries());
        assert_eq!(view.selected, 0);
    }
}
