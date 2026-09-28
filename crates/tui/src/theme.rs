// SPDX-License-Identifier: GPL-3.0-only

//! Theme (docs/PLAN.md §12.4 `[ui]`, Phase 6 `core.theme`). Wave A: just the data shape a theme
//! name resolves to; the actual built-in palettes and rendering are Wave B/Phase 6.

/// A named color role a view can ask the active theme for (kept small and semantic rather than
/// raw RGB, so a future palette swap doesn't need to touch every view).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeRole {
    Foreground,
    Background,
    Accent,
    Muted,
    Success,
    Warning,
    Danger,
}

/// A theme: a name plus (eventually) a `ThemeRole -> ratatui::style::Color` mapping. Wave A only
/// carries the name — `xlightcli_config::UiConfig::theme` already persists this string;
/// `Theme::resolve` (Wave B) turns it into a real style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub name: String,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
        }
    }
}
