// SPDX-License-Identifier: GPL-3.0-only

//! `App` — top-level TUI state (docs/PLAN.md §18.2). Owns every view's state plus the
//! `RuntimeHandle`. [`App::apply_ui_event`] folds a `UiEvent` into that state; [`App::on_key`] is
//! a pure state transition from one `crossterm::event::KeyEvent` to an [`Intent`] the caller
//! (`crate::run`'s event loop) turns into the one `RuntimeHandle` call it describes — keeping the
//! actual `.await` out of `App` so this stays unit-testable without a Tokio runtime. [`App::draw`]
//! renders the current state with `ratatui`.

use std::collections::BTreeSet;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use xlightcli_protocol::{AgentId, SessionId, ToolCallId};
use xlightcli_runtime::{ExecutionMode, PermissionResponse, RuntimeHandle, UiEvent};

use crate::input;
use crate::keymap::{Action, Keymap};
use crate::theme::Theme;
use crate::view::{
    CommandPaletteView, DiffView, PermissionChoice, PermissionDialogView, PromptView,
    StatusLineView, TranscriptView,
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

/// What `App::on_key` decided should happen, translated into a `RuntimeHandle` call by
/// `crate::run`'s event loop (the one place allowed to `.await`).
#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    /// Nothing to do outside of the state already mutated in place.
    None,
    /// Quit the app (`should_quit = true`; already set by the time this is returned).
    Quit,
    /// Submit `session_id`-less user text; the event loop fills in the active session.
    Submit(String),
    /// Run a `/command` (without the leading `/`).
    RunCommand(String),
    RespondPermission(ToolCallId, PermissionResponse),
    CancelTurn,
    SetExecutionMode(ExecutionMode),
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
    /// The session `Submit`/`RunCommand`/`CancelTurn` target — set from `UiEvent::SessionChanged`/
    /// `TurnStarted`. `None` until the frontend (bare TUI entry point, or a future session picker)
    /// has created or resumed one — see the Wave B report's "known gaps" section.
    pub session_id: Option<SessionId>,
    /// True right after a first `Ctrl+C`, waiting for a second one within the same "no other key
    /// pressed" window (docs/PLAN.md §18.2 brief: "Ctrl+C twice exits").
    pub quit_confirm_pending: bool,
    /// Set by any state change; `crate::run`'s event loop redraws when true and clears it,
    /// implementing the "never redraw per delta, only on a frame-rate-capped tick" requirement.
    pub dirty: bool,
    running_agents: BTreeSet<AgentId>,
    all_agents: BTreeSet<AgentId>,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("execution_mode", &self.execution_mode)
            .field("should_quit", &self.should_quit)
            .field("session_id", &self.session_id)
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
            status_line: Some(StatusLineView::default()),
            overlay: Overlay::default(),
            execution_mode: ExecutionMode::default(),
            should_quit: false,
            session_id: None,
            quit_confirm_pending: false,
            dirty: true,
            running_agents: BTreeSet::new(),
            all_agents: BTreeSet::new(),
        }
    }

    /// Applies a `UiEvent` to the view state. Every variant is handled (Wave B fills in the arms
    /// Wave A left as `_ => {}`); always marks the frame dirty (`crate::run`'s ticking redraw
    /// picks it up on the next tick rather than drawing immediately, PATTERNS.md §3).
    pub fn apply_ui_event(&mut self, event: UiEvent) {
        self.dirty = true;
        match event {
            UiEvent::TurnStarted {
                session_id,
                agent_id,
                model,
            } => {
                self.session_id = Some(session_id);
                self.running_agents.insert(agent_id);
                self.all_agents.insert(agent_id);
                if let Some(status) = &mut self.status_line {
                    status.model = model;
                }
                self.sync_agent_counts();
            }
            UiEvent::TextDelta { agent_id, text, .. } => {
                self.transcript.push_assistant_delta(agent_id, &text);
            }
            UiEvent::ReasoningDelta { agent_id, text, .. } => {
                self.transcript.push_reasoning_delta(agent_id, &text);
            }
            UiEvent::ToolCallStarted {
                agent_id,
                call_id,
                name,
            } => {
                self.transcript.start_tool_call(agent_id, call_id, &name);
            }
            UiEvent::ToolCallFinished {
                agent_id,
                call_id,
                summary,
            } => {
                self.transcript
                    .finish_tool_call(agent_id, &call_id, &summary);
            }
            UiEvent::PermissionRequested { request, .. } => {
                let call_id = request
                    .tool_call_id
                    .clone()
                    .unwrap_or_else(|| ToolCallId::new("unknown"));
                self.overlay = Overlay::Permission(PermissionDialogView::new(call_id, request));
            }
            UiEvent::Usage { usage, .. } => {
                if let Some(status) = &mut self.status_line {
                    status.last_usage = Some(usage);
                }
            }
            UiEvent::RateLimit { info, .. } => {
                if let Some(status) = &mut self.status_line {
                    status.last_rate_limit = Some(info);
                }
            }
            UiEvent::TurnCompleted { agent_id, stop } => {
                self.running_agents.remove(&agent_id);
                self.sync_agent_counts();
                self.transcript
                    .push_notice(format!("turn completed: {stop:?}"));
            }
            UiEvent::TurnFailed { agent_id, error } => {
                self.running_agents.remove(&agent_id);
                self.sync_agent_counts();
                self.transcript.push_error(error);
            }
            UiEvent::SessionChanged { session_id } => {
                self.session_id = Some(session_id);
            }
            UiEvent::ModeChanged { mode, .. } => {
                self.execution_mode = mode;
                if let Some(status) = &mut self.status_line {
                    status.execution_mode = mode;
                }
            }
            UiEvent::Notice { level, message } => {
                self.transcript.push_notice_leveled(level, message);
            }
        }
    }

    fn sync_agent_counts(&mut self) {
        if let Some(status) = &mut self.status_line {
            status.agents_running = self.running_agents.len() as u32;
            status.agents_total = status.agents_total.max(self.all_agents.len() as u32);
        }
    }

    /// `(alias, summary)` pairs for every registered command — owned strings so
    /// `view::CommandPaletteView` doesn't need to name `xlightcli_provider::CommandDefinition`
    /// (which `tui` doesn't depend on, CODEBASE.md §3).
    fn command_entries(&self) -> Vec<(String, String)> {
        self.handle
            .commands()
            .core_commands()
            .iter()
            .map(|d| (d.alias.to_string(), d.summary.to_string()))
            .collect()
    }

    /// Pure state transition: given the next key event, updates `self` and returns the `Intent`
    /// describing what (if anything) `crate::run`'s event loop should ask the `RuntimeHandle` to
    /// do. No `.await` anywhere in this call tree — that's what makes it unit-testable directly.
    pub fn on_key(&mut self, key: KeyEvent) -> Intent {
        if key.kind == KeyEventKind::Release {
            return Intent::None;
        }
        self.dirty = true;

        if input::chord_of(&key) == "ctrl+c" {
            if self.quit_confirm_pending {
                self.should_quit = true;
                return Intent::Quit;
            }
            self.quit_confirm_pending = true;
            self.transcript.push_notice("Press Ctrl+C again to exit.");
            return Intent::None;
        }
        self.quit_confirm_pending = false;

        let overlay = std::mem::take(&mut self.overlay);
        let (overlay, intent) = match overlay {
            Overlay::Permission(view) => self.handle_permission_key(view, key),
            Overlay::CommandPalette(view) => self.handle_palette_key(view, key),
            Overlay::Diff(view) => self.handle_diff_key(view, key),
            Overlay::None => self.handle_normal_key(key),
        };
        self.overlay = overlay;
        if intent == Intent::Quit {
            self.should_quit = true;
        }
        intent
    }

    fn handle_normal_key(&mut self, key: KeyEvent) -> (Overlay, Intent) {
        if let Some(action) = input::action_for(&key, &self.keymap) {
            match action {
                Action::Quit => return (Overlay::None, Intent::Quit),
                Action::Cancel => return (Overlay::None, Intent::CancelTurn),
                Action::CycleExecutionMode => {
                    self.execution_mode = self.execution_mode.next();
                    if let Some(status) = &mut self.status_line {
                        status.execution_mode = self.execution_mode;
                    }
                    return (Overlay::None, Intent::SetExecutionMode(self.execution_mode));
                }
                Action::OpenCommandPalette => {
                    return (Overlay::CommandPalette(self.fresh_palette()), Intent::None);
                }
                // Agent tree is a later Wave (no `AgentId` tree UI exists yet) — swallow the key
                // rather than falling through to text insertion.
                Action::OpenAgentTree => return (Overlay::None, Intent::None),
                Action::ScrollTranscriptUp => {
                    self.transcript.scroll_up(3);
                    return (Overlay::None, Intent::None);
                }
                Action::ScrollTranscriptDown => {
                    self.transcript.scroll_down(3);
                    return (Overlay::None, Intent::None);
                }
                Action::Submit => {
                    let text = self.prompt.take();
                    if text.trim().is_empty() {
                        return (Overlay::None, Intent::None);
                    }
                    if let Some(rest) = text.trim().strip_prefix('/') {
                        self.transcript.push_user(text.clone());
                        return (Overlay::None, Intent::RunCommand(rest.to_string()));
                    }
                    self.transcript.push_user(text.clone());
                    return (Overlay::None, Intent::Submit(text));
                }
            }
        }

        // Typing `/` at the start of an empty prompt opens the palette directly (matches the
        // `core.mcp`-adjacent UX docs/PLAN.md §18.2 describes for other CLIs).
        if key.code == KeyCode::Char('/') && self.prompt.buffer.is_empty() {
            return (Overlay::CommandPalette(self.fresh_palette()), Intent::None);
        }
        // Ctrl+R collapses/expands the most recent reasoning block.
        if key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.transcript.toggle_last_reasoning();
            return (Overlay::None, Intent::None);
        }

        self.prompt.on_key(&key);
        (Overlay::None, Intent::None)
    }

    fn fresh_palette(&self) -> CommandPaletteView {
        let mut view = CommandPaletteView::default();
        view.recompute_matches(&self.command_entries());
        view
    }

    fn handle_permission_key(
        &mut self,
        mut view: PermissionDialogView,
        key: KeyEvent,
    ) -> (Overlay, Intent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                view.select_prev();
                (Overlay::Permission(view), Intent::None)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                view.select_next();
                (Overlay::Permission(view), Intent::None)
            }
            KeyCode::Char('a') => self.resolve_permission(view, PermissionChoice::AllowOnce),
            KeyCode::Char('A') => self.resolve_permission(view, PermissionChoice::AlwaysAllow),
            KeyCode::Char('d') | KeyCode::Esc => {
                self.resolve_permission(view, PermissionChoice::Deny)
            }
            KeyCode::Enter => {
                let choice = view.selected_choice();
                self.resolve_permission(view, choice)
            }
            _ => (Overlay::Permission(view), Intent::None),
        }
    }

    fn resolve_permission(
        &mut self,
        view: PermissionDialogView,
        choice: PermissionChoice,
    ) -> (Overlay, Intent) {
        let call_id = view.tool_call_id.clone();
        match choice {
            PermissionChoice::AllowOnce => (
                Overlay::None,
                Intent::RespondPermission(call_id, PermissionResponse::Allow),
            ),
            PermissionChoice::AlwaysAllow => {
                // Known gap (Wave B, see view::permission_dialog's module doc): there is no
                // `RuntimeHandle` call yet to persist a new allow rule, so this only answers the
                // one pending request and says so — never silently drops the "always" intent
                // (INV-10).
                self.transcript.push_notice(
                    "allowed once; persisting an always-allow rule isn't wired up yet",
                );
                (
                    Overlay::None,
                    Intent::RespondPermission(call_id, PermissionResponse::Allow),
                )
            }
            PermissionChoice::Deny => (
                Overlay::None,
                Intent::RespondPermission(call_id, PermissionResponse::Deny),
            ),
        }
    }

    fn handle_palette_key(
        &mut self,
        mut view: CommandPaletteView,
        key: KeyEvent,
    ) -> (Overlay, Intent) {
        match key.code {
            KeyCode::Esc => (Overlay::None, Intent::None),
            KeyCode::Up => {
                view.select_prev();
                (Overlay::CommandPalette(view), Intent::None)
            }
            KeyCode::Down => {
                view.select_next();
                (Overlay::CommandPalette(view), Intent::None)
            }
            KeyCode::Enter => {
                let entries = self.command_entries();
                match view.selected_alias(&entries) {
                    Some(alias) => (Overlay::None, Intent::RunCommand(alias.to_string())),
                    None => (Overlay::CommandPalette(view), Intent::None),
                }
            }
            KeyCode::Backspace => {
                view.query.pop();
                let entries = self.command_entries();
                view.recompute_matches(&entries);
                (Overlay::CommandPalette(view), Intent::None)
            }
            KeyCode::Char(c) => {
                view.query.push(c);
                let entries = self.command_entries();
                view.recompute_matches(&entries);
                (Overlay::CommandPalette(view), Intent::None)
            }
            _ => (Overlay::CommandPalette(view), Intent::None),
        }
    }

    fn handle_diff_key(&mut self, mut view: DiffView, key: KeyEvent) -> (Overlay, Intent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => (Overlay::None, Intent::None),
            KeyCode::Up | KeyCode::Char('k') => {
                view.select_prev();
                (Overlay::Diff(view), Intent::None)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                view.select_next();
                (Overlay::Diff(view), Intent::None)
            }
            _ => (Overlay::Diff(view), Intent::None),
        }
    }

    /// Opens the diff overlay directly (used by `crate::run`'s `/diff` handling and by tests).
    pub fn open_diff(&mut self, view: DiffView) {
        self.overlay = Overlay::Diff(view);
        self.dirty = true;
    }

    /// Renders the current state. Layout: a 1-row status line, the transcript filling the rest,
    /// and a dynamically-sized prompt at the bottom (`PromptView::height`); an overlay (if any) is
    /// drawn last, on top, as a centered popup.
    pub fn draw(&self, frame: &mut Frame<'_>) {
        let area = frame.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(3),
                Constraint::Length(self.prompt.height()),
            ])
            .split(area);

        if let Some(status) = &self.status_line {
            status.render(frame, chunks[0], &self.theme);
        }
        self.transcript.render(frame, chunks[1], &self.theme);
        let title = format!(" prompt [{}] ", execution_mode_label(self.execution_mode));
        self.prompt.render(frame, chunks[2], &self.theme, &title);

        match &self.overlay {
            Overlay::Permission(view) => view.render(frame, area, &self.theme),
            Overlay::CommandPalette(view) => {
                let entries = self.command_entries();
                view.render(frame, area, &self.theme, &entries);
            }
            Overlay::Diff(view) => view.render(frame, area, &self.theme),
            Overlay::None => {}
        }
    }
}

