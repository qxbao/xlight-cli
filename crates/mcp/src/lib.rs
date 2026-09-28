// SPDX-License-Identifier: GPL-3.0-only

//! `xlightcli-mcp` — `McpManager`, `ConnectionPool`, `ToolBridge` (CODEBASE.md §2, Phase 3).
//!
//! **Status: empty skeleton (Phase 0).** MCP belongs to core (INV-8): one server is spawned once
//! and shared across agents through `McpManager`. Like `tools`, this crate does not depend on
//! `xlightcli-auth` — MCP credentials are a separate `CredentialBroker`, never the provider
//! credential (INV-4).
