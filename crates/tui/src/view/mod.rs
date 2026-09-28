// SPDX-License-Identifier: GPL-3.0-only

//! View state skeleton (docs/PLAN.md §18.2). Every view here holds only *state*; actual
//! `ratatui::Frame` rendering is Wave B (the Wave A brief: "No rendering logic needed yet").

pub mod command_palette;
pub mod diff_view;
pub mod permission_dialog;
pub mod prompt;
pub mod session_setup;
pub mod status_line;
pub mod transcript;

pub use command_palette::CommandPaletteView;
pub use diff_view::{DiffHunk, DiffView, parse_unified_diff};
pub use permission_dialog::{PermissionChoice, PermissionDialogView, centered_rect};
pub use prompt::PromptView;
pub use session_setup::{SessionSetupView, SetupStage};
pub use status_line::StatusLineView;
pub use transcript::{TranscriptLine, TranscriptRole, TranscriptView};
