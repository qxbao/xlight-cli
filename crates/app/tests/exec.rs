// SPDX-License-Identifier: GPL-3.0-only

//! Integration test: a valid `xlightcli exec` invocation reaches the real runtime wiring (storage
//! opened, providers/tools registered). `run_exec` is real now (Wave B, `xlightcli_runtime::exec`)
//! — with no `--provider` and no `default_provider` configured, it can't resolve which
//! session/provider to run against, so it reports a clear `RuntimeError::InvalidRequest` (never a
//! crash, never a fake result — INV-10) rather than the earlier Wave A stub's blanket
//! `NotImplemented`.

#[tokio::test]
async fn exec_without_a_provider_or_default_provider_reports_invalid_request() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = xlightcli::wiring::build_runtime_at(dir.path().join("test.db"))
        .await
        .expect("build_runtime_at should open storage successfully");

    let options = xlightcli_runtime::ExecOptions::new("hello");
    let err = xlightcli_runtime::run_exec(&runtime.handle, options)
        .await
        .expect_err("no --provider and no default_provider configured");
    assert!(matches!(
        err,
        xlightcli_runtime::RuntimeError::InvalidRequest(_)
    ));
}
