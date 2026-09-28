// SPDX-License-Identifier: GPL-3.0-only

//! `testing` feature: builds a [`crate::handle::RuntimeHandle`] wired to
//! `xlightcli_provider::testing::MockProvider` (docs/PLAN.md §16) instead of a real `provider-*`
//! crate — the only way `runtime`'s own tests (and later, `tui`'s) can exercise the handle without
//! violating INV-2 (`runtime` must never depend on a concrete `provider-*` crate).
//!
//! Storage is deliberately **not** opened here: this module has no opinion on temp-file vs. real
//! paths, so callers pass an already-open `Storage` (typically backed by a `tempfile::tempdir()`
//! in the caller's own test).

use std::sync::Arc;

use crate::handle::{RuntimeConfig, RuntimeDeps, RuntimeHandle};

/// Builds [`RuntimeDeps`] with an empty `ProviderRegistry` (callers register their own
/// `MockProvider` instances via [`register_mock_provider`]), a `testing`-feature `AuthBroker`, and
/// the full built-in tool registry.
pub fn mock_deps(storage: xlightcli_storage::Storage) -> RuntimeDeps {
    RuntimeDeps {
        providers: Arc::new(xlightcli_provider::ProviderRegistry::new()),
        auth: Arc::new(xlightcli_auth::AuthBroker::new()),
        tools: Arc::new(xlightcli_tools::ToolRegistry::with_builtins()),
        storage,
        config: xlightcli_config::Config::default(),
    }
}

/// Builds a [`RuntimeHandle`] over `deps` with default [`RuntimeConfig`] — the common case for a
/// test that doesn't need to tune the UI channel capacity.
pub fn mock_handle(deps: RuntimeDeps) -> RuntimeHandle {
    RuntimeHandle::new(deps, RuntimeConfig::default())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[tokio::test]
    async fn mock_handle_can_subscribe() {
        let dir = tempfile::tempdir().unwrap();
        let storage = xlightcli_storage::Storage::open(dir.path().join("test.db"))
            .await
            .unwrap();
        let handle = mock_handle(mock_deps(storage));
        assert!(handle.subscribe().await.is_ok());
    }
}
