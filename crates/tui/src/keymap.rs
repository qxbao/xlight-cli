// SPDX-License-Identifier: GPL-3.0-only

//! Keybindings (docs/PLAN.md §12.4 `[keybind]`, Phase 6 `core.keybindings`). Wave A: the action
//! set + a default mapping loaded from `xlightcli_config::UiConfig::keybind`; actually dispatching
//! a `crossterm::event::KeyEvent` to an `Action` is Wave B (`crate::input`).

use std::collections::BTreeMap;

/// A user-invocable action a key chord can be bound to. Intentionally a flat enum for Phase 1 —
/// this grows as more of docs/commands.md's core commands get dedicated keybindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Action {
    Submit,
    Quit,
    /// Esc: cancel the in-flight turn (docs/PLAN.md §18.2 brief).
    Cancel,
    CycleExecutionMode,
    OpenCommandPalette,
    OpenAgentTree,
    ScrollTranscriptUp,
    ScrollTranscriptDown,
}

/// Action name -> key chord string (docs/PLAN.md §12.4 example: `"new_agent" -> "ctrl+n"`).
#[derive(Debug, Clone)]
pub struct Keymap {
    bindings: BTreeMap<Action, String>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self {
            bindings: BTreeMap::from([
                (Action::Submit, "enter".to_string()),
                (Action::Quit, "ctrl+c".to_string()),
                (Action::Cancel, "esc".to_string()),
                (Action::CycleExecutionMode, "shift+tab".to_string()),
                (Action::OpenCommandPalette, "ctrl+p".to_string()),
                (Action::OpenAgentTree, "ctrl+a".to_string()),
                // Plain Up/Down are left free for the prompt's history navigation
                // (`view::prompt::PromptView`); the transcript scrolls on Page Up/Down instead.
                (Action::ScrollTranscriptUp, "pageup".to_string()),
                (Action::ScrollTranscriptDown, "pagedown".to_string()),
            ]),
        }
    }
}

impl Keymap {
    /// Overrides bindings from `xlightcli_config::UiConfig::keybind`. Unknown action names are
    /// ignored (Wave B may want to surface these as a config warning instead).
    pub fn apply_overrides(&mut self, overrides: &std::collections::BTreeMap<String, String>) {
        let actions: Vec<Action> = self.bindings.keys().copied().collect();
        for action in actions {
            if let Some(chord_override) = overrides.get(action_name(action)) {
                self.bindings.insert(action, chord_override.clone());
            }
        }
    }

    pub fn chord_for(&self, action: Action) -> Option<&str> {
        self.bindings.get(&action).map(String::as_str)
    }

    /// Reverse lookup: which action (if any) is bound to `chord` (as produced by
    /// `crate::input::chord_of`). Used by `crate::input::action_for` to dispatch a key event.
    pub fn action_for_chord(&self, chord: &str) -> Option<Action> {
        self.bindings
            .iter()
            .find(|(_, bound)| bound.as_str() == chord)
            .map(|(action, _)| *action)
    }
}

fn action_name(action: Action) -> &'static str {
    match action {
        Action::Submit => "submit",
        Action::Quit => "quit",
        Action::Cancel => "cancel",
        Action::CycleExecutionMode => "cycle_execution_mode",
        Action::OpenCommandPalette => "command_palette",
        Action::OpenAgentTree => "agent_tree",
        Action::ScrollTranscriptUp => "scroll_transcript_up",
        Action::ScrollTranscriptDown => "scroll_transcript_down",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn default_binds_shift_tab_to_cycle_execution_mode() {
        let keymap = Keymap::default();
        assert_eq!(
            keymap.chord_for(Action::CycleExecutionMode),
            Some("shift+tab")
        );
    }

    #[test]
    fn action_for_chord_reverse_lookup() {
        let keymap = Keymap::default();
        assert_eq!(
            keymap.action_for_chord("shift+tab"),
            Some(Action::CycleExecutionMode)
        );
        assert_eq!(keymap.action_for_chord("esc"), Some(Action::Cancel));
        assert_eq!(keymap.action_for_chord("ctrl+z"), None);
    }

    #[test]
    fn override_replaces_default_binding() {
        let mut keymap = Keymap::default();
        keymap.apply_overrides(&std::collections::BTreeMap::from([(
            "quit".to_string(),
            "ctrl+q".to_string(),
        )]));
        assert_eq!(keymap.chord_for(Action::Quit), Some("ctrl+q"));
    }
}
