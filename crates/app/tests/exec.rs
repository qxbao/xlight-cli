// SPDX-License-Identifier: GPL-3.0-only

//! Integration test: a valid `xlightcli exec` invocation reaches the real runtime wiring (storage
//! opened, providers/tools registered) and fails with a clear "not implemented yet" error — never
//! a crash, never a fake result (INV-10) — because `xlightcli_runtime::run_exec`'s turn execution
//! is still a Wave B stub (docs/PLAN.md §9.3).

#[tokio::test]
async fn exec_reaches_the_runtime_and_reports_not_implemented() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = xlightcli::wiring::build_runtime_at(dir.path().join("test.db"))
        .await
        .expect("build_runtime_at should open storage successfully");

    let options = xlightcli_runtime::ExecOptions::new("hello");
    let err = xlightcli_runtime::run_exec(&runtime.handle, options)
        .await
        .expect_err("exec::run_exec is still a Wave B stub");
    assert!(matches!(
        err,
        xlightcli_runtime::RuntimeError::NotImplemented(_)
    ));
}
