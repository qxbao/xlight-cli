// SPDX-License-Identifier: GPL-3.0-only

//! Canonical event stream types (docs/PLAN.md §4.3, D-007, D-008).
//!
//! A transport's `stream()` yields `Result<AgentEvent, ProviderError>` (see `crate::error`).
//! Contract (PATTERNS.md §6): exactly one `TurnStarted` at the start and exactly one
//! `Completed` at the end on success; `Completed.message` is the full assistant message —
//! the runtime never reassembles it from deltas itself.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ids::{ModelId, ToolCallId};
use crate::message::Message;

/// Canonical event emitted by a transport while streaming a turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    TurnStarted {
        model: ModelId,
    },
    TextDelta {
        index: u32,
        text: String,
    },
    ReasoningDelta {
        index: u32,
        text: String,
    },
    /// UI-only signal that a tool call has started arriving; the authoritative `ToolUse` block
    /// is only present in `Completed.message`.
    ToolCallStarted {
        index: u32,
        id: ToolCallId,
        name: String,
    },
    /// May appear multiple times (incremental usage reporting from some transports).
    Usage(Usage),
    RateLimit(RateLimitInfo),
    Completed {
        message: Message,
        stop: StopReason,
        usage: Usage,
    },
}

/// Why a turn stopped.
///
/// Deliberately *not* internally tagged: `Other(String)` is a newtype variant, and serde's
/// internally-tagged representation requires newtype payloads to serialize as a map. The default
/// (externally tagged) representation handles both unit and newtype variants correctly, at the
/// cost of unit variants serializing as bare strings (`"end_turn"`) while `Other` serializes as
/// `{"other": "..."}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Refusal,
    Cancelled,
    Other(String),
}

/// Token accounting for one turn (or a running total, for incremental `Usage` events).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub reasoning_tokens: u64,
}

/// Rate-limit info surfaced from upstream headers/metadata, when available.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RateLimitInfo {
    pub limit: Option<u64>,
    pub remaining: Option<u64>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub reset_at: Option<OffsetDateTime>,
}

/// Plan/quota snapshot for `/usage` (D-027). A transport returns `Ok(None)` from
/// `TransportAdapter::quota()` when upstream exposes nothing at all.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QuotaSnapshot {
    pub plan: Option<String>,
    pub used_percent: Option<f32>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub resets_at: Option<OffsetDateTime>,
    /// Provider-specific extra detail (opaque to core, rendered as-is by `/usage --verbose`).
    #[serde(default)]
    pub detail: serde_json::Value,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::message::Role;

    #[test]
    fn agent_event_roundtrips_through_json() {
        let events = vec![
            AgentEvent::TurnStarted {
                model: ModelId::new("gpt-5"),
            },
            AgentEvent::TextDelta {
                index: 0,
                text: "hel".into(),
            },
            AgentEvent::Usage(Usage {
                input_tokens: 10,
                output_tokens: 2,
                ..Default::default()
            }),
            AgentEvent::Completed {
                message: Message {
                    role: Role::Assistant,
                    content: vec![],
                },
                stop: StopReason::EndTurn,
                usage: Usage::default(),
            },
        ];
        for ev in events {
            let json = serde_json::to_string(&ev).unwrap();
            let back: AgentEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(back, ev);
        }
    }

    #[test]
    fn stop_reason_other_roundtrips() {
        let sr = StopReason::Other("unknown_finish".into());
        let json = serde_json::to_string(&sr).unwrap();
        let back: StopReason = serde_json::from_str(&json).unwrap();
        assert_eq!(back, sr);
    }

    #[test]
    fn rate_limit_info_with_timestamp_roundtrips() {
        let info = RateLimitInfo {
            limit: Some(100),
            remaining: Some(42),
            reset_at: Some(OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap()),
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: RateLimitInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back, info);
    }
}
