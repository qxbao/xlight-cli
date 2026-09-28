// SPDX-License-Identifier: GPL-3.0-only

//! `App` — top-level TUI state (docs/PLAN.md §18.2). Owns every view's state plus the
//! `RuntimeHandle`; `crate::run`'s event loop mutates this in response to `UiEvent`s and input
//! `Action`s. No rendering (Wave B: `App::draw(&self, frame: &mut ratatui::Frame)`).

use xlightcli_runtime::ExecutionMode;
use xlightcli_runtime::RuntimeHandle;

use crate::keymap::Keymap;
use crate::theme::Theme;
use crate::view::{
    CommandPaletteView, DiffView, PermissionDialogView, PromptView, StatusLineView, TranscriptView,
};

/// Which overlay (if any) is currently shown on top of the transcript/prompt.
#[derive(Debug, Clone, Default)]
pub enum Overlay {
    #[default]
    None,
    CommandPalette(CommandPaletteView),
    Permission(PermissionDialogView),
    Diff(DiffView),
}

pub struct App {
    pub handle: RuntimeHandle,
    pub keymap: Keymap,
    pub theme: Theme,
    pub transcript: TranscriptView,
    pub prompt: PromptView,
    pub status_line: Option<StatusLineView>,
    pub overlay: Overlay,
    pub execution_mode: ExecutionMode,
    pub should_quit: bool,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("execution_mode", &self.execution_mode)
            .field("should_quit", &self.should_quit)
            .finish_non_exhaustive()
    }
}

impl App {
    pub fn new(handle: RuntimeHandle) -> Self {
        Self {
            handle,
            keymap: Keymap::default(),
            theme: Theme::default(),
            transcript: TranscriptView::new(),
            prompt: PromptView::new(),
            status_line: None,
            overlay: Overlay::default(),
            execution_mode: ExecutionMode::default(),
            should_quit: false,
        }
    }

    /// Applies a `UiEvent` to the view state (Wave B fills in every arm; today only the events
    /// needed to keep the app state consistent — mode changes, quit-worthy failures — are wired).
    pub fn apply_ui_event(&mut self, event: xlightcli_runtime::UiEvent) {
        match event {
            xlightcli_runtime::UiEvent::ModeChanged { mode, .. } => self.execution_mode = mode,
            xlightcli_runtime::UiEvent::TextDelta { text, .. } => self.transcript.push_line(text),
            _ => {}
        }
    }
}
