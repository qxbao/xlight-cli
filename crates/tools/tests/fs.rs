// SPDX-License-Identifier: GPL-3.0-only

//! Integration tests for `read_file`, `write_file`, `edit_file`, `list_dir`, `glob`
//! (`crates/tools/src/builtin/fs.rs`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use serde_json::json;
use xlightcli_tools::builtin::{EditFile, Glob, ListDir, ReadFile, WriteFile};
use xlightcli_tools::{
    AskPolicy, PermissionMode, PermissionRule, RuleEffect, Tool, ToolError, ToolOutput,
};

#[tokio::test]
async fn read_file_returns_the_whole_small_file() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), "hello\nworld\n").unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let out = ReadFile::new()
        .run(json!({"path": "a.txt"}), ctx)
        .await
        .unwrap();
    assert_eq!(out.model_facing_text(), "hello\nworld\n");
}

#[tokio::test]
async fn read_file_supports_a_line_range() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let out = ReadFile::new()
        .run(
            json!({"path": "a.txt", "start_line": 2, "end_line": 3}),
            ctx,
        )
        .await
        .unwrap();
    assert_eq!(out.model_facing_text(), "two\nthree");
}

#[tokio::test]
async fn read_file_rejects_a_start_line_past_the_end() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), "one\ntwo\n").unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let err = ReadFile::new()
        .run(json!({"path": "a.txt", "start_line": 50}), ctx)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)));
}

#[tokio::test]
async fn read_file_rejects_binary_content() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("bin.dat"), [0_u8, 1, 2, 0, 3]).unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let err = ReadFile::new()
        .run(json!({"path": "bin.dat"}), ctx)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)));
}

#[tokio::test]
async fn read_file_spools_a_large_file_and_writes_the_full_artifact() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    // Larger than READ_INLINE_LIMIT (256 KiB) but each window (default 8/32 KiB) still captures
    // only part of it, so this exercises the RAM-bounded spool path end to end.
    let big = "x".repeat(300 * 1024);
    std::fs::write(root.path().join("big.txt"), &big).unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let out = ReadFile::new()
        .run(json!({"path": "big.txt"}), ctx)
        .await
        .unwrap();
    match out {
        ToolOutput::Spooled(summary) => {
            assert_eq!(summary.total_bytes, big.len() as u64);
            assert!(summary.truncated);
            let on_disk = std::fs::read_to_string(&summary.artifact.path).unwrap();
            assert_eq!(on_disk.len(), big.len());
        }
        other => panic!("expected Spooled output, got {other:?}"),
    }
}

#[tokio::test]
async fn read_file_denies_when_permission_rule_denies() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("secret.txt"), "top secret").unwrap();

    let rule = PermissionRule::parse(RuleEffect::Deny, "read_file(*secret*)").unwrap();
    let ctx = support::context(
        root.path(),
        artifacts.path(),
        PermissionMode::FullAuto,
        vec![rule],
        AskPolicy::AutoAllow,
        "call-1",
    );
    let err = ReadFile::new()
        .run(json!({"path": "secret.txt"}), ctx)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::PermissionDenied(_)));
}

#[tokio::test]
async fn read_file_blocks_escaping_the_workspace_with_dotdot() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let err = ReadFile::new()
        .run(json!({"path": "../../etc/passwd"}), ctx)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::PathEscape(_)));
}

#[tokio::test]
async fn read_file_blocks_escaping_the_workspace_with_an_absolute_path() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    // An absolute path is joined onto the root by `Path::join` semantics replacing the whole
    // path only if it starts with `/`; here we exercise the same escape via `..` prefixed with
    // enough segments, since `PathBuf::join` with an absolute path replaces the base entirely on
    // Unix — resolve() must reject that replacement result too.
    let err = ReadFile::new()
        .run(json!({"path": "/etc/passwd"}), ctx)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::PathEscape(_)));
}

#[cfg(unix)]
#[tokio::test]
async fn read_file_blocks_a_symlink_escape() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "nope").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let err = ReadFile::new()
        .run(json!({"path": "escape/secret.txt"}), ctx)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::PathEscape(_)));
}

