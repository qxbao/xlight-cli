// SPDX-License-Identifier: GPL-3.0-only

//! Capability manifest types (docs/PLAN.md §4.4) and the `AuthKind` / `Stability` /
//! `ProtocolVersion` enums used by `provider::TransportAdapter`.

use serde::{Deserialize, Serialize};

use crate::ids::ModelId;

/// How a feature/command is implemented relative to the active provider (docs/PLAN.md §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityMode {
    /// Upstream genuinely exposes the capability; core calls the protocol directly.
    Native,
    /// Runtime implements it itself, provider-independent.
    Core,
    /// Exists in a provider CLI but is mostly client-side; xlightcli reimplements compatible
    /// behavior as a `runtime::recipes` recipe (D-024).
    Compatible,
    /// Not available with the current provider/transport; surfaced as `CommandResult::Unavailable`.
    Unsupported,
}

/// Whether a transport is safe to enable by default (D-002).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stability {
    Stable,
    Experimental,
}

/// What kind of credential a transport requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    Subscription,
    ApiKey,
}

/// Wire protocol/schema version pinned by a transport adapter. A response that doesn't match the
/// pinned version triggers `ProviderError::ProtocolMismatch` rather than a best-effort guess
/// (PATTERNS.md §5, docs/PLAN.md §15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProtocolVersion(pub u32);

/// Static capability manifest for one transport, rendered by `/provider info`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    pub reasoning: bool,
    pub images: bool,
    pub tool_calls: bool,
    pub parallel_tool_calls: bool,
    pub web_search: CapabilityMode,
    /// Always `Core` in v0.x (docs/PLAN.md §4.4): MCP is a core-runtime concern.
    pub mcp: CapabilityMode,
    pub session_resume: CapabilityMode,
    pub usage: CapabilityMode,
    pub quota: CapabilityMode,
    pub context_window: Option<u32>,
}

/// One model as listed by `TransportAdapter::list_models`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: ModelId,
    pub display_name: String,
    pub context_window: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub supports_reasoning: bool,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn capabilities_roundtrip_through_json() {
        let caps = ProviderCapabilities {
            reasoning: true,
            images: true,
            tool_calls: true,
            parallel_tool_calls: false,
            web_search: CapabilityMode::Native,
            mcp: CapabilityMode::Core,
            session_resume: CapabilityMode::Core,
            usage: CapabilityMode::Native,
            quota: CapabilityMode::Unsupported,
            context_window: Some(200_000),
        };
        let json = serde_json::to_string(&caps).unwrap();
        let back: ProviderCapabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(back, caps);
    }

    #[test]
    fn protocol_version_ordering() {
        assert!(ProtocolVersion(1) < ProtocolVersion(2));
    }
}