fn execution_mode_label(mode: ExecutionMode) -> &'static str {
    match mode {
        ExecutionMode::Default => "default",
        ExecutionMode::AcceptEdits => "accept-edits",
        ExecutionMode::Plan => "plan",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use crossterm::event::KeyModifiers;
    use pretty_assertions::assert_eq;
    use xlightcli_runtime::PermissionRequest;

    use super::*;

    async fn test_app() -> (App, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let storage = xlightcli_storage::Storage::open(dir.path().join("test.db"))
            .await
            .unwrap();
        let handle =
            xlightcli_runtime::testing::mock_handle(xlightcli_runtime::testing::mock_deps(storage));
        (App::new(handle), dir)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[tokio::test]
    async fn app_applies_mode_changed_events() {
        let (mut app, _dir) = test_app().await;
        app.apply_ui_event(UiEvent::ModeChanged {
            session_id: SessionId::new(),
            mode: ExecutionMode::Plan,
        });
        assert_eq!(app.execution_mode, ExecutionMode::Plan);
    }

    #[tokio::test]
    async fn text_delta_events_coalesce_into_the_transcript() {
        let (mut app, _dir) = test_app().await;
        let agent_id = AgentId::new();
        app.apply_ui_event(UiEvent::TextDelta {
            agent_id,
            index: 0,
            text: "Hel".to_string(),
        });
        app.apply_ui_event(UiEvent::TextDelta {
            agent_id,
            index: 0,
            text: "lo".to_string(),
        });
        assert_eq!(app.transcript.lines.len(), 1);
        assert_eq!(app.transcript.lines[0].text, "Hello");
    }

    #[tokio::test]
    async fn permission_requested_opens_the_dialog() {
        let (mut app, _dir) = test_app().await;
        app.apply_ui_event(UiEvent::PermissionRequested {
            agent_id: AgentId::new(),
            request: PermissionRequest {
                action: "write_file".to_string(),
                target: "a.rs".to_string(),
                tool_call_id: Some(ToolCallId::new("call-1")),
                reason: "no rule".to_string(),
            },
        });
        assert!(matches!(app.overlay, Overlay::Permission(_)));
    }

    #[tokio::test]
    async fn shift_tab_cycles_execution_mode_and_returns_set_intent() {
        let (mut app, _dir) = test_app().await;
        let intent = app.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
        assert_eq!(app.execution_mode, ExecutionMode::AcceptEdits);
        assert_eq!(intent, Intent::SetExecutionMode(ExecutionMode::AcceptEdits));
    }

    #[tokio::test]
    async fn esc_cancels_the_turn() {
        let (mut app, _dir) = test_app().await;
        let intent = app.on_key(key(KeyCode::Esc));
        assert_eq!(intent, Intent::CancelTurn);
    }

    #[tokio::test]
    async fn single_ctrl_c_does_not_quit_but_second_does() {
        let (mut app, _dir) = test_app().await;
        let first = app.on_key(ctrl(KeyCode::Char('c')));
        assert_eq!(first, Intent::None);
        assert!(!app.should_quit);
        let second = app.on_key(ctrl(KeyCode::Char('c')));
        assert_eq!(second, Intent::Quit);
        assert!(app.should_quit);
    }

    #[tokio::test]
    async fn any_other_key_resets_pending_quit_confirmation() {
        let (mut app, _dir) = test_app().await;
        app.on_key(ctrl(KeyCode::Char('c')));
        assert!(app.quit_confirm_pending);
        app.on_key(key(KeyCode::Char('x')));
        assert!(!app.quit_confirm_pending);
    }

    #[tokio::test]
    async fn typing_a_character_updates_the_prompt_buffer() {
        let (mut app, _dir) = test_app().await;
        app.on_key(key(KeyCode::Char('h')));
        app.on_key(key(KeyCode::Char('i')));
        assert_eq!(app.prompt.buffer, "hi");
    }

    #[tokio::test]
    async fn enter_submits_a_non_slash_prompt() {
        let (mut app, _dir) = test_app().await;
        app.on_key(key(KeyCode::Char('h')));
        app.on_key(key(KeyCode::Char('i')));
        let intent = app.on_key(key(KeyCode::Enter));
        assert_eq!(intent, Intent::Submit("hi".to_string()));
        assert_eq!(app.prompt.buffer, "");
    }

    #[tokio::test]
    async fn enter_on_a_slash_prefixed_prompt_runs_a_command() {
        let (mut app, _dir) = test_app().await;
        for c in "/help".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        let intent = app.on_key(key(KeyCode::Enter));
        assert_eq!(intent, Intent::RunCommand("help".to_string()));
    }

    #[tokio::test]
    async fn slash_on_empty_prompt_opens_the_command_palette() {
        let (mut app, _dir) = test_app().await;
        app.on_key(key(KeyCode::Char('/')));
        assert!(matches!(app.overlay, Overlay::CommandPalette(_)));
    }

    #[tokio::test]
    async fn command_palette_enter_selects_the_highlighted_alias() {
        let (mut app, _dir) = test_app().await;
        app.on_key(key(KeyCode::Char('/')));
        for c in "help".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        let intent = app.on_key(key(KeyCode::Enter));
        assert_eq!(intent, Intent::RunCommand("help".to_string()));
        assert!(matches!(app.overlay, Overlay::None));
    }

    #[tokio::test]
    async fn permission_dialog_a_allows_once() {
        let (mut app, _dir) = test_app().await;
        app.apply_ui_event(UiEvent::PermissionRequested {
            agent_id: AgentId::new(),
            request: PermissionRequest {
                action: "write_file".to_string(),
                target: "a.rs".to_string(),
                tool_call_id: Some(ToolCallId::new("call-1")),
                reason: "no rule".to_string(),
            },
        });
        let intent = app.on_key(key(KeyCode::Char('a')));
        assert_eq!(
            intent,
            Intent::RespondPermission(ToolCallId::new("call-1"), PermissionResponse::Allow)
        );
        assert!(matches!(app.overlay, Overlay::None));
    }

    #[tokio::test]
    async fn permission_dialog_d_denies() {
        let (mut app, _dir) = test_app().await;
        app.apply_ui_event(UiEvent::PermissionRequested {
            agent_id: AgentId::new(),
            request: PermissionRequest {
                action: "shell".to_string(),
                target: "rm -rf".to_string(),
                tool_call_id: Some(ToolCallId::new("call-2")),
                reason: "denied by rule".to_string(),
            },
        });
        let intent = app.on_key(key(KeyCode::Char('d')));
        assert_eq!(
            intent,
            Intent::RespondPermission(ToolCallId::new("call-2"), PermissionResponse::Deny)
        );
    }
}
