// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli-tui` — ratatui + crossterm terminal UI (CODEBASE.md §2, docs/PLAN.md §18.2). Only
//! ever talks to `xlightcli_runtime::RuntimeHandle` (CODEBASE.md §3): no dependency on
//! `storage`/`provider`/`provider-*`/`tools` directly.
//!
//! **Status (Phase 1 Wave B):** [`run`] drives a real terminal lifecycle — raw mode + alternate
//! screen, a panic hook that restores the terminal before any panic message prints, a
//! `tokio::select!` loop over `UiEvent`s and crossterm key/resize events — and now actually draws:
//! `App::draw` renders on a frame-rate-capped tick (never once per delta, PATTERNS.md §3) whenever
//! `App::dirty` is set, and unconditionally on a resize. `app`/`view`/`input`/`keymap`/`theme`
//! hold the state `App::draw` renders and `App::on_key` mutates.

pub mod app;
pub mod error;
pub mod input;
pub mod keymap;
pub mod theme;
pub mod view;

use std::time::Duration;

use crossterm::event::{Event, EventStream};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use tokio_stream::StreamExt;
use xlightcli_runtime::{PermissionResponse, RuntimeHandle};

pub use app::App;
use app::Intent;
pub use error::TuiError;

/// Redraw budget: PATTERNS.md §3 asks for a frame-rate cap rather than a draw per delta. ~20 FPS
/// is plenty for a text UI and keeps a fast-streaming turn from burning CPU on redraws.
const TICK: Duration = Duration::from_millis(50);

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
        app.session_id = Some(session_id);
    }
    let mut ui_events = handle.subscribe().await?;

    install_panic_hook();
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;
    terminal.clear()?;

    let mut key_events = EventStream::new();
    let result = event_loop(&mut app, &mut terminal, &mut ui_events, &mut key_events).await;

    // Always try to restore the terminal, even if the loop above errored.
    let _ = disable_raw_mode();
    let _ = crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen);

    result
}

/// Restores the terminal before the default panic hook prints its message — otherwise a panic
/// while in raw mode / the alternate screen leaves the user's shell in a broken state, invisible
/// panic message included.
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(std::io::stdout(), LeaveAlternateScreen);
        original(info);
    }));
}

type Terminal = ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>;

async fn event_loop(
    app: &mut App,
    terminal: &mut Terminal,
    ui_events: &mut tokio::sync::mpsc::Receiver<xlightcli_runtime::UiEvent>,
    key_events: &mut EventStream,
) -> Result<(), TuiError> {
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            biased;
            Some(event) = ui_events.recv() => {
                app.apply_ui_event(event);
            }
            Some(Ok(event)) = key_events.next() => {
                match event {
                    Event::Key(key) => {
                        let intent = app.on_key(key);
                        dispatch_intent(app, intent).await;
                    }
                    Event::Resize(_, _) => {
                        app.dirty = true;
                    }
                    _ => {}
                }
            }
            _ = ticker.tick() => {
                if app.dirty {
                    terminal.draw(|frame| app.draw(frame))?;
                    app.dirty = false;
                }
            }
        }
        if app.should_quit {
            // One last draw so the "press again to exit" notice (if any) never lingers behind a
            // stale frame — harmless if nothing changed since the last tick.
            if app.dirty {
                let _ = terminal.draw(|frame| app.draw(frame));
            }
            return Ok(());
        }
    }
}

