// SPDX-License-Identifier: GPL-3.0-only

//! Input handling: maps a `crossterm` key event to a `crate::keymap::Action` (docs/PLAN.md
//! §18.2). Wave A only handles the one binding `crate::run`'s minimal loop needs (quit); the rest
//! of the keymap-to-action dispatch is Wave B.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::keymap::Action;

/// Recognizes the hardcoded `Ctrl+C` quit chord regardless of keymap overrides (a terminal app
/// should always have *some* way out, even with a broken custom keymap). Everything else is Wave
/// B (`Keymap`-driven chord parsing).
pub fn quit_requested(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
}

/// Placeholder for the full keymap-driven mapping (Wave B). Returns `None` for everything today.
pub fn action_for(_key: &KeyEvent) -> Option<Action> {
    None
}
