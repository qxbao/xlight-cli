// SPDX-License-Identifier: GPL-3.0-only

//! Diff view (`core.diff`, docs/commands.md §2; plan/diff artifact review, D-025). Renders
//! already-computed unified diff text (`tui` doesn't depend on `xlightcli-tools`/`similar`
//! directly, CODEBASE.md §3 — the diff text arrives as a string, e.g. a tool call's
//! `text_preview` or `core.diff`'s `CommandOutcome::Message`) split per file, with `+`/`-` lines
//! colored and a file list to navigate between hunks.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::theme::{Theme, ThemeRole};
use crate::view::permission_dialog::centered_rect;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    pub path: String,
    pub unified_text: String,
}

#[derive(Debug, Clone, Default)]
pub struct DiffView {
    pub hunks: Vec<DiffHunk>,
    pub selected: usize,
}

impl DiffView {
    pub fn new(hunks: Vec<DiffHunk>) -> Self {
        Self { hunks, selected: 0 }
    }

    pub fn select_next(&mut self) {
        if !self.hunks.is_empty() {
            self.selected = (self.selected + 1) % self.hunks.len();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.hunks.is_empty() {
            self.selected = (self.selected + self.hunks.len() - 1) % self.hunks.len();
        }
    }

    fn line_style(theme: &Theme, line: &str) -> Style {
        if line.starts_with('+') && !line.starts_with("+++") {
            theme.style(ThemeRole::Success)
        } else if line.starts_with('-') && !line.starts_with("---") {
            theme.style(ThemeRole::Danger)
        } else if line.starts_with("@@") {
            theme.style(ThemeRole::Accent)
        } else {
            theme.style(ThemeRole::Foreground)
        }
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let popup = centered_rect(90, 85, area);
        frame.render_widget(ratatui::widgets::Clear, popup);

        let outer = Block::default()
            .borders(Borders::ALL)
            .title("Diff")
            .style(theme.style(ThemeRole::Foreground));
        let inner = outer.inner(popup);
        frame.render_widget(outer, popup);

        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(25), Constraint::Percentage(75)])
            .split(inner);

        let items: Vec<ListItem<'_>> = self
            .hunks
            .iter()
            .map(|hunk| ListItem::new(Line::from(hunk.path.clone())))
            .collect();
        let list = List::new(items)
            .block(Block::default().borders(Borders::RIGHT))
            .highlight_style(theme.style(ThemeRole::Accent))
            .highlight_symbol("> ");
        let mut state = ListState::default();
        state.select(if self.hunks.is_empty() {
            None
        } else {
            Some(self.selected)
        });
        frame.render_stateful_widget(list, columns[0], &mut state);

        let body = self
            .hunks
            .get(self.selected)
            .map(|hunk| {
                let lines: Vec<Line<'_>> = hunk
                    .unified_text
                    .lines()
                    .map(|l| Line::from(Span::styled(l.to_string(), Self::line_style(theme, l))))
                    .collect();
                Paragraph::new(lines)
            })
            .unwrap_or_else(|| Paragraph::new("(no diff to show)"))
            .wrap(Wrap { trim: false });
        frame.render_widget(body, columns[1]);
    }
}

/// Splits raw unified-diff text into one [`DiffHunk`] per file. Recognizes two boundary styles:
/// `git diff`'s own `diff --git a/<path> b/<path>` header (`core.diff`'s actual source,
/// `RuntimeHandle::run_command`'s `/diff` shells out to `git diff --no-color`) and, when that's
/// absent, the plain `--- a/<path>` / `+++ b/<path>` marker pair a tool's spooled `text_preview`
/// might use instead. Tolerant of text that isn't a unified diff at all: if no marker is found,
/// the whole text becomes a single hunk under an `"(unknown file)"` path rather than being
/// silently discarded.
pub fn parse_unified_diff(text: &str) -> Vec<DiffHunk> {
    if text.lines().any(|l| l.starts_with("diff --git ")) {
        return parse_by_marker(text, "diff --git ", |rest| {
            // `a/<path> b/<path>` — the path (identical on both sides outside a rename) is
            // everything up to " b/".
            rest.split(" b/")
                .next()
                .unwrap_or(rest)
                .trim_start_matches("a/")
                .to_string()
        });
    }
    parse_by_marker(text, "--- ", |rest| {
        rest.trim_start_matches("a/")
            .trim_end_matches("\t(no newline at end of file)")
            .to_string()
    })
}

fn parse_by_marker(
    text: &str,
    marker: &str,
    path_from_rest: impl Fn(&str) -> String,
) -> Vec<DiffHunk> {
    let mut hunks = Vec::new();
    let mut current_path: Option<String> = None;
    let mut current_lines: Vec<&str> = Vec::new();

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(marker) {
            if let Some(path) = current_path.take() {
                hunks.push(DiffHunk {
                    path,
                    unified_text: current_lines.join("\n"),
                });
                current_lines.clear();
            }
            current_path = Some(path_from_rest(rest));
        }
        current_lines.push(line);
    }
    if !current_lines.is_empty() {
        hunks.push(DiffHunk {
            path: current_path.unwrap_or_else(|| "(unknown file)".to_string()),
            unified_text: current_lines.join("\n"),
        });
    }
    hunks
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn parses_a_single_file_diff() {
        let text = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n";
        let hunks = parse_unified_diff(text);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].path, "src/lib.rs");
        assert!(hunks[0].unified_text.contains("+new"));
    }

    #[test]
    fn splits_a_multi_file_diff() {
        let text = "--- a/a.rs\n+++ b/a.rs\n@@\n-1\n+2\n--- a/b.rs\n+++ b/b.rs\n@@\n-3\n+4\n";
        let hunks = parse_unified_diff(text);
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].path, "a.rs");
        assert_eq!(hunks[1].path, "b.rs");
    }

    #[test]
    fn splits_real_git_diff_output_by_diff_git_headers() {
        let text = "diff --git a/a.rs b/a.rs\nindex 111..222 100644\n--- a/a.rs\n+++ b/a.rs\n\
             @@ -1 +1 @@\n-1\n+2\n\
             diff --git a/b.rs b/b.rs\nindex 333..444 100644\n--- a/b.rs\n+++ b/b.rs\n\
             @@ -1 +1 @@\n-3\n+4\n";
        let hunks = parse_unified_diff(text);
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].path, "a.rs");
        assert_eq!(hunks[1].path, "b.rs");
        // The second file's `diff --git`/`index` header lines must not leak into the first
        // file's hunk (the bug plain `--- ` splitting alone would have on real `git diff` output).
        assert!(!hunks[0].unified_text.contains("b.rs"));
    }

    #[test]
    fn non_diff_text_becomes_a_single_unknown_hunk() {
        let hunks = parse_unified_diff("just some text\nno markers here\n");
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].path, "(unknown file)");
    }

    #[test]
    fn selection_wraps() {
        let mut view = DiffView::new(vec![
            DiffHunk {
                path: "a".to_string(),
                unified_text: String::new(),
            },
            DiffHunk {
                path: "b".to_string(),
                unified_text: String::new(),
            },
        ]);
        view.select_prev();
        assert_eq!(view.selected, 1);
        view.select_next();
        assert_eq!(view.selected, 0);
    }
}
