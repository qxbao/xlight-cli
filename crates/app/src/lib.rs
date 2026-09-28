// SPDX-License-Identifier: GPL-3.0-only

//! Library half of the `xlightcli` binary crate (CODEBASE.md §2): CLI surface, wiring, and
//! command implementations. `src/main.rs` is a thin wrapper around [`run`] — splitting it this
//! way lets `crates/app/tests/*.rs` exercise the CLI in-process instead of spawning a subprocess.
//!
//! **Status: Phase 0.** `dev probe`, `auth {list,login,logout,import}`, and `provider {list,info}`
//! are implemented; `init`/`exec`/`config` and the bare TUI (docs/PLAN.md §18.3) are later phases.

pub mod cli;
pub mod cmd;
pub mod logging;
pub mod login_ui;
pub mod output;
pub mod wiring;

use std::process::ExitCode;

use clap::Parser;
use cli::{Cli, Command, DevCommand};
use cmd::error::CliError;
use wiring::AppContext;

/// Entry point shared by `main.rs` and integration tests: parses argv, builds wiring, dispatches,
/// prints any error, and returns the process exit code (docs/PLAN.md §18.3 / Wave 2 brief): `0`
/// ok, `1` error, `2` invalid input, `3` error after partial output.
pub async fn run() -> ExitCode {
    let cli = Cli::parse();
    logging::init(cli.verbose);
    let ctx = wiring::build().await;
    run_with(&cli, &ctx).await
}

/// Same as [`run`], but with an already-built `Cli`/`AppContext` — the seam tests use to avoid
/// re-parsing argv / re-building wiring for every case.
pub async fn run_with(cli: &Cli, ctx: &AppContext) -> ExitCode {
    match execute(cli, ctx).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            output::error(&err.to_string());
            err.exit_code()
        }
    }
}

async fn execute(cli: &Cli, ctx: &AppContext) -> Result<(), CliError> {
    match &cli.command {
        Some(Command::Dev {
            command:
                DevCommand::Probe {
                    provider,
                    transport,
                    model,
                    prompt,
                },
        }) => {
            let cancel = tokio_util::sync::CancellationToken::new();
            let watcher = {
                let cancel = cancel.clone();
                tokio::spawn(async move {
                    if tokio::signal::ctrl_c().await.is_ok() {
                        cancel.cancel();
                    }
                })
            };
            let result = cmd::dev::probe(
                ctx,
                provider,
                transport.as_deref(),
                model.as_deref(),
                prompt,
                cancel,
            )
            .await;
            watcher.abort();
            result
        }
        Some(Command::Auth { command }) => cmd::auth::dispatch(ctx, command).await,
        Some(Command::Provider { command }) => cmd::provider::dispatch(ctx, command),
        None => Err(CliError::other(
            "xlightcli TUI is not implemented yet (Phase 1); try `xlightcli dev probe <provider> \
             \"<prompt>\"`",
        )),
    }
}
