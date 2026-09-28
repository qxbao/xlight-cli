// SPDX-License-Identifier: GPL-3.0-only

//! Integration tests for `grep` (`crates/tools/src/builtin/grep.rs`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use serde_json::json;
use xlightcli_tools::builtin::Grep;
use xlightcli_tools::{AskPolicy, PermissionMode, PermissionRule, RuleEffect, Tool, ToolError};

#[tokio::test]
async fn grep_finds_matches_across_files_with_path_line_number() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src/a.rs"), "fn main() {}\n// TODO: fix\n").unwrap();
    std::fs::write(root.path().join("src/b.rs"), "fn other() {}\n").unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let out = Grep::new()
        .run(json!({"pattern": "TODO"}), ctx)
        .await
        .unwrap();
    let text = out.model_facing_text();
    assert!(text.contains("src/a.rs:2:"));
    assert!(!text.contains("src/b.rs"));
}

#[tokio::test]
async fn grep_respects_path_glob_and_case_sensitivity() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src/a.rs"), "Needle here\n").unwrap();
    std::fs::write(root.path().join("notes.md"), "needle here too\n").unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let out = Grep::new()
        .run(
            json!({"pattern": "needle", "path_glob": "*.rs", "case_sensitive": false}),
            ctx,
        )
        .await
        .unwrap();
    let text = out.model_facing_text();
    assert!(text.contains("src/a.rs"));
    assert!(!text.contains("notes.md"));
}

#[tokio::test]
async fn grep_reports_no_matches_without_error() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), "nothing interesting\n").unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let out = Grep::new()
        .run(json!({"pattern": "zzz_not_present"}), ctx)
        .await
        .unwrap();
    assert!(out.model_facing_text().contains("no matches"));
}

#[tokio::test]
async fn grep_rejects_an_invalid_regex() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let err = Grep::new()
        .run(json!({"pattern": "("}), ctx)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)));
}

#[tokio::test]
async fn grep_denied_by_permission_rule() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), "content\n").unwrap();

    let rule = PermissionRule::parse(RuleEffect::Deny, "read_file(*)").unwrap();
    let ctx = support::context(
        root.path(),
        artifacts.path(),
        PermissionMode::FullAuto,
        vec![rule],
        AskPolicy::AutoAllow,
        "call-1",
    );
    let err = Grep::new()
        .run(json!({"pattern": "content"}), ctx)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::PermissionDenied(_)));
}
