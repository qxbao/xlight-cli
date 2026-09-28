// SPDX-License-Identifier: GPL-3.0-only

//! Input handling: maps a `crossterm` key event to a `crate::keymap::Action` (docs/PLAN.md
//! §18.2). [`chord_of`] normalizes a key event into the same string shape `Keymap`'s bindings use
//! (`"ctrl+c"`, `"shift+tab"`, `"enter"`, ...); [`action_for`] looks that chord up in a `Keymap`.
//! Everything that isn't bound to an `Action` (plain characters, editing keys) falls through to
//! `App::on_key`'s text-editing path.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::keymap::{Action, Keymap};

/// Hardcoded `Ctrl+C` chord, independent of the keymap (a terminal app should always have *some*
/// way to signal "I want out", even with a broken custom keymap) — `App::on_key` uses this to
/// implement "press twice to quit" regardless of whether `Action::Quit` is rebound.
pub fn quit_requested(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
}

/// Normalizes a key event into the chord string format `Keymap` bindings use: modifiers joined by
/// `+` in `ctrl`, `alt`, `shift` order, then a lowercase key name. Returns `""` for keys that never
/// participate in chord matching (e.g. plain modifier presses alone).
///
/// `Shift+Tab` is special-cased to `"shift+tab"`: crossterm reports it as `KeyCode::BackTab`
/// (already shift-flavored), not `Tab` with a `SHIFT` modifier.
pub fn chord_of(key: &KeyEvent) -> String {
    if key.code == KeyCode::BackTab {
        return "shift+tab".to_string();
    }
    let key_name = match key.code {
        KeyCode::Char(c) => c.to_ascii_lowercase().to_string(),
        KeyCode::Enter => "enter".to_string(),
        KeyCode::Tab => "tab".to_string(),
        KeyCode::Backspace => "backspace".to_string(),
        KeyCode::Esc => "esc".to_string(),
        KeyCode::Left => "left".to_string(),
        KeyCode::Right => "right".to_string(),
        KeyCode::Up => "up".to_string(),
        KeyCode::Down => "down".to_string(),
        KeyCode::Home => "home".to_string(),
        KeyCode::End => "end".to_string(),
        KeyCode::PageUp => "pageup".to_string(),
        KeyCode::PageDown => "pagedown".to_string(),
        KeyCode::Delete => "delete".to_string(),
        _ => return String::new(),
    };
    let mut parts = Vec::with_capacity(4);
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        parts.push("ctrl");
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        parts.push("alt");
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        parts.push("shift");
    }
    parts.push(&key_name);
    parts.join("+")
}

/// Resolves `key` to an `Action` via `keymap`'s bindings, or `None` if the key isn't bound to
/// anything (in which case the caller should treat it as ordinary text input).
pub fn action_for(key: &KeyEvent, keymap: &Keymap) -> Option<Action> {
    let chord = chord_of(key);
    if chord.is_empty() {
        return None;
    }
    keymap.action_for_chord(&chord)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn quit_requested_matches_ctrl_c_only() {
        assert!(quit_requested(&key(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        )));
        assert!(!quit_requested(&key(
            KeyCode::Char('c'),
            KeyModifiers::NONE
        )));
    }

    #[test]
    fn chord_of_normalizes_ctrl_p() {
        assert_eq!(
            chord_of(&key(KeyCode::Char('P'), KeyModifiers::CONTROL)),
            "ctrl+p"
        );
    }

    #[test]
    fn chord_of_back_tab_is_shift_tab() {
        assert_eq!(
            chord_of(&key(KeyCode::BackTab, KeyModifiers::NONE)),
            "shift+tab"
        );
    }

    #[test]
    fn chord_of_plain_char_has_no_modifier_prefix() {
        assert_eq!(chord_of(&key(KeyCode::Char('x'), KeyModifiers::NONE)), "x");
    }

    #[test]
    fn action_for_resolves_bound_chord() {
        let keymap = Keymap::default();
        let action = action_for(&key(KeyCode::BackTab, KeyModifiers::NONE), &keymap);
        assert_eq!(action, Some(Action::CycleExecutionMode));
    }

    #[test]
    fn action_for_unbound_char_is_none() {
        let keymap = Keymap::default();
        let action = action_for(&key(KeyCode::Char('x'), KeyModifiers::NONE), &keymap);
        assert_eq!(action, None);
    }
}