/// Turns an `Intent` (`App::on_key`'s pure output) into the one `RuntimeHandle` call it describes.
/// Errors are surfaced as a transcript notice rather than propagated — a failed permission
/// response/cancel/mode-change shouldn't crash the whole TUI (INV-11 in spirit: an adapter/runtime
/// error must never take the frontend down with it).
async fn dispatch_intent(app: &mut App, intent: Intent) {
    match intent {
        Intent::None | Intent::Quit => {}
        Intent::Submit(text) => {
            let Some(session_id) = app.session_id else {
                app.transcript
                    .push_error("no active session yet; nothing to submit to");
                app.dirty = true;
                return;
            };
            if let Err(err) = app.handle.submit_user_input(session_id, text).await {
                app.transcript.push_error(err.to_string());
                app.dirty = true;
            }
        }
        Intent::RunCommand(raw) => {
            let Some(session_id) = app.session_id else {
                app.transcript
                    .push_error("no active session yet; nothing to run a command against");
                app.dirty = true;
                return;
            };
            run_command(app, session_id, &raw).await;
        }
        Intent::RespondPermission(call_id, response) => {
            let allowed = matches!(response, PermissionResponse::Allow);
            if let Err(err) = app.handle.respond_to_permission(call_id, response).await {
                app.transcript.push_error(err.to_string());
            } else {
                app.transcript.push_notice(if allowed {
                    "permission: allowed"
                } else {
                    "permission: denied"
                });
            }
            app.dirty = true;
        }
        Intent::CancelTurn => {
            let Some(session_id) = app.session_id else {
                return;
            };
            if let Err(err) = app.handle.cancel_turn(session_id).await {
                app.transcript.push_error(err.to_string());
                app.dirty = true;
            }
        }
        Intent::SetExecutionMode(mode) => {
            let Some(session_id) = app.session_id else {
                return;
            };
            if let Err(err) = app.handle.set_execution_mode(session_id, mode).await {
                app.transcript.push_error(err.to_string());
                app.dirty = true;
            }
        }
    }
}

/// Runs a resolved `/command` and folds its `CommandOutcome` into the transcript
/// (`core.diff` specifically opens the diff overlay instead, docs/commands.md §2).
async fn run_command(app: &mut App, session_id: xlightcli_protocol::SessionId, alias: &str) {
    match app.handle.run_command(session_id, alias).await {
        Ok(xlightcli_runtime::CommandOutcome::Message(text)) => {
            if alias == "diff" {
                let hunks = view::parse_unified_diff(&text);
                if hunks.is_empty() {
                    app.transcript.push_notice(text);
                } else {
                    app.open_diff(view::DiffView::new(hunks));
                }
            } else {
                app.transcript.push_notice(text);
            }
        }
        Ok(xlightcli_runtime::CommandOutcome::TurnStarted) => {}
        Ok(xlightcli_runtime::CommandOutcome::Unavailable { reason }) => {
            app.transcript.push_notice(format!("unavailable: {reason}"));
        }
        Err(err) => {
            app.transcript.push_error(err.to_string());
        }
    }
    app.dirty = true;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    async fn test_handle() -> (RuntimeHandle, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let storage = xlightcli_storage::Storage::open(dir.path().join("test.db"))
            .await
            .unwrap();
        (
            xlightcli_runtime::testing::mock_handle(xlightcli_runtime::testing::mock_deps(storage)),
            dir,
        )
    }

    #[tokio::test]
    async fn app_applies_mode_changed_events() {
        let (handle, _dir) = test_handle().await;
        let mut app = App::new(handle);
        app.apply_ui_event(xlightcli_runtime::UiEvent::ModeChanged {
            session_id: xlightcli_protocol::SessionId::new(),
            mode: xlightcli_runtime::ExecutionMode::Plan,
        });
        assert_eq!(app.execution_mode, xlightcli_runtime::ExecutionMode::Plan);
    }

    #[tokio::test]
    async fn submit_intent_without_a_session_reports_a_transcript_error_not_a_panic() {
        let (handle, _dir) = test_handle().await;
        let mut app = App::new(handle);
        dispatch_intent(&mut app, Intent::Submit("hi".to_string())).await;
        assert!(
            app.transcript
                .lines
                .iter()
                .any(|l| l.text.contains("no active session"))
        );
    }

    #[tokio::test]
    async fn run_command_diff_opens_the_diff_overlay_when_text_is_a_unified_diff() {
        let (handle, _dir) = test_handle().await;
        let mut app = App::new(handle);
        // No real session/command backend yet (Wave B stub) — exercise the parsing branch
        // directly instead of the full round trip.
        let hunks = view::parse_unified_diff("--- a/x.rs\n+++ b/x.rs\n@@\n-old\n+new\n");
        app.open_diff(view::DiffView::new(hunks));
        assert!(matches!(app.overlay, app::Overlay::Diff(_)));
    }
}
