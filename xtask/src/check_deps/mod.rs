// SPDX-License-Identifier: GPL-3.0-only

//! `xtask check-deps` — enforces INV-2 by rejecting any internal dependency edge not listed in
//! `CODEBASE.md` §3. Split into a pure rule engine (`rules`, unit-testable with synthetic graphs)
//! and `cargo metadata` parsing (`metadata`).

mod metadata;
mod rules;

use std::path::Path;

/// Runs `cargo metadata` against the workspace at `manifest_dir`, checks the resulting graph, and
/// prints+returns an error listing every violation. Exits clean (Ok) iff the graph matches
/// `CODEBASE.md` §3 exactly.
// `xtask` is a dev-only CLI, not a library (PATTERNS.md §14's "no println in library" doesn't
// apply); this is its user-facing output.
#[allow(clippy::print_stdout, clippy::print_stderr)]
pub fn run(manifest_dir: &Path) -> anyhow::Result<()> {
    let nodes = metadata::load_graph(manifest_dir)?;
    if nodes.is_empty() {
        anyhow::bail!("cargo metadata returned no workspace packages — nothing to check");
    }
    let violations = rules::check(&nodes);
    if violations.is_empty() {
        // dev-dependency edges are never enforced (rules::CrateNode::dev_deps doc) but are worth
        // surfacing: a dev-dep on a concrete provider-* crate from runtime/tui is exactly the
        // kind of thing a reviewer should double-check even though it's not an INV-2 violation.
        let dev_edges: usize = nodes.iter().map(|n| n.dev_deps.len()).sum();
        println!(
            "xtask check-deps: {} crates checked, dependency graph matches CODEBASE.md §3 \
             ({dev_edges} dev-dependency edge(s), not enforced)",
            nodes.len()
        );
        return Ok(());
    }
    eprintln!(
        "xtask check-deps: {} dependency-rule violation(s) found:",
        violations.len()
    );
    for violation in &violations {
        eprintln!("  - {violation}");
    }
    anyhow::bail!("dependency graph violates CODEBASE.md §3 (INV-2) — see violations above");
}