#[tokio::test]
async fn write_file_creates_missing_parent_directories() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    WriteFile::new()
        .run(json!({"path": "nested/dir/file.txt", "content": "hi"}), ctx)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(root.path().join("nested/dir/file.txt")).unwrap(),
        "hi"
    );
}

#[tokio::test]
async fn write_file_denied_by_permission_rule() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();

    let rule = PermissionRule::parse(RuleEffect::Deny, "write_file(*)").unwrap();
    let ctx = support::context(
        root.path(),
        artifacts.path(),
        PermissionMode::FullAuto,
        vec![rule],
        AskPolicy::AutoAllow,
        "call-1",
    );
    let err = WriteFile::new()
        .run(json!({"path": "a.txt", "content": "x"}), ctx)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::PermissionDenied(_)));
    assert!(!root.path().join("a.txt").exists());
}

#[tokio::test]
async fn write_file_blocks_escaping_the_workspace() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let err = WriteFile::new()
        .run(json!({"path": "../evil.txt", "content": "x"}), ctx)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::PathEscape(_)));
}

#[tokio::test]
async fn edit_file_replaces_a_unique_occurrence_and_returns_a_diff() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("a.txt"),
        "line one\nline two\nline three\n",
    )
    .unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let out = EditFile::new()
        .run(
            json!({"path": "a.txt", "old_string": "line two", "new_string": "line TWO"}),
            ctx,
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(root.path().join("a.txt")).unwrap(),
        "line one\nline TWO\nline three\n"
    );
    let diff = out.model_facing_text();
    assert!(diff.contains("-line two"));
    assert!(diff.contains("+line TWO"));
}

#[tokio::test]
async fn edit_file_rejects_an_absent_old_string() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), "hello\n").unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let err = EditFile::new()
        .run(
            json!({"path": "a.txt", "old_string": "missing", "new_string": "x"}),
            ctx,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)));
}

#[tokio::test]
async fn edit_file_rejects_an_ambiguous_old_string() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), "dup\ndup\n").unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let err = EditFile::new()
        .run(
            json!({"path": "a.txt", "old_string": "dup", "new_string": "x"}),
            ctx,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)));
    // The file must not have been touched on an ambiguous match.
    assert_eq!(
        std::fs::read_to_string(root.path().join("a.txt")).unwrap(),
        "dup\ndup\n"
    );
}

#[tokio::test]
async fn list_dir_lists_and_sorts_entries() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("b.txt"), "").unwrap();
    std::fs::write(root.path().join("a.txt"), "").unwrap();
    std::fs::create_dir(root.path().join("sub")).unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let out = ListDir::new().run(json!({"path": "."}), ctx).await.unwrap();
    let ToolOutput::Structured(value) = out else {
        panic!("expected Structured output");
    };
    let entries = value["entries"].as_array().unwrap();
    let names: Vec<&str> = entries
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["a.txt", "b.txt", "sub"]);
    let sub_kind = entries.iter().find(|e| e["name"] == "sub").unwrap()["kind"]
        .as_str()
        .unwrap();
    assert_eq!(sub_kind, "dir");
}

#[tokio::test]
async fn list_dir_rejects_a_path_that_is_not_a_directory() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), "x").unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let err = ListDir::new()
        .run(json!({"path": "a.txt"}), ctx)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)));
}

#[tokio::test]
async fn glob_finds_matching_files_and_respects_gitignore() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src/main.rs"), "").unwrap();
    std::fs::write(root.path().join("src/lib.rs"), "").unwrap();
    std::fs::write(root.path().join("README.md"), "").unwrap();
    std::fs::write(root.path().join("ignored.rs"), "").unwrap();
    std::fs::write(root.path().join(".gitignore"), "ignored.rs\n").unwrap();

    let ctx = support::allow_all_context(root.path(), artifacts.path(), "call-1");
    let out = Glob::new()
        .run(json!({"pattern": "**/*.rs"}), ctx)
        .await
        .unwrap();
    let ToolOutput::Structured(value) = out else {
        panic!("expected Structured output");
    };
    let matches: Vec<&str> = value["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(matches.contains(&"src/main.rs"));
    assert!(matches.contains(&"src/lib.rs"));
    assert!(!matches.contains(&"ignored.rs"));
    assert!(!matches.iter().any(|m| m.ends_with(".md")));
}
