// SPDX-License-Identifier: GPL-3.0-only

//! Prompt view (docs/PLAN.md §18.2): the user's multiline input line, submission history
//! (Up/Down when the keymap doesn't claim them for transcript scrolling, see `crate::keymap`),
//! and `/command` autocomplete (`crate::view::command_palette`). `@file` mention autocomplete
//! (`core.mention`) is not implemented yet (Phase 2+).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Text;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::theme::{Theme, ThemeRole};

#[derive(Debug, Clone, Default)]
pub struct PromptView {
    pub buffer: String,
    /// Byte offset into `buffer` (always at a `char` boundary).
    pub cursor: usize,
    /// Previously submitted prompts, oldest first.
    pub history: Vec<String>,
    /// `Some(i)` while navigating history (`i` indexes `history`, counting back from the end);
    /// `None` means the user is editing a fresh (non-history) buffer.
    history_index: Option<usize>,
    /// The buffer as it was before the user started navigating history, restored on
    /// `history_index` reaching `None` again (pressing Down past the newest history entry).
    draft: Option<String>,
}

impl PromptView {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.history_index = None;
        self.draft = None;
    }

    /// Number of terminal rows the prompt needs to render its current content plus a 1-row border
    /// on each side, clamped to a sane range so one giant paste doesn't eat the whole screen.
    pub fn height(&self) -> u16 {
        let lines = self.buffer.lines().count().max(1) as u16;
        (lines + 2).clamp(3, 8)
    }

    /// Takes the buffer (for submission), recording it into history unless blank. Resets cursor
    /// and history navigation state.
    pub fn take(&mut self) -> String {
        let text = std::mem::take(&mut self.buffer);
        self.cursor = 0;
        self.history_index = None;
        self.draft = None;
        if !text.trim().is_empty() {
            self.history.push(text.clone());
        }
        text
    }

    fn insert_char(&mut self, c: char) {
        self.buffer.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev_len = self.buffer[..self.cursor]
            .chars()
            .next_back()
            .map(char::len_utf8)
            .unwrap_or(0);
        let start = self.cursor - prev_len;
        self.buffer.drain(start..self.cursor);
        self.cursor = start;
    }

    fn delete_forward(&mut self) {
        if self.cursor >= self.buffer.len() {
            return;
        }
        let next_len = self.buffer[self.cursor..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(0);
        self.buffer.drain(self.cursor..self.cursor + next_len);
    }

    fn move_left(&mut self) {
        if let Some(prev_len) = self.buffer[..self.cursor]
            .chars()
            .next_back()
            .map(char::len_utf8)
        {
            self.cursor -= prev_len;
        }
    }

    fn move_right(&mut self) {
        if let Some(next_len) = self.buffer[self.cursor..]
            .chars()
            .next()
            .map(char::len_utf8)
        {
            self.cursor += next_len;
        }
    }

    fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next_index = match self.history_index {
            None => {
                self.draft = Some(self.buffer.clone());
                self.history.len() - 1
            }
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.history_index = Some(next_index);
        self.buffer = self.history[next_index].clone();
        self.cursor = self.buffer.len();
    }

    fn history_next(&mut self) {
        let Some(i) = self.history_index else {
            return;
        };
        if i + 1 < self.history.len() {
            self.history_index = Some(i + 1);
            self.buffer = self.history[i + 1].clone();
        } else {
            self.history_index = None;
            self.buffer = self.draft.take().unwrap_or_default();
        }
        self.cursor = self.buffer.len();
    }

    /// Handles a key the caller (`App::on_key`) has already decided is plain text input (i.e. not
    /// bound to any `Action`, and no overlay wants it). Returns `true` if it changed the buffer or
    /// cursor state (so the caller knows to mark the frame dirty).
    pub fn on_key(&mut self, key: &KeyEvent) -> bool {
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert_char(c);
                true
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                self.insert_newline();
                true
            }
            KeyCode::Backspace => {
                self.backspace();
                true
            }
            KeyCode::Delete => {
                self.delete_forward();
                true
            }
            KeyCode::Left => {
                self.move_left();
                true
            }
            KeyCode::Right => {
                self.move_right();
                true
            }
            KeyCode::Up => {
                self.history_prev();
                true
            }
            KeyCode::Down => {
                self.history_next();
                true
            }
            KeyCode::Home => {
                self.cursor = 0;
                true
            }
            KeyCode::End => {
                self.cursor = self.buffer.len();
                true
            }
            _ => false,
        }
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme, title: &str) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title.to_string())
            .style(theme.style(ThemeRole::Foreground));
        let text: Text<'_> = if self.buffer.is_empty() {
            Text::styled(
                "Type a message, or /command...",
                theme.style(ThemeRole::Muted),
            )
        } else {
            Text::raw(self.buffer.clone())
        };
        let paragraph = Paragraph::new(text).block(block).wrap(Wrap { trim: false });
        frame.render_widget(paragraph, area);

        // Cursor position: naive — counts newlines/columns up to `self.cursor`. Good enough for
        // the input sizes a terminal prompt actually sees.
        let (row, col) = self.cursor_row_col();
        let inner_x = area.x + 1 + col as u16;
        let inner_y = area.y + 1 + row as u16;
        if inner_x < area.x + area.width.saturating_sub(1)
            && inner_y < area.y + area.height.saturating_sub(1)
        {
            frame.set_cursor_position((inner_x, inner_y));
        }
    }

    fn cursor_row_col(&self) -> (usize, usize) {
        let before = &self.buffer[..self.cursor];
        let row = before.matches('\n').count();
        let col = before.rsplit('\n').next().unwrap_or("").chars().count();
        (row, col)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use crossterm::event::KeyModifiers;
    use pretty_assertions::assert_eq;

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn typing_inserts_at_cursor() {
        let mut view = PromptView::new();
        view.on_key(&key(KeyCode::Char('h')));
        view.on_key(&key(KeyCode::Char('i')));
        assert_eq!(view.buffer, "hi");
        assert_eq!(view.cursor, 2);
    }

    #[test]
    fn backspace_removes_previous_char() {
        let mut view = PromptView::new();
        view.buffer = "hi".to_string();
        view.cursor = 2;
        view.on_key(&key(KeyCode::Backspace));
        assert_eq!(view.buffer, "h");
        assert_eq!(view.cursor, 1);
    }

    #[test]
    fn take_records_non_blank_submission_into_history() {
        let mut view = PromptView::new();
        view.buffer = "hello".to_string();
        view.cursor = 5;
        let taken = view.take();
        assert_eq!(taken, "hello");
        assert_eq!(view.buffer, "");
        assert_eq!(view.history, vec!["hello".to_string()]);
    }

    #[test]
    fn take_does_not_record_blank_submission() {
        let mut view = PromptView::new();
        view.buffer = "   ".to_string();
        view.take();
        assert!(view.history.is_empty());
    }

    #[test]
    fn history_prev_then_next_restores_draft() {
        let mut view = PromptView::new();
        view.history = vec!["first".to_string(), "second".to_string()];
        view.buffer = "draft".to_string();
        view.cursor = 5;

        view.on_key(&key(KeyCode::Up));
        assert_eq!(view.buffer, "second");
        view.on_key(&key(KeyCode::Up));
        assert_eq!(view.buffer, "first");
        view.on_key(&key(KeyCode::Down));
        assert_eq!(view.buffer, "second");
        view.on_key(&key(KeyCode::Down));
        assert_eq!(view.buffer, "draft");
    }

    #[test]
    fn height_is_clamped_between_3_and_8() {
        let mut view = PromptView::new();
        assert_eq!(view.height(), 3);
        view.buffer = "a\n".repeat(20);
        assert_eq!(view.height(), 8);
    }
}
