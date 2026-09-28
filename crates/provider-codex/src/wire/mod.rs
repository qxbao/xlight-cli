// SPDX-License-Identifier: GPL-3.0-only

//! `pub(crate)` wire types + translator (PATTERNS.md §5). No wire type may appear in a `pub`
//! signature (INV-3): only `request`/`response` translate to/from `xlightcli_protocol` types.

pub(crate) mod request;
pub(crate) mod response;

#[cfg(test)]
mod fixture_tests;

use xlightcli_protocol::ProtocolVersion;

/// Wire/schema version this adapter is pinned to (docs/PLAN.md §15). A `response.completed`
/// shape that doesn't match what `response.rs` understands surfaces `ProtocolMismatch` rather
/// than a best-effort guess.
pub(crate) const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion(1);
