// SPDX-License-Identifier: GPL-3.0-only

//! `TurnRequest` and its parts (docs/PLAN.md §4.3).

use serde::{Deserialize, Serialize};

use crate::ids::ModelId;
use crate::message::{ContentBlock, Message, Role};
use crate::tool::ToolDefinition;

/// Fully resolved system prompt text (project rules + base prompt already merged by
/// `runtime::ContextManager`). Transports never see raw rule files, only this string.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemPrompt {
    pub text: String,
}

impl SystemPrompt {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

/// Opaque per-transport request options (docs/PLAN.md §4.3). Core builds/forwards this map
/// without ever inspecting its contents; only the target transport interprets it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderOptions(serde_json::Map<String, serde_json::Value>);

impl ProviderOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.0.get(key)
    }

    pub fn insert(
        &mut self,
        key: impl Into<String>,
        value: serde_json::Value,
    ) -> Option<serde_json::Value> {
        self.0.insert(key.into(), value)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn as_map(&self) -> &serde_json::Map<String, serde_json::Value> {
        &self.0
    }
}

/// Canonical reasoning effort level; each transport maps this to its own wire field or model id
/// variant (agy encodes the tier in the model id — see `TransportAdapter::apply_effort`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Minimal,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReasoningConfig {
    pub effort: ReasoningEffort,
    /// Ask the transport to surface reasoning text when upstream supports it.
    pub include_text: bool,
}

/// A single request for one model turn. This is the only request shape the agent runtime
/// builds; provider adapters translate it to their wire body (INV-3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnRequest {
    pub model: ModelId,
    pub system: SystemPrompt,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
    #[serde(default)]
    pub reasoning: Option<ReasoningConfig>,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub provider_options: ProviderOptions,
}

impl TurnRequest {
    /// Minimal single-user-turn request, used by `dev probe` and the Phase 0 smoke test
    /// (docs/PLAN.md §19).
    pub fn simple(model: ModelId, text: impl Into<String>) -> Self {
        Self {
            model,
            system: SystemPrompt::default(),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text { text: text.into() }],
            }],
            tools: Vec::new(),
            reasoning: None,
            max_output_tokens: None,
            provider_options: ProviderOptions::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn simple_request_has_one_user_message() {
        let req = TurnRequest::simple(ModelId::new("gpt-5"), "Reply exactly with hello");
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, Role::User);
    }

    #[test]
    fn turn_request_roundtrips_through_json() {
        let mut req = TurnRequest::simple(ModelId::new("gpt-5"), "hi");
        req.provider_options
            .insert("verbosity", serde_json::json!("low"));
        req.reasoning = Some(ReasoningConfig {
            effort: ReasoningEffort::High,
            include_text: true,
        });
        let json = serde_json::to_string(&req).unwrap();
        let back: TurnRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, req);
    }
}
