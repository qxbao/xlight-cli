// SPDX-License-Identifier: GPL-3.0-only

//! Pure dependency-graph rule engine (no IO) enforcing the allowed edges in `CODEBASE.md` §3
//! (INV-2). Kept separate from `cargo metadata` parsing (`super::metadata`) so it can be unit
//! tested with synthetic graphs.

use std::collections::BTreeSet;
use std::fmt;

/// One workspace crate's internal dependency edges, short-named per `CODEBASE.md` §3 (e.g.
/// `xlightcli-provider-codex` -> `"provider-codex"`, the `xlightcli` binary -> `"app"`).
#[derive(Debug, Clone, Default)]
pub struct CrateNode {
    pub name: String,
    /// `[dependencies]` edges (enforced strictly against `ALLOWED_EDGES`).
    pub normal_deps: BTreeSet<String>,
    /// `[dev-dependencies]` edges. Not enforced: dev-deps are allowed to reach into lower crates'
    /// `testing` feature (e.g. `runtime` dev-depending on `xlightcli-provider/testing` for
    /// `MockProvider`) per the Wave 2 brief.
    pub dev_deps: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub krate: String,
    pub dep: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} -> {} is not an allowed dependency edge (CODEBASE.md §3)",
            self.krate, self.dep
        )
    }
}

/// The allowed edge table from `CODEBASE.md` §3, `A -> B` meaning "A may depend on B". Crates not
/// listed here have no internal crates they're allowed to depend on.
const ALLOWED_EDGES: &[(&str, &[&str])] = &[
    ("protocol", &[]),
    ("config", &["protocol"]),
    ("storage", &["protocol"]),
    ("auth", &["protocol", "config", "storage"]),
    ("provider", &["protocol", "auth"]),
    (
        "provider-codex",
        &["provider", "protocol", "auth", "config"],
    ),
    (
        "provider-claude",
        &["provider", "protocol", "auth", "config"],
    ),
    ("provider-agy", &["provider", "protocol", "auth", "config"]),
    ("tools", &["protocol", "config", "storage"]),
    ("mcp", &["protocol", "config", "tools"]),
    (
        "runtime",
        &[
            "protocol", "config", "storage", "auth", "provider", "tools", "mcp",
        ],
    ),
    ("tui", &["protocol", "config", "runtime"]),
    (
        "app",
        &[
            "protocol",
            "config",
            "storage",
            "auth",
            "provider",
            "provider-codex",
            "provider-claude",
            "provider-agy",
            "tools",
            "mcp",
            "runtime",
            "tui",
        ],
    ),
    ("xtask", &[]),
];

/// `None` for a crate name not present in the table (unknown internal crate — treated as having
/// no allowed edges, i.e. every normal dep of it is a violation).
pub fn allowed_edges_for(krate: &str) -> Option<&'static [&'static str]> {
    ALLOWED_EDGES
        .iter()
        .find(|(name, _)| *name == krate)
        .map(|(_, edges)| *edges)
}

/// Checks every node's `normal_deps` against `ALLOWED_EDGES`. Dev-deps are never checked (see
/// `CrateNode::dev_deps` doc). Returns one `Violation` per disallowed edge, in a stable order
/// (nodes then deps, both already `BTreeSet`/slice-ordered inputs from the caller).
pub fn check(nodes: &[CrateNode]) -> Vec<Violation> {
    let mut violations = Vec::new();
    for node in nodes {
        let allowed = allowed_edges_for(&node.name).unwrap_or(&[]);
        for dep in &node.normal_deps {
            if !allowed.contains(&dep.as_str()) {
                violations.push(Violation {
                    krate: node.name.clone(),
                    dep: dep.clone(),
                });
            }
        }
    }
    violations
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn node(name: &str, normal: &[&str], dev: &[&str]) -> CrateNode {
        CrateNode {
            name: name.to_string(),
            normal_deps: normal.iter().map(|s| s.to_string()).collect(),
            dev_deps: dev.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn valid_graph_matching_codebase_md_has_no_violations() {
        let nodes = vec![
            node("protocol", &[], &[]),
            node("config", &["protocol"], &[]),
            node("storage", &["protocol"], &[]),
            node("auth", &["protocol", "config", "storage"], &[]),
            node("provider", &["protocol", "auth"], &[]),
            node(
                "provider-codex",
                &["provider", "protocol", "auth", "config"],
                &[],
            ),
            node("tools", &["protocol", "config", "storage"], &[]),
            node("mcp", &["protocol", "config", "tools"], &[]),
            node(
                "runtime",
                &[
                    "protocol", "config", "storage", "auth", "provider", "tools", "mcp",
                ],
                &[],
            ),
            node("tui", &["protocol", "config", "runtime"], &[]),
            node(
                "app",
                &[
                    "protocol",
                    "config",
                    "storage",
                    "auth",
                    "provider",
                    "provider-codex",
                    "provider-claude",
                    "provider-agy",
                    "tools",
                    "mcp",
                    "runtime",
                    "tui",
                ],
                &[],
            ),
        ];
        assert_eq!(check(&nodes), Vec::new());
    }

    #[test]
    fn detects_runtime_depending_on_provider_codex() {
        // The canonical INV-2 violation: runtime must never see a concrete provider crate.
        let nodes = vec![node("runtime", &["protocol", "provider-codex"], &[])];
        let violations = check(&nodes);
        assert_eq!(
            violations,
            vec![Violation {
                krate: "runtime".to_string(),
                dep: "provider-codex".to_string(),
            }]
        );
    }

    #[test]
    fn detects_tui_depending_on_storage_directly() {
        let nodes = vec![node("tui", &["protocol", "config", "storage"], &[])];
        let violations = check(&nodes);
        assert_eq!(
            violations,
            vec![Violation {
                krate: "tui".to_string(),
                dep: "storage".to_string(),
            }]
        );
    }

    #[test]
    fn dev_dependency_edges_are_never_flagged() {
        // Same illegal edge, but as a dev-dependency: allowed (e.g. test-only MockProvider use).
        let nodes = vec![node("runtime", &["protocol"], &["provider-codex"])];
        assert_eq!(check(&nodes), Vec::new());
    }

    #[test]
    fn tools_and_mcp_never_depend_on_auth() {
        // INV-4 corollary (CODEBASE.md §3): no code path can hand a provider credential to a
        // tool/MCP server.
        let nodes = vec![
            node("tools", &["protocol", "config", "storage", "auth"], &[]),
            node("mcp", &["protocol", "config", "tools", "auth"], &[]),
        ];
        let violations = check(&nodes);
        assert_eq!(violations.len(), 2);
        assert!(violations.iter().all(|v| v.dep == "auth"));
    }

    #[test]
    fn unknown_crate_name_flags_every_normal_dep() {
        let nodes = vec![node("mystery-crate", &["protocol"], &[])];
        assert_eq!(
            check(&nodes),
            vec![Violation {
                krate: "mystery-crate".to_string(),
                dep: "protocol".to_string(),
            }]
        );
    }
}
