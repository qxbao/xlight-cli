// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli` binary entry point. All logic lives in the library half (`src/lib.rs`,
//! CODEBASE.md §2) so `crates/app/tests/*.rs` can exercise it in-process.

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    xlightcli::run().await
}
