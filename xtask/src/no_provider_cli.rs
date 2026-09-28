// SPDX-License-Identifier: GPL-3.0-only

//! `xtask no-provider-cli` — e2e enforcement of INV-1 (docs/PLAN.md §16, "No-provider-CLI",
//! mandatory in CI): builds the `xlightcli` binary, prepends a temp `PATH` containing
//! marker-writing shims named after every provider CLI, runs a batch of commands against temp XDG
//! dirs, and asserts the marker was never written (i.e. none of the shims were ever invoked).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

/// Provider CLI names that must never be spawned by `xlightcli` (INV-1).
const SHIMMED_PROGRAMS: &[&str] = &["codex", "claude", "agy", "antigravity"];

/// Runs the full check. `manifest_dir` is the workspace root (so `cargo build`/`cargo metadata`
/// resolve the same workspace regardless of the caller's cwd).
// `xtask` is a dev-only CLI, not a library (PATTERNS.md §14's "no println in library" doesn't
// apply); this is its user-facing progress output.
#[allow(clippy::print_stdout, clippy::print_stderr)]
pub fn run(manifest_dir: &Path) -> anyhow::Result<()> {
    let target_dir = target_directory(manifest_dir)?;
    build_binary(manifest_dir)?;
    let bin_path = target_dir.join("debug").join("xlightcli");
    if !bin_path.is_file() {
        bail!(
            "expected binary at {} after `cargo build -p xlightcli` — build succeeded but binary \
             is missing (unexpected target layout?)",
            bin_path.display()
        );
    }

    let tmp = tempfile::tempdir().context("failed to create temp dir for shims/XDG dirs")?;
    let marker = tmp.path().join("provider-cli-invoked.marker");
    let shim_dir = tmp.path().join("shims");
    fs::create_dir_all(&shim_dir).context("failed to create shim dir")?;
    for program in SHIMMED_PROGRAMS {
        write_shim(&shim_dir, program, &marker)
            .with_context(|| format!("failed to write shim for {program}"))?;
    }

    let xdg_data = tmp.path().join("xdg-data");
    let xdg_config = tmp.path().join("xdg-config");
    let xdg_state = tmp.path().join("xdg-state");
    for dir in [&xdg_data, &xdg_config, &xdg_state] {
        fs::create_dir_all(dir).context("failed to create temp XDG dir")?;
    }

    let existing_path = std::env::var("PATH").unwrap_or_default();
    let patched_path = format!("{}:{existing_path}", shim_dir.display());

    // `dev probe` is intentionally NOT exercised here: Wave 2 transports hard-code their upstream
    // endpoint in each adapter's `consts.rs` with no override hook yet, so there is no way to
    // point a probe at an unreachable URL without either a real credential or a real network call
    // (neither allowed in this sandbox/CI). Skipped per the Wave 2 brief ("if endpoints are
    // overridable, else skip"); revisit once transports support a base-URL override for tests.
    let commands: &[&[&str]] = &[
        &["--help"],
        &["provider", "list"],
        &["provider", "info", "codex"],
        &["provider", "info", "claude"],
        &["provider", "info", "agy"],
        &["auth", "list"],
    ];

    println!(
        "xtask no-provider-cli: running {} command(s) against {}",
        commands.len(),
        bin_path.display()
    );
    for args in commands {
        #[allow(clippy::disallowed_methods)]
        // dev tooling driving the binary under test, not the runtime spawn path (see AGENTS.md Wave 2 brief for xtask)
        let output = Command::new(&bin_path)
            .args(*args)
            .env("PATH", &patched_path)
            .env("XDG_DATA_HOME", &xdg_data)
            .env("XDG_CONFIG_HOME", &xdg_config)
            .env("XDG_STATE_HOME", &xdg_state)
            .env("XLIGHTCLI_AUTH_STORE", "file")
            .output()
            .with_context(|| format!("failed to run `xlightcli {}`", args.join(" ")))?;
        if marker.exists() {
            bail!(
                "INV-1 violation: `xlightcli {}` invoked a shimmed provider CLI (marker at {})",
                args.join(" "),
                marker.display()
            );
        }
        println!(
            "  ran: xlightcli {} (exit {:?})",
            args.join(" "),
            output.status.code()
        );
    }

    println!("xtask no-provider-cli: no provider CLI shim was ever invoked (INV-1 holds)");
    Ok(())
}

#[allow(clippy::disallowed_methods)] // dev tooling: builds the binary under test, not the runtime spawn path
fn build_binary(manifest_dir: &Path) -> anyhow::Result<()> {
    let status = Command::new("cargo")
        .current_dir(manifest_dir)
        .args(["build", "-p", "xlightcli"])
        .status()
        .context("failed to run `cargo build -p xlightcli`")?;
    if !status.success() {
        bail!("`cargo build -p xlightcli` failed with {status}");
    }
    Ok(())
}

#[allow(clippy::disallowed_methods)] // dev tooling: reads workspace layout, not the runtime spawn path
fn target_directory(manifest_dir: &Path) -> anyhow::Result<PathBuf> {
    let output = Command::new("cargo")
        .current_dir(manifest_dir)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .context("failed to run `cargo metadata`")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let dir = json
        .get("target_directory")
        .and_then(|v| v.as_str())
        .context("cargo metadata output missing `target_directory`")?;
    Ok(PathBuf::from(dir))
}

/// Writes an executable shell shim at `shim_dir/program` that appends its invocation to `marker`
/// and exits non-zero (so callers notice if one is ever actually invoked, even without checking
/// the marker file).
fn write_shim(shim_dir: &Path, program: &str, marker: &Path) -> anyhow::Result<()> {
    let path = shim_dir.join(program);
    let script = format!(
        "#!/bin/sh\necho \"$0 $*\" >> \"{}\"\nexit 1\n",
        marker.display()
    );
    fs::write(&path, script).with_context(|| format!("failed to write shim {}", path.display()))?;
    let mut perms = fs::metadata(&path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

    use super::*;

    #[test]
    fn shim_is_executable_and_writes_marker_on_invocation() {
        let tmp = tempfile::tempdir().unwrap();
        let shim_dir = tmp.path().join("shims");
        fs::create_dir_all(&shim_dir).unwrap();
        let marker = tmp.path().join("marker");

        write_shim(&shim_dir, "codex", &marker).unwrap();
        let shim_path = shim_dir.join("codex");
        assert!(shim_path.is_file());
        let mode = fs::metadata(&shim_path).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "shim must be executable");

        assert!(!marker.exists());
        let status = Command::new(&shim_path).arg("hello").status().unwrap();
        assert!(!status.success(), "shim exits non-zero by design");
        let contents = fs::read_to_string(&marker).unwrap();
        assert!(contents.contains("codex"));
        assert!(contents.contains("hello"));
    }
}
