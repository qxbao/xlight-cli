// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli-runtime` — provider-independent agent runtime: `Session`, `AgentLoop`,
//! `ContextManager`, `Scheduler`, `CommandRegistry` (CODEBASE.md §2, Phase 1+).
//!
//! **Status: empty skeleton (Phase 0).** This crate must never depend on any `provider-*` crate
//! and must never `match` on a `ProviderId` (INV-2, enforced by `cargo xtask check-deps`).
//! Provider differences are only visible through `ProviderCapabilities` and `provider_options`.
