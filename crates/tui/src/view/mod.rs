// SPDX-License-Identifier: GPL-3.0-only

//! View state skeleton (docs/PLAN.md §18.2). Every view here holds only *state*; actual
//! `ratatui::Frame` rendering is Wave B (the Wave A brief: "No rendering logic needed yet").

pub mod command_palette;
pub mod diff_view;
pub mod permission_dialog;
pub mod prompt;
pub mod status_line;
pub mod transcript;

pub use command_palette::CommandPaletteView;
pub use diff_view::DiffView;
pub use permission_dialog::PermissionDialogView;
pub use prompt::PromptView;
pub use status_line::StatusLineView;
pub use transcript::TranscriptView;
