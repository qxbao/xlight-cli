// SPDX-License-Identifier: GPL-3.0-only

//! `xtask bench-mem` — memory/latency benchmark harness against a mock provider server
//! (docs/PLAN.md §17). Needs the Phase 1 TUI/session/mock-provider-server machinery
//! (`crates/runtime`, `crates/provider::testing::MockProvider` wired into a real binary run) that
//! doesn't exist yet in Phase 0 (CODEBASE.md §2: `runtime`/`tui` are still empty skeletons).
//!
//! **Status: stub.** Lands in Phase 1 once there's an actual session loop to measure.

// `xtask` is a dev-only CLI, not a library.
#[allow(clippy::print_stdout)]
pub fn run() -> anyhow::Result<()> {
    println!(
        "xtask bench-mem: not implemented yet — lands in Phase 1 (docs/PLAN.md §17), once \
         `runtime`/`tui` have an actual session loop and mock-provider-server harness to measure. \
         Phase 0 only wires the CLI shape; see CODEBASE.md §2 for current crate status."
    );
    Ok(())
}
