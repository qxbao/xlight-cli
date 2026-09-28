// SPDX-License-Identifier: GPL-3.0-only

//! `xtask` — dev tooling, not shipped (CODEBASE.md §2). Run via `cargo xtask <command>` (alias in
//! `.cargo/config.toml`).
//!
//! - `check-deps`: parses `cargo metadata` and rejects any dependency edge not in CODEBASE.md §3
//!   (enforces INV-2).
//! - `no-provider-cli`: runs the e2e suite with PATH-shimmed `codex`/`claude`/`agy`/`antigravity`
//!   binaries that write a marker file if invoked, then asserts the marker never appears
//!   (enforces INV-1; mandatory in CI per docs/PLAN.md §16).
//! - `bench-mem`: stub — lands in Phase 1 (docs/PLAN.md §17).
//! - `redact-fixture`: scrubs tokens/account ids/emails from a `.sse`/`.json` fixture before it
//!   can be committed (docs/PLAN.md §16, §6 fixture rule).

mod bench_mem;
mod check_deps;
mod no_provider_cli;
mod redact_fixture;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "xtask", about = "xlightcli dev tooling")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Enforces the crate dependency graph in CODEBASE.md §3 (INV-2).
    CheckDeps,
    /// Runs the e2e suite against PATH-shimmed provider CLIs and asserts they're never invoked
    /// (INV-1).
    NoProviderCli,
    /// Runs the memory benchmark scenarios against a mock provider server (docs/PLAN.md §17).
    BenchMem,
    /// Redacts secrets/account ids from a fixture file before it can be committed.
    RedactFixture {
        /// Path to the `.sse`/`.json` fixture to redact in place.
        file: PathBuf,
        /// Only check whether the file would change; exits non-zero without writing if so.
        #[arg(long)]
        check: bool,
    },
}

/// Workspace root: `xtask`'s own manifest dir is `<root>/xtask`, so its parent is `<root>`. Used
/// so `check-deps`/`no-provider-cli` resolve the same workspace regardless of the caller's cwd.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::CheckDeps => check_deps::run(&workspace_root()),
        Command::NoProviderCli => no_provider_cli::run(&workspace_root()),
        Command::BenchMem => bench_mem::run(),
        Command::RedactFixture { file, check } => redact_fixture::run(&file, check),
    }
}
