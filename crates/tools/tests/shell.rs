// SPDX-License-Identifier: GPL-3.0-only

//! Integration tests for `shell` (`crates/tools/src/builtin/shell.rs`): timeout, cancellation,
//! output spooling, and the permission gate.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use serde_json::json;
use xlightcli_tools::builtin::Shell;
use xlightcli_tools::{
    AskPolicy, PermissionMode, PermissionRule, RuleEffect, Tool, ToolError, ToolOutput,
};

#[tokio::test]
async fn shell_runs_a_command_and_reports_the_exit_code() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let out = Shell::new()
        .run(json!({"command": "echo hello-shell"}), ctx)
        .await
        .unwrap();
    let text = out.model_facing_text();
    assert!(text.contains("hello-shell"));
    assert!(text.contains("[exit code: 0]"));
}

#[tokio::test]
async fn shell_reports_a_non_zero_exit_code() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let out = Shell::new()
        .run(json!({"command": "exit 7"}), ctx)
        .await
        .unwrap();
    assert!(out.model_facing_text().contains("[exit code: 7]"));
}

#[tokio::test]
async fn shell_runs_in_the_workspace_root_as_cwd() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("marker.txt"), "here").unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let out = Shell::new()
        .run(json!({"command": "cat marker.txt"}), ctx)
        .await
        .unwrap();
    assert!(out.model_facing_text().contains("here"));
}

#[tokio::test]
async fn shell_times_out_a_long_running_command() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let err = Shell::new()
        .run(json!({"command": "sleep 30", "timeout_secs": 1}), ctx)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Spawn(_)));
}

#[tokio::test]
async fn shell_cancellation_kills_the_process() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let cancel = ctx.cancel.clone();

    let handle =
        tokio::spawn(async move { Shell::new().run(json!({"command": "sleep 30"}), ctx).await });

    // Give the process a moment to actually spawn before cancelling it.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    cancel.cancel();

    let result = tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .expect("shell tool did not react to cancellation in time")
        .expect("task panicked");
    assert!(matches!(result, Err(ToolError::Spawn(_))));
}

#[tokio::test]
async fn shell_spools_large_output_and_keeps_the_full_artifact() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    // Bigger than the default 8 KiB head + 32 KiB tail window (40 KiB), so this exercises
    // truncation while the artifact file still has to contain everything.
    let out = Shell::new()
        .run(
            json!({"command": "head -c 100000 /dev/zero | tr '\\0' 'a'"}),
            ctx,
        )
        .await
        .unwrap();
    match out {
        ToolOutput::Spooled(summary) => {
            assert!(summary.total_bytes >= 100_000);
            assert!(summary.truncated);
            let on_disk_len = std::fs::metadata(&summary.artifact.path).unwrap().len();
            assert!(on_disk_len >= 100_000);
        }
        other => panic!("expected Spooled output, got {other:?}"),
    }
}

#[tokio::test]
async fn shell_denied_by_permission_rule() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();

    let rule = PermissionRule::parse(RuleEffect::Deny, "command(*)").unwrap();
    let ctx = support::context(
        root.path(),
        artifacts.path(),
        PermissionMode::FullAuto,
        vec![rule],
        AskPolicy::AutoAllow,
        "call-1",
    );
    let err = Shell::new()
        .run(json!({"command": "echo should-not-run"}), ctx)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::PermissionDenied(_)));
}
