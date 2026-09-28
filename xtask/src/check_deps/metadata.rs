// SPDX-License-Identifier: GPL-3.0-only

//! Turns `cargo metadata --format-version 1 --no-deps` output into `rules::CrateNode`s. Parsed
//! with `serde_json::Value` directly (no `cargo_metadata` dependency) since we only need package
//! names and their declared `[dependencies]`/`[dev-dependencies]` — not full resolved versions.

use std::collections::BTreeSet;
use std::process::Command;

use anyhow::{Context, bail};

use super::rules::CrateNode;

/// Maps a Cargo package name to the short name used in `CODEBASE.md` §3 / `rules::ALLOWED_EDGES`.
/// `None` means "not one of our internal crates" (an external dependency like `tokio`), which the
/// caller should ignore rather than flag.
pub fn short_name(pkg_name: &str) -> Option<String> {
    if pkg_name == "xlightcli" {
        Some("app".to_string())
    } else if pkg_name == "xtask" {
        Some("xtask".to_string())
    } else {
        pkg_name.strip_prefix("xlightcli-").map(str::to_string)
    }
}

/// Runs `cargo metadata` in `manifest_dir` (the workspace root) and returns one `CrateNode` per
/// internal workspace crate. Dev tooling only, not a runtime spawn path (INV-1 doesn't apply to
/// `xtask`) — see PATTERNS.md §1 for the one exemption this repo grants outside
/// `tools::launcher::ProcessLauncher`.
#[allow(clippy::disallowed_methods)]
pub fn load_graph(manifest_dir: &std::path::Path) -> anyhow::Result<Vec<CrateNode>> {
    let output = Command::new("cargo")
        .current_dir(manifest_dir)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .context("failed to run `cargo metadata` (is `cargo` on PATH?)")?;
    if !output.status.success() {
        bail!(
            "cargo metadata exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    parse_metadata(&output.stdout)
}

fn parse_metadata(raw: &[u8]) -> anyhow::Result<Vec<CrateNode>> {
    let json: serde_json::Value =
        serde_json::from_slice(raw).context("cargo metadata did not print valid JSON")?;
    let packages = json
        .get("packages")
        .and_then(|v| v.as_array())
        .context("cargo metadata output missing `packages` array")?;

    let mut nodes = Vec::new();
    for pkg in packages {
        let name = pkg
            .get("name")
            .and_then(|v| v.as_str())
            .context("package missing `name`")?;
        let Some(short) = short_name(name) else {
            continue; // not one of our crates (shouldn't happen with --no-deps, but be safe)
        };
        let mut normal_deps = BTreeSet::new();
        let mut dev_deps = BTreeSet::new();
        let deps = pkg
            .get("dependencies")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        for dep in &deps {
            let Some(dep_name) = dep.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(dep_short) = short_name(dep_name) else {
                continue; // external crate, not part of the internal graph
            };
            // `kind` is `null` for [dependencies], `"dev"` for [dev-dependencies], `"build"` for
            // [build-dependencies]. We have no legitimate internal build-dependency, so treat it
            // like normal (still flaggable) rather than silently ignoring it.
            match dep.get("kind").and_then(|v| v.as_str()) {
                Some("dev") => {
                    dev_deps.insert(dep_short);
                }
                _ => {
                    normal_deps.insert(dep_short);
                }
            }
        }
        nodes.push(CrateNode {
            name: short,
            normal_deps,
            dev_deps,
        });
    }
    Ok(nodes)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn short_name_maps_binary_and_prefixed_crates() {
        assert_eq!(short_name("xlightcli").as_deref(), Some("app"));
        assert_eq!(short_name("xtask").as_deref(), Some("xtask"));
        assert_eq!(
            short_name("xlightcli-provider-codex").as_deref(),
            Some("provider-codex")
        );
        assert_eq!(short_name("tokio"), None);
    }

    #[test]
    fn parses_normal_and_dev_dependency_kinds() {
        let raw = serde_json::json!({
            "packages": [
                {
                    "name": "xlightcli-runtime",
                    "dependencies": [
                        { "name": "xlightcli-protocol", "kind": serde_json::Value::Null },
                        { "name": "xlightcli-provider", "kind": "dev" },
                        { "name": "tokio", "kind": serde_json::Value::Null },
                    ]
                }
            ]
        })
        .to_string();
        let nodes = parse_metadata(raw.as_bytes()).unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].name, "runtime");
        assert!(nodes[0].normal_deps.contains("protocol"));
        assert!(!nodes[0].normal_deps.contains("tokio")); // external, ignored
        assert!(nodes[0].dev_deps.contains("provider"));
    }
}
