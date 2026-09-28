// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli-tui` — ratatui + crossterm terminal UI (CODEBASE.md §2, docs/PLAN.md §18.2). Only
//! ever talks to `xlightcli_runtime::RuntimeHandle` (CODEBASE.md §3): no dependency on
//! `storage`/`provider`/`provider-*`/`tools` directly.
//!
//! **Status (Phase 1 Wave A — contracts):** [`run`] is a real (if minimal) terminal lifecycle —
//! raw mode + alternate screen, a `tokio::select!` loop over `UiEvent`s and key events, restores
//! the terminal on the way out — but never calls `Terminal::draw` (no rendering logic yet, per
//! the Wave A brief). `app`/`view`/`input`/`keymap`/`theme` are the module skeleton Wave B renders
//! into. This entry point can't meaningfully be unit-tested (it requires a real TTY); the state
//! types it drives (`App`, `view::*`, `Keymap`) are tested directly instead.

pub mod app;
pub mod error;
pub mod input;
pub mod keymap;
pub mod theme;
pub mod view;

use crossterm::event::{Event, EventStream};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use tokio_stream::StreamExt;
use xlightcli_runtime::RuntimeHandle;

pub use app::App;
pub use error::TuiError;

/// Options for [`run`] (docs/PLAN.md §18.3 — the bare `xlightcli` invocation).
#[derive(Debug, Clone, Default)]
pub struct TuiOptions {
    /// Resume this session instead of starting fresh.
    pub initial_session: Option<xlightcli_protocol::SessionId>,
}

/// Runs the TUI until the user quits. Anyhow-free (PATTERNS.md §2: `anyhow` only in `app`/`xtask`)
/// — `app::main` wraps any `Err` for its own reporting.
pub async fn run(handle: RuntimeHandle, opts: TuiOptions) -> Result<(), TuiError> {
    let mut app = App::new(handle.clone());
    if let Some(session_id) = opts.initial_session {
        handle.resume_session(session_id).await?;
    }
    let mut ui_events = handle.subscribe().await?;

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;

    let mut key_events = EventStream::new();
    let result = event_loop(&mut app, &mut ui_events, &mut key_events).await;

    // Always try to restore the terminal, even if the loop above errored.
    let _ = disable_raw_mode();
    let _ = crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen);

    result
}

async fn event_loop(
    app: &mut App,
    ui_events: &mut tokio::sync::mpsc::Receiver<xlightcli_runtime::UiEvent>,
    key_events: &mut EventStream,
) -> Result<(), TuiError> {
    loop {
        tokio::select! {
            biased;
            Some(event) = ui_events.recv() => {
                app.apply_ui_event(event);
            }
            Some(Ok(Event::Key(key))) = key_events.next() => {
                if input::quit_requested(&key) {
                    app.should_quit = true;
                }
            }
        }
        if app.should_quit {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[tokio::test]
    async fn app_applies_mode_changed_events() {
        let dir = tempfile::tempdir().unwrap();
        let storage = xlightcli_storage::Storage::open(dir.path().join("test.db"))
            .await
            .unwrap();
        let handle =
            xlightcli_runtime::testing::mock_handle(xlightcli_runtime::testing::mock_deps(storage));
        let mut app = App::new(handle);
        app.apply_ui_event(xlightcli_runtime::UiEvent::ModeChanged {
            session_id: xlightcli_protocol::SessionId::new(),
            mode: xlightcli_runtime::ExecutionMode::Plan,
        });
        assert_eq!(app.execution_mode, xlightcli_runtime::ExecutionMode::Plan);
    }
}
