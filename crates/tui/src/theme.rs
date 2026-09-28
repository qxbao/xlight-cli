// SPDX-License-Identifier: GPL-3.0-only

//! Theme (docs/PLAN.md §12.4 `[ui]`, Phase 6 `core.theme`). Wave A carried just the data shape a
//! theme name resolves to; Wave B adds [`Theme::style`], a small `ThemeRole -> ratatui::style`
//! mapping every view's rendering uses instead of hardcoding colors. A future palette swap
//! (Phase 6 `core.theme`) only needs to change this one function.

use ratatui::style::{Color, Modifier, Style};

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

impl Theme {
    /// Resolves a semantic role to a concrete style. Only one built-in palette exists today
    /// (`self.name` is otherwise unused) — real per-name palettes are Phase 6 `core.theme`.
    pub fn style(&self, role: ThemeRole) -> Style {
        match role {
            ThemeRole::Foreground => Style::default().fg(Color::White),
            ThemeRole::Background => Style::default(),
            ThemeRole::Accent => Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            ThemeRole::Muted => Style::default().fg(Color::DarkGray),
            ThemeRole::Success => Style::default().fg(Color::Green),
            ThemeRole::Warning => Style::default().fg(Color::Yellow),
            ThemeRole::Danger => Style::default().fg(Color::Red),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn danger_role_resolves_to_red() {
        let theme = Theme::default();
        assert_eq!(theme.style(ThemeRole::Danger).fg, Some(Color::Red));
    }
}
