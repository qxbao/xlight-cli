// SPDX-License-Identifier: GPL-3.0-only

//! Integration test for `xlightcli::wiring::build()` (Phase 0 deliverable, docs/PLAN.md §19):
//! all three providers must be registered with their expected transports.

use xlightcli_protocol::ProviderId;

#[tokio::test]
async fn all_three_providers_are_registered_with_at_least_one_transport() {
    let ctx = xlightcli::wiring::build().await;
    assert_eq!(ctx.providers.len(), 3, "expected codex + claude + agy");

    for id in ["codex", "claude", "agy"] {
        let provider = ctx
            .providers
            .get(&ProviderId::new(id))
            .unwrap_or_else(|| panic!("provider {id:?} not registered"));
        assert!(
            !provider.transports().is_empty(),
            "provider {id:?} has no transports"
        );
    }
}

#[tokio::test]
async fn provider_list_command_does_not_error() {
    let ctx = xlightcli::wiring::build().await;
    let result = xlightcli::cmd::provider::dispatch(&ctx, &xlightcli::cli::ProviderCommand::List);
    assert!(result.is_ok(), "{result:?}");
}
