// SPDX-License-Identifier: GPL-3.0-only
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Snapshot tests for the key screens the Phase 1 Wave B brief calls out: empty session,
//! streaming answer, tool call card, permission dialog, diff view, status line in each execution
//! mode. Uses `ratatui::backend::TestBackend` (renders into an in-memory cell buffer, no real
//! TTY needed) plus `insta` for the actual snapshot assertions/review workflow
//! (`cargo insta review` — see `PATTERNS.md` §13, which documents `insta` as the project's
//! snapshot tool of choice for fixture-driven tests).

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use xlightcli_protocol::{AgentId, ModelId, SessionId, ToolCallId};
use xlightcli_runtime::{ArtifactRef, ExecutionMode, PermissionRequest, ToolCallSummary, UiEvent};
use xlightcli_tui::App;
use xlightcli_tui::view::{DiffHunk, DiffView};

const WIDTH: u16 = 80;
const HEIGHT: u16 = 20;

async fn test_app() -> (App, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let storage = xlightcli_storage::Storage::open(dir.path().join("test.db"))
        .await
        .expect("open storage");
    let handle =
        xlightcli_runtime::testing::mock_handle(xlightcli_runtime::testing::mock_deps(storage));
    (App::new(handle), dir)
}

/// Renders `buffer`'s cell grid into a plain-text, line-per-row string — easy to read in a
/// snapshot diff, unlike `Buffer`'s derived `Debug` (which dumps every `Cell` field).
fn buffer_to_string(buffer: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let symbol = buffer.cell((x, y)).map(|cell| cell.symbol()).unwrap_or(" ");
            out.push_str(symbol);
        }
        out.push('\n');
    }
    out
}

fn render(app: &App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal.draw(|frame| app.draw(frame)).expect("draw");
    buffer_to_string(terminal.backend().buffer())
}

#[tokio::test]
async fn empty_session_screen() {
    let (app, _dir) = test_app().await;
    insta::assert_snapshot!(render(&app, WIDTH, HEIGHT));
}

#[tokio::test]
async fn streaming_answer_screen() {
    let (mut app, _dir) = test_app().await;
    let agent = AgentId::new();
    app.apply_ui_event(UiEvent::TurnStarted {
        session_id: SessionId::new(),
        agent_id: agent,
        model: ModelId::new("gpt-5"),
    });
    app.apply_ui_event(UiEvent::TextDelta {
        agent_id: agent,
        index: 0,
        text: "Hello, ".to_string(),
    });
    app.apply_ui_event(UiEvent::TextDelta {
        agent_id: agent,
        index: 0,
        text: "world!".to_string(),
    });
    insta::assert_snapshot!(render(&app, WIDTH, HEIGHT));
}

#[tokio::test]
async fn tool_call_card_screen() {
    let (mut app, _dir) = test_app().await;
    let agent = AgentId::new();
    let call_id = ToolCallId::new("call-1");
    app.apply_ui_event(UiEvent::ToolCallStarted {
        agent_id: agent,
        call_id: call_id.clone(),
        name: "read_file".to_string(),
    });
    app.apply_ui_event(UiEvent::ToolCallFinished {
        agent_id: agent,
        call_id: call_id.clone(),
        summary: ToolCallSummary {
            name: "read_file".to_string(),
            text_preview: "42 lines read from src/main.rs".to_string(),
            artifact: Some(ArtifactRef {
                session_id: SessionId::new(),
                call_id,
                path: "/tmp/artifacts/read_file.log".into(),
            }),
        },
    });
    insta::assert_snapshot!(render(&app, WIDTH, HEIGHT));
}

#[tokio::test]
async fn permission_dialog_screen() {
    let (mut app, _dir) = test_app().await;
    app.apply_ui_event(UiEvent::PermissionRequested {
        agent_id: AgentId::new(),
        request: PermissionRequest {
            action: "write_file".to_string(),
            target: "src/lib.rs".to_string(),
            tool_call_id: Some(ToolCallId::new("call-2")),
            reason: "no matching allow rule".to_string(),
        },
    });
    insta::assert_snapshot!(render(&app, WIDTH, HEIGHT));
}

#[tokio::test]
async fn diff_view_screen() {
    let (mut app, _dir) = test_app().await;
    app.open_diff(DiffView::new(vec![DiffHunk {
        path: "src/lib.rs".to_string(),
        unified_text: "@@ -1,2 +1,2 @@\n-let x = 1;\n+let x = 2;\n".to_string(),
    }]));
    insta::assert_snapshot!(render(&app, WIDTH, HEIGHT));
}

/// Renders just the status line widget at its natural 1-row height — going through the full
/// `App::draw` at `HEIGHT: 1` would starve `Constraint::Min(3)` (transcript) and produce a
/// misleading layout, since ratatui's constraint solver reallocates rather than erroring.
async fn status_line_snapshot(mode: ExecutionMode) -> String {
    let (mut app, _dir) = test_app().await;
    app.apply_ui_event(UiEvent::ModeChanged {
        session_id: SessionId::new(),
        mode,
    });
    let backend = TestBackend::new(WIDTH, 1);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| {
            if let Some(status) = &app.status_line {
                let area = frame.area();
                status.render(frame, area, &app.theme);
            }
        })
        .expect("draw");
    buffer_to_string(terminal.backend().buffer())
}

#[tokio::test]
async fn status_line_default_mode() {
    insta::assert_snapshot!(status_line_snapshot(ExecutionMode::Default).await);
}

#[tokio::test]
async fn status_line_accept_edits_mode() {
    insta::assert_snapshot!(status_line_snapshot(ExecutionMode::AcceptEdits).await);
}

#[tokio::test]
async fn status_line_plan_mode() {
    insta::assert_snapshot!(status_line_snapshot(ExecutionMode::Plan).await);
}
