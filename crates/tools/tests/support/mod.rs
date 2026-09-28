// SPDX-License-Identifier: GPL-3.0-only

//! Shared integration-test support: builds a `ToolContext` over a real (tempdir) workspace with a
//! `StaticPermissionGate`, and a small `git` fixture helper that goes through `ProcessLauncher`
//! (never `std::process::Command` directly — INV-1 applies to test code too, `clippy.toml`'s
//! `disallowed-methods` has no test-only exemption).

#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use xlightcli_protocol::{SessionId, ToolCallId};
use xlightcli_tools::{
    AskPolicy, EnvPolicy, OutputSpool, PermissionEngine, PermissionGate, PermissionMode,
    PermissionRule, ProcessLauncher, SpawnPurpose, SpawnSpec, SpoolLimits, StaticPermissionGate,
    ToolContext, ToolSpoolConfig, WorkspacePath,
};

/// Builds a `ToolContext` rooted at `root`, spooling artifacts under `artifacts_dir`, with the
/// given permission mode/rules and how `Ask` decisions resolve. `call_id` should be unique per
/// tool invocation within a test so each gets its own spool artifact file.
pub fn context(
    root: &Path,
    artifacts_dir: &Path,
    mode: PermissionMode,
    rules: Vec<PermissionRule>,
    ask_policy: AskPolicy,
    call_id: &str,
) -> ToolContext {
    let engine = PermissionEngine::new(mode, rules);
    let gate: Arc<dyn PermissionGate> = Arc::new(StaticPermissionGate::new(engine, ask_policy));
    ToolContext::new(
        WorkspacePath::new(root.to_path_buf()),
        gate,
        Arc::new(ProcessLauncher::new()),
        CancellationToken::new(),
        SessionId::new(),
        ToolCallId::new(call_id),
        ToolSpoolConfig {
            artifacts_dir: artifacts_dir.to_path_buf(),
            limits: SpoolLimits::default(),
        },
    )
}

/// A permissive context: `FullAuto` mode, no rules — every action is allowed outright.
pub fn allow_all_context(root: &Path, artifacts_dir: &Path, call_id: &str) -> ToolContext {
    context(
        root,
        artifacts_dir,
        PermissionMode::FullAuto,
        Vec::new(),
        AskPolicy::AutoAllow,
        call_id,
    )
}

/// Runs `git <args>` in `dir` via `ProcessLauncher` (not a raw `Command`) and asserts success —
/// used only to set up fixture repos, never exercised as "the tool under test".
pub async fn git_fixture(dir: &Path, args: &[&str]) {
    let launcher = ProcessLauncher::new();
    let process = launcher
        .spawn(SpawnSpec {
            purpose: SpawnPurpose::Git,
            program: "git".to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd: dir.to_path_buf(),
            env: EnvPolicy::scrubbed(),
            timeout: Some(Duration::from_secs(10)),
            cancel: CancellationToken::new(),
        })
        .await
        .unwrap_or_else(|e| panic!("spawn git {args:?}: {e}"));

    let spool_dir = tempfile::tempdir().expect("spool dir");
    let mut spool = OutputSpool::create(
        spool_dir.path(),
        SessionId::new(),
        ToolCallId::new("git-fixture"),
        SpoolLimits::default(),
    )
    .await
    .expect("open spool");
    let status = process
        .pipe_into(&mut spool)
        .await
        .unwrap_or_else(|e| panic!("run git {args:?}: {e}"));
    let summary = spool.finish().await.expect("finish spool");
    assert!(
        status.success(),
        "git {args:?} failed: {}",
        summary.head_text()
    );
}
