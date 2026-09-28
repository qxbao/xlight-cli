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
                (Action::CycleExecutionMode, "shift+tab".to_string()),
                (Action::OpenCommandPalette, "ctrl+p".to_string()),
                (Action::OpenAgentTree, "ctrl+a".to_string()),
                (Action::ScrollTranscriptUp, "up".to_string()),
                (Action::ScrollTranscriptDown, "down".to_string()),
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
}

fn action_name(action: Action) -> &'static str {
    match action {
        Action::Submit => "submit",
        Action::Quit => "quit",
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
    fn override_replaces_default_binding() {
        let mut keymap = Keymap::default();
        keymap.apply_overrides(&std::collections::BTreeMap::from([(
            "quit".to_string(),
            "ctrl+q".to_string(),
        )]));
        assert_eq!(keymap.chord_for(Action::Quit), Some("ctrl+q"));
    }
}
