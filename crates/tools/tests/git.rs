// SPDX-License-Identifier: GPL-3.0-only

//! Integration tests for `git_status`/`git_diff` (`crates/tools/src/builtin/git.rs`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use serde_json::json;
use xlightcli_tools::builtin::{GitDiff, GitStatus};
use xlightcli_tools::{Tool, ToolError};

async fn init_repo(root: &std::path::Path) {
    support::git_fixture(root, &["init", "-q"]).await;
    support::git_fixture(root, &["config", "user.email", "test@example.com"]).await;
    support::git_fixture(root, &["config", "user.name", "Test"]).await;
}

#[tokio::test]
async fn git_status_reports_untracked_files() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    init_repo(root.path()).await;
    std::fs::write(root.path().join("new.txt"), "content").unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let out = GitStatus::new().run(json!({}), ctx).await.unwrap();
    let text = out.model_facing_text();
    assert!(text.contains("new.txt"));
    assert!(text.contains("??"));
}

#[tokio::test]
async fn git_status_fails_clearly_outside_a_git_repository() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let err = GitStatus::new().run(json!({}), ctx).await.unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)));
}

#[tokio::test]
async fn git_diff_shows_unstaged_changes_to_a_tracked_file() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    init_repo(root.path()).await;
    std::fs::write(root.path().join("a.txt"), "one\n").unwrap();
    support::git_fixture(root.path(), &["add", "a.txt"]).await;
    support::git_fixture(root.path(), &["commit", "-q", "-m", "initial"]).await;

    std::fs::write(root.path().join("a.txt"), "two\n").unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let out = GitDiff::new().run(json!({}), ctx).await.unwrap();
    let text = out.model_facing_text();
    assert!(text.contains("-one"));
    assert!(text.contains("+two"));
}

#[tokio::test]
async fn git_diff_can_be_restricted_to_staged_changes_only() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    init_repo(root.path()).await;
    std::fs::write(root.path().join("a.txt"), "one\n").unwrap();
    support::git_fixture(root.path(), &["add", "a.txt"]).await;
    support::git_fixture(root.path(), &["commit", "-q", "-m", "initial"]).await;

    std::fs::write(root.path().join("a.txt"), "staged\n").unwrap();
    support::git_fixture(root.path(), &["add", "a.txt"]).await;
    std::fs::write(root.path().join("a.txt"), "staged\nunstaged\n").unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let out = GitDiff::new()
        .run(json!({"staged": true}), ctx)
        .await
        .unwrap();
    let text = out.model_facing_text();
    assert!(text.contains("+staged"));
    assert!(!text.contains("+unstaged"));
}

#[tokio::test]
async fn git_diff_can_be_restricted_to_a_single_path() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    init_repo(root.path()).await;
    std::fs::write(root.path().join("a.txt"), "a\n").unwrap();
    std::fs::write(root.path().join("b.txt"), "b\n").unwrap();
    support::git_fixture(root.path(), &["add", "."]).await;
    support::git_fixture(root.path(), &["commit", "-q", "-m", "initial"]).await;

    std::fs::write(root.path().join("a.txt"), "a-changed\n").unwrap();
    std::fs::write(root.path().join("b.txt"), "b-changed\n").unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let out = GitDiff::new()
        .run(json!({"path": "a.txt"}), ctx)
        .await
        .unwrap();
    let text = out.model_facing_text();
    assert!(text.contains("a.txt"));
    assert!(!text.contains("b.txt"));
}
