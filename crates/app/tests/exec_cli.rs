// SPDX-License-Identifier: GPL-3.0-only
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end `xlightcli exec` tests against the real compiled binary (as opposed to
//! `tests/exec.rs`, which exercises the library seam in-process). Every test runs with isolated
//! `XDG_*` dirs pointed at a fresh tempdir (never the developer's real config/credentials) and
//! never reaches the network: the invalid-input cases fail before any wiring happens, and the
//! "no stored credential" cases fail at `AuthBroker::credential` — before any HTTP client is ever
//! constructed.

use assert_cmd::Command;

fn isolated_cmd(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("xlightcli").expect("the xlightcli binary should be built");
    cmd.env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("XDG_STATE_HOME", dir.join("state"))
        .env_remove("XLIGHTCLI_API_KEY_CODEX")
        // Force the file-backed secret store (D-019) instead of the OS keyring: deterministic,
        // fast, and never touches a real D-Bus Secret Service session in this sandbox.
        .env("XLIGHTCLI_AUTH_STORE", "file")
        .current_dir(dir);
    cmd
}

#[test]
fn missing_prompt_exits_with_invalid_input_code() {
    let dir = tempfile::tempdir().expect("tempdir");
    isolated_cmd(dir.path())
        .arg("exec")
        .assert()
        .failure()
        .code(2);
}

#[test]
fn unknown_mode_exits_with_invalid_input_code() {
    let dir = tempfile::tempdir().expect("tempdir");
    isolated_cmd(dir.path())
        .args(["exec", "-p", "hello", "--mode", "not-a-mode"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn invalid_resume_id_exits_with_invalid_input_code() {
    let dir = tempfile::tempdir().expect("tempdir");
    isolated_cmd(dir.path())
        .args(["exec", "-p", "hello", "--resume", "not-a-uuid"])
        .assert()
        .failure()
        .code(2);
}

/// A fully-specified request (provider/transport/model all given, so `run_exec` gets past
/// `resolve_session`'s "well-formed?" checks — `RuntimeError::InvalidRequest`, exit `2` — and
/// actually reaches `AuthBroker::credential`, which fails with `AuthError::NotLoggedIn` (no
/// credential in this fresh tempdir): exit `1`, per every output format.
const FULL_EXEC_ARGS: [&str; 9] = [
    "exec",
    "-p",
    "hello",
    "--provider",
    "codex",
    "--transport",
    "chatgpt",
    "--model",
    "gpt-5-codex",
];

#[test]
fn text_format_without_credentials_fails_cleanly_not_a_panic() {
    let dir = tempfile::tempdir().expect("tempdir");
    isolated_cmd(dir.path())
        .args(FULL_EXEC_ARGS)
        .args(["--output-format", "text"])
        .assert()
        .failure()
        .code(1);
}

#[test]
fn json_format_without_credentials_fails_cleanly_not_a_panic() {
    let dir = tempfile::tempdir().expect("tempdir");
    isolated_cmd(dir.path())
        .args(FULL_EXEC_ARGS)
        .args(["--output-format", "json"])
        .assert()
        .failure()
        .code(1);
}

#[test]
fn stream_json_format_without_credentials_fails_cleanly_not_a_panic() {
    let dir = tempfile::tempdir().expect("tempdir");
    isolated_cmd(dir.path())
        .args(FULL_EXEC_ARGS)
        .args(["--output-format", "stream-json"])
        .assert()
        .failure()
        .code(1);
}

#[test]
fn missing_transport_and_model_is_an_invalid_request_not_a_generic_error() {
    // Only `--provider` given, no default transport/model configured (`Config::default()`) —
    // `resolve_session` reports this as a well-formed-but-unresolvable request, exit code 2, not
    // the exit-code-1 "talked to something and it failed" case above.
    let dir = tempfile::tempdir().expect("tempdir");
    isolated_cmd(dir.path())
        .args(["exec", "-p", "hello", "--provider", "codex"])
        .assert()
        .failure()
        .code(2);
}
