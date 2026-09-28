// SPDX-License-Identifier: GPL-3.0-only

//! Integration test: `xlightcli dev probe <unknown-provider>` must fail with exit code 2
//! (invalid input), never attempt a network call (docs/PLAN.md §18.3 exit codes).
//!
//! Only needs `xlightcli::wiring::build()` to compile (see `provider_registration.rs` for the
//! same caveat: blocked on provider-codex/claude/agy implementing `Provider`), not for any
//! specific provider to actually be registered — "totally-not-a-real-provider" is unknown
//! regardless of what's wired up.

use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn unknown_provider_is_invalid_input() {
    let ctx = xlightcli::wiring::build().await;
    let err = xlightcli::cmd::dev::probe(
        &ctx,
        "totally-not-a-real-provider",
        None,
        None,
        "hello",
        CancellationToken::new(),
    )
    .await
    .expect_err("unknown provider must fail");
    assert_eq!(err.exit_code_number(), 2);
}
