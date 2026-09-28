// SPDX-License-Identifier: GPL-3.0-only
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end `xlightcli init` tests against the real compiled binary, in a tempdir, no network.
//! Covers the two behaviors the Wave B brief calls out: writes a fresh project config +
//! `AGENTS.md` stub, and never overwrites an existing file without an explicit confirmation.

use std::fs;

use assert_cmd::Command;

fn isolated_cmd(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("xlightcli").expect("the xlightcli binary should be built");
    cmd.env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("XDG_STATE_HOME", dir.join("state"))
        .current_dir(dir);
    cmd
}

#[test]
fn init_writes_project_config_and_agents_stub_in_a_fresh_dir() {
    let dir = tempfile::tempdir().expect("tempdir");
    isolated_cmd(dir.path()).arg("init").assert().success();

    let config_path = dir.path().join(".xlightcli").join("config.toml");
    assert!(config_path.exists(), "expected {config_path:?} to exist");
    assert!(dir.path().join("AGENTS.md").exists());
}

#[test]
fn init_does_not_overwrite_an_existing_agents_md_without_confirmation() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("AGENTS.md"), "custom content\n").expect("write AGENTS.md");

    // `.xlightcli/config.toml` doesn't exist yet, so only the AGENTS.md prompt consumes a line.
    isolated_cmd(dir.path())
        .arg("init")
        .write_stdin("n\n")
        .assert()
        .success();

    let contents = fs::read_to_string(dir.path().join("AGENTS.md")).expect("read AGENTS.md");
    assert_eq!(contents, "custom content\n");
}

#[test]
fn init_overwrites_an_existing_config_when_confirmed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let xlightcli_dir = dir.path().join(".xlightcli");
    fs::create_dir_all(&xlightcli_dir).expect("mkdir .xlightcli");
    fs::write(xlightcli_dir.join("config.toml"), "stale = true\n").expect("write stale config");

    // AGENTS.md doesn't exist yet, so only the config.toml prompt consumes a line.
    isolated_cmd(dir.path())
        .arg("init")
        .write_stdin("y\n")
        .assert()
        .success();

    let contents = fs::read_to_string(xlightcli_dir.join("config.toml")).expect("read config");
    assert!(contents.contains("xlightcli project configuration"));
}
