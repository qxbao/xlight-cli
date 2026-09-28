// SPDX-License-Identifier: GPL-3.0-only

//! Anthropic Messages SSE → `AgentEvent` translator (PATTERNS.md §5/§6): a pure state machine,
//! no IO, fed one already-parsed `SseEvent` (from `xlightcli_provider::sse::parse`) at a time.
//! Shared by `transport_api` and `transport_subscription` — both use the same wire shape, only
//! auth/headers differ.
//!
//! Port notes (docs/PLAN.md §4.5): the event set and terminal-frame handling mirror OpenCodex
//! (MIT) @ 3cc34e1181926b64331490fdcfee162ffb62fe73, `src/adapters/anthropic.ts`
//! (`parseStream`) — reimplemented from scratch against the canonical `protocol` types, not
//! copied verbatim (that file mixes in unrelated proxy-only concerns we deliberately drop, e.g.
//! prompt-cache breakpoints, tiered "fast mode").

use std::collections::BTreeMap;

use serde_json::Value;
use xlightcli_protocol::{
    AgentEvent, ContentBlock, Message, OpaqueBlob, ProtocolVersion, ProviderError, ProviderId,
    Role, StopReason, ToolCallId, TransportId, Usage,
};
use xlightcli_provider::SseEvent;

/// Wire protocol version this translator is pinned to (docs/PLAN.md §15). A response shape this
/// translator doesn't understand surfaces `ProviderError::ProtocolMismatch { expected, .. }`
/// instead of a best-effort guess.
pub(crate) const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion(1);

#[derive(Debug, Clone)]
enum BlockState {
    Text {
        text: String,
    },
    Thinking {
        text: String,
        signature: Option<String>,
    },
    RedactedThinking {
        data: String,
    },
    ToolUse {
        id: ToolCallId,
        name: String,
        partial_json: String,
    },
}

/// Pure state machine translating one Messages API SSE stream into canonical `AgentEvent`s.
/// `feed` is called once per parsed `SseEvent`; `finish` is called exactly once, after the
/// upstream SSE stream ends, to produce the terminal `AgentEvent::Completed`.
#[derive(Debug)]
pub(crate) struct Translator {
    provider: ProviderId,
    transport: TransportId,
    /// Mirrors `wire::request::RequestOptions::oauth_mode`: when `true`, strips the `custom_`
    /// tool-name prefix `wire::request` added on the way out (`consts::OAUTH_TOOL_NAME_PREFIX`) so
    /// `Completed.message`'s `tool_use.name` matches the original `ToolDefinition::name` the
    /// caller sent, not the OAuth-only wire-safe name.
    oauth_mode: bool,
    blocks: BTreeMap<u32, BlockState>,
    stop_reason: Option<String>,
    usage: Usage,
    got_message_stop: bool,
}

/// Inverse of `wire::request::tool_name_to_wire` — strips `consts::OAUTH_TOOL_NAME_PREFIX` unless
/// the name is one of Anthropic's own builtins (which are never prefixed on the way out either).
fn tool_name_from_wire(name: &str, oauth_mode: bool) -> String {
    if !oauth_mode {
        return name.to_string();
    }
    let lower = name.to_ascii_lowercase();
    if crate::consts::OAUTH_BUILTIN_TOOL_NAMES.contains(&lower.as_str()) {
        return name.to_string();
    }
    name.strip_prefix(crate::consts::OAUTH_TOOL_NAME_PREFIX)
        .unwrap_or(name)
        .to_string()
}

fn protocol_mismatch(detail: impl Into<String>) -> ProviderError {
    ProviderError::ProtocolMismatch {
        expected: PROTOCOL_VERSION,
        detail: detail.into(),
    }
}

fn json_u32(value: &Value, field: &str) -> Result<u32, ProviderError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| protocol_mismatch(format!("missing/invalid integer field `{field}`")))
}

fn map_anthropic_error(error: &Value) -> ProviderError {
    let kind = error.get("type").and_then(Value::as_str).unwrap_or("");
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("anthropic error event")
        .to_string();
    match kind {
        "authentication_error" | "permission_error" => {
            ProviderError::Auth(xlightcli_protocol::AuthFailure::Rejected)
        }
        "rate_limit_error" => ProviderError::RateLimited {
            retry_after: None,
            info: xlightcli_protocol::RateLimitInfo::default(),
        },
        "overloaded_error" => ProviderError::Upstream {
            status: 529,
            body_excerpt: xlightcli_provider::body_excerpt(&message),
        },
        _ => ProviderError::Upstream {
            status: 500,
            body_excerpt: xlightcli_provider::body_excerpt(&message),
        },
    }
}

fn map_usage(usage: &Value, into: &mut Usage) {
    if let Some(v) = usage.get("input_tokens").and_then(Value::as_u64) {
        into.input_tokens = v;
    }
    if let Some(v) = usage.get("output_tokens").and_then(Value::as_u64) {
        into.output_tokens = v;
    }
    let cache_read = usage
        .get("cache_read_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if cache_read > 0 {
        into.cached_input_tokens = cache_read;
    }
    // `reasoning_tokens` has no direct Anthropic counterpart (thinking tokens are billed as
    // regular output tokens) — left at 0 rather than guessed.
}

fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "end_turn" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "refusal" | "content_filter" => StopReason::Refusal,
        // `pause_turn` (long-running server tool turns) and `stop_sequence` are real, distinct
        // outcomes with no matching canonical variant — preserve the wire string rather than
        // collapsing them into `EndTurn` (PATTERNS.md §5: don't guess).
        other => StopReason::Other(other.to_string()),
    }
}

impl Translator {
    pub(crate) fn new(provider: ProviderId, transport: TransportId, oauth_mode: bool) -> Self {
        Self {
            provider,
            transport,
            oauth_mode,
            blocks: BTreeMap::new(),
            stop_reason: None,
            usage: Usage::default(),
            got_message_stop: false,
        }
    }

    /// Feeds one parsed SSE event, returning zero or more canonical events to yield immediately.
    pub(crate) fn feed(&mut self, event: SseEvent) -> Result<Vec<AgentEvent>, ProviderError> {
        if event.data.trim().is_empty() {
            return Ok(Vec::new());
        }
        let data: Value = serde_json::from_str(&event.data)
            .map_err(|e| protocol_mismatch(format!("non-JSON SSE data: {e}")))?;
        let event_type = event
            .event
            .as_deref()
            .or_else(|| data.get("type").and_then(Value::as_str))
            .unwrap_or_default();

        match event_type {
            "message_start" => {
                if let Some(usage) = data.get("message").and_then(|m| m.get("usage")) {
                    map_usage(usage, &mut self.usage);
                }
                Ok(vec![AgentEvent::Usage(self.usage)])
            }
            "content_block_start" => self.on_content_block_start(&data),
            "content_block_delta" => self.on_content_block_delta(&data),
            "content_block_stop" => self.on_content_block_stop(&data),
            "message_delta" => {
                if let Some(usage) = data.get("usage") {
                    map_usage(usage, &mut self.usage);
                }
                if let Some(reason) = data
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(Value::as_str)
                {
                    self.stop_reason = Some(reason.to_string());
                }
                Ok(vec![AgentEvent::Usage(self.usage)])
            }
            "message_stop" => {
                self.got_message_stop = true;
                Ok(Vec::new())
            }
            "ping" => Ok(Vec::new()),
            "error" => {
                let error = data.get("error").cloned().unwrap_or(Value::Null);
                Err(map_anthropic_error(&error))
            }
            other => {
                tracing::debug!(
                    event = other,
                    "anthropic wire: ignoring unknown SSE event type"
                );
                Ok(Vec::new())
            }
        }
    }

    fn on_content_block_start(&mut self, data: &Value) -> Result<Vec<AgentEvent>, ProviderError> {
        let index = json_u32(data, "index")?;
        let block = data
            .get("content_block")
            .ok_or_else(|| protocol_mismatch("content_block_start missing `content_block`"))?;
        let block_type = block
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| protocol_mismatch("content_block missing `type`"))?;
        match block_type {
            "text" => {
                self.blocks.insert(
                    index,
                    BlockState::Text {
                        text: String::new(),
                    },
                );
                Ok(Vec::new())
            }
            "thinking" => {
                let seed = block.get("thinking").and_then(Value::as_str).unwrap_or("");
                self.blocks.insert(
                    index,
                    BlockState::Thinking {
                        text: seed.to_string(),
                        signature: None,
                    },
                );
                Ok(Vec::new())
            }
            "redacted_thinking" => {
                let data_str = block
                    .get("data")
                    .and_then(Value::as_str)
                    .ok_or_else(|| protocol_mismatch("redacted_thinking block missing `data`"))?;
                self.blocks.insert(
                    index,
                    BlockState::RedactedThinking {
                        data: data_str.to_string(),
                    },
                );
                Ok(Vec::new())
            }
            "tool_use" => {
                let id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| protocol_mismatch("tool_use block missing `id`"))?;
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| protocol_mismatch("tool_use block missing `name`"))?;
                let name = tool_name_from_wire(name, self.oauth_mode);
                self.blocks.insert(
                    index,
                    BlockState::ToolUse {
                        id: ToolCallId::new(id),
                        name: name.clone(),
                        partial_json: String::new(),
                    },
                );
                Ok(vec![AgentEvent::ToolCallStarted {
                    index,
                    id: ToolCallId::new(id),
                    name,
                }])
            }
            other => Err(protocol_mismatch(format!(
                "unknown content_block type `{other}`"
            ))),
        }
    }

    fn on_content_block_delta(&mut self, data: &Value) -> Result<Vec<AgentEvent>, ProviderError> {
        let index = json_u32(data, "index")?;
        let delta = data
            .get("delta")
            .ok_or_else(|| protocol_mismatch("content_block_delta missing `delta`"))?;
        let delta_type = delta.get("type").and_then(Value::as_str).unwrap_or("");
        match delta_type {
            "text_delta" => {
                let text = delta.get("text").and_then(Value::as_str).unwrap_or("");
                if let Some(BlockState::Text { text: buf }) = self.blocks.get_mut(&index) {
                    buf.push_str(text);
                }
                Ok(vec![AgentEvent::TextDelta {
                    index,
                    text: text.to_string(),
                }])
            }
            "thinking_delta" => {
                let text = delta.get("thinking").and_then(Value::as_str).unwrap_or("");
                if let Some(BlockState::Thinking { text: buf, .. }) = self.blocks.get_mut(&index) {
                    buf.push_str(text);
                }
                Ok(vec![AgentEvent::ReasoningDelta {
                    index,
                    text: text.to_string(),
                }])
            }
            "signature_delta" => {
                let signature = delta.get("signature").and_then(Value::as_str).unwrap_or("");
                if let Some(BlockState::Thinking { signature: sig, .. }) =
                    self.blocks.get_mut(&index)
                {
                    *sig = Some(signature.to_string());
                }
                // No canonical event: the signature is continuity data folded into
                // `Completed.message`'s `OpaqueBlob`, not a user-visible delta.
                Ok(Vec::new())
            }
            "input_json_delta" => {
                let partial = delta
                    .get("partial_json")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if let Some(BlockState::ToolUse { partial_json, .. }) = self.blocks.get_mut(&index)
                {
                    partial_json.push_str(partial);
                }
                // No canonical "tool input delta" event in Phase 0 (docs/PLAN.md §4.3); the
                // assembled `input` only appears once, in `Completed.message`.
                Ok(Vec::new())
            }
            other => {
                tracing::debug!(
                    delta_type = other,
                    "anthropic wire: ignoring unknown delta type"
                );
                Ok(Vec::new())
            }
        }
    }

    fn on_content_block_stop(&mut self, data: &Value) -> Result<Vec<AgentEvent>, ProviderError> {
        let index = json_u32(data, "index")?;
        if let Some(BlockState::ToolUse { partial_json, .. }) = self.blocks.get(&index)
            && !partial_json.trim().is_empty()
            && serde_json::from_str::<Value>(partial_json).is_err()
        {
            return Err(protocol_mismatch(format!(
                "tool_use block {index} closed with unparseable input JSON"
            )));
        }
        Ok(Vec::new())
    }

    /// Consumes the translator once the SSE stream ends, producing the terminal
    /// `AgentEvent::Completed` (PATTERNS.md §6: exactly one, with the full assistant message).
    pub(crate) fn finish(self) -> Result<AgentEvent, ProviderError> {
        if !self.got_message_stop {
            return Err(ProviderError::Upstream {
                status: 0,
                body_excerpt: "anthropic stream ended before `message_stop` (possible truncation)"
                    .to_string(),
            });
        }
        let mut content = Vec::with_capacity(self.blocks.len());
        for (_, block) in self.blocks {
            let converted = match block {
                BlockState::Text { text } => ContentBlock::Text { text },
                BlockState::Thinking { text, signature } => ContentBlock::Reasoning {
                    text: if text.is_empty() { None } else { Some(text) },
                    opaque: signature.map(|signature| OpaqueBlob {
                        provider: self.provider.clone(),
                        transport: self.transport.clone(),
                        data: serde_json::json!({"signature": signature}),
                    }),
                },
                BlockState::RedactedThinking { data } => ContentBlock::Reasoning {
                    text: None,
                    opaque: Some(OpaqueBlob {
                        provider: self.provider.clone(),
                        transport: self.transport.clone(),
                        data: serde_json::json!({"redacted_thinking": data}),
                    }),
                },
                BlockState::ToolUse {
                    id,
                    name,
                    partial_json,
                } => {
                    let input = if partial_json.trim().is_empty() {
                        Value::Object(Default::default())
                    } else {
                        serde_json::from_str(&partial_json).map_err(|e| {
                            protocol_mismatch(format!("tool_use input failed to parse: {e}"))
                        })?
                    };
                    ContentBlock::ToolUse {
                        id,
                        name,
                        input,
                        opaque: None,
                    }
                }
            };
            content.push(converted);
        }
        let stop = self
            .stop_reason
            .as_deref()
            .map(map_stop_reason)
            .unwrap_or_else(|| StopReason::Other("missing_stop_reason".to_string()));
        Ok(AgentEvent::Completed {
            message: Message {
                role: Role::Assistant,
                content,
            },
            stop,
            usage: self.usage,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    fn sse(event: &str, data: Value) -> SseEvent {
        SseEvent {
            event: Some(event.to_string()),
            data: data.to_string(),
            id: None,
            retry: None,
        }
    }

    fn translator() -> Translator {
        Translator::new(
            ProviderId::new("claude"),
            TransportId::new("anthropic-api"),
            false,
        )
    }

    #[test]
    fn text_only_turn_produces_expected_events_and_completion() {
        let mut tr = translator();
        tr.feed(sse(
            "message_start",
            serde_json::json!({"type": "message_start", "message": {"usage": {"input_tokens": 10, "output_tokens": 0}}}),
        ))
        .unwrap();
        tr.feed(sse(
            "content_block_start",
            serde_json::json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        ))
        .unwrap();
        let events = tr
            .feed(sse(
                "content_block_delta",
                serde_json::json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "hi"}}),
            ))
            .unwrap();
        assert_eq!(
            events,
            vec![AgentEvent::TextDelta {
                index: 0,
                text: "hi".into()
            }]
        );
        tr.feed(sse(
            "content_block_stop",
            serde_json::json!({"type": "content_block_stop", "index": 0}),
        ))
        .unwrap();
        tr.feed(sse(
            "message_delta",
            serde_json::json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 2}}),
        ))
        .unwrap();
        tr.feed(sse(
            "message_stop",
            serde_json::json!({"type": "message_stop"}),
        ))
        .unwrap();

        let completed = tr.finish().unwrap();
        match completed {
            AgentEvent::Completed {
                message,
                stop,
                usage,
            } => {
                assert_eq!(stop, StopReason::EndTurn);
                assert_eq!(usage.input_tokens, 10);
                assert_eq!(usage.output_tokens, 2);
                assert_eq!(
                    message.content,
                    vec![ContentBlock::Text { text: "hi".into() }]
                );
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn thinking_block_with_signature_becomes_reasoning_with_opaque_blob() {
        let mut tr = translator();
        tr.feed(sse(
            "content_block_start",
            serde_json::json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
        ))
        .unwrap();
        tr.feed(sse(
            "content_block_delta",
            serde_json::json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "because..."}}),
        ))
        .unwrap();
        tr.feed(sse(
            "content_block_delta",
            serde_json::json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig-1"}}),
        ))
        .unwrap();
        tr.feed(sse(
            "content_block_stop",
            serde_json::json!({"type": "content_block_stop", "index": 0}),
        ))
        .unwrap();
        tr.feed(sse(
            "message_delta",
            serde_json::json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}),
        ))
        .unwrap();
        tr.feed(sse(
            "message_stop",
            serde_json::json!({"type": "message_stop"}),
        ))
        .unwrap();
        let AgentEvent::Completed { message, .. } = tr.finish().unwrap() else {
            panic!("expected Completed");
        };
        match &message.content[0] {
            ContentBlock::Reasoning { text, opaque } => {
                assert_eq!(text.as_deref(), Some("because..."));
                let blob = opaque.as_ref().unwrap();
                assert_eq!(blob.data["signature"], serde_json::json!("sig-1"));
            }
            other => panic!("expected Reasoning, got {other:?}"),
        }
    }

    #[test]
    fn chunked_tool_use_input_json_assembles_correctly() {
        let mut tr = translator();
        let events = tr
            .feed(sse(
                "content_block_start",
                serde_json::json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "call-1", "name": "read_file"}}),
            ))
            .unwrap();
        assert_eq!(
            events,
            vec![AgentEvent::ToolCallStarted {
                index: 0,
                id: ToolCallId::new("call-1"),
                name: "read_file".into()
            }]
        );
        for chunk in ["{\"path\":", "\"a.rs\"}"] {
            tr.feed(sse(
                "content_block_delta",
                serde_json::json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": chunk}}),
            ))
            .unwrap();
        }
        tr.feed(sse(
            "content_block_stop",
            serde_json::json!({"type": "content_block_stop", "index": 0}),
        ))
        .unwrap();
        tr.feed(sse(
            "message_delta",
            serde_json::json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}}),
        ))
        .unwrap();
        tr.feed(sse(
            "message_stop",
            serde_json::json!({"type": "message_stop"}),
        ))
        .unwrap();
        let AgentEvent::Completed { message, stop, .. } = tr.finish().unwrap() else {
            panic!("expected Completed");
        };
        assert_eq!(stop, StopReason::ToolUse);
        match &message.content[0] {
            ContentBlock::ToolUse { name, input, .. } => {
                assert_eq!(name, "read_file");
                assert_eq!(input["path"], serde_json::json!("a.rs"));
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn oauth_mode_strips_the_custom_tool_name_prefix_on_the_way_back() {
        let mut tr = Translator::new(
            ProviderId::new("claude"),
            TransportId::new("claude-subscription"),
            true,
        );
        let events = tr
            .feed(sse(
                "content_block_start",
                serde_json::json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "call-1", "name": "custom_read_file"}}),
            ))
            .unwrap();
        assert_eq!(
            events,
            vec![AgentEvent::ToolCallStarted {
                index: 0,
                id: ToolCallId::new("call-1"),
                name: "read_file".into(),
            }]
        );
    }

    #[test]
    fn oauth_mode_leaves_builtin_tool_names_untouched() {
        let mut tr = Translator::new(
            ProviderId::new("claude"),
            TransportId::new("claude-subscription"),
            true,
        );
        let events = tr
            .feed(sse(
                "content_block_start",
                serde_json::json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "call-1", "name": "web_search"}}),
            ))
            .unwrap();
        assert_eq!(
            events,
            vec![AgentEvent::ToolCallStarted {
                index: 0,
                id: ToolCallId::new("call-1"),
                name: "web_search".into(),
            }]
        );
    }

    #[test]
    fn mid_stream_error_event_is_propagated() {
        let mut tr = translator();
        let err = tr
            .feed(sse(
                "error",
                serde_json::json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
            ))
            .unwrap_err();
        assert!(matches!(err, ProviderError::Upstream { status: 529, .. }));
    }

    #[test]
    fn stream_ending_without_message_stop_is_an_upstream_error() {
        let mut tr = translator();
        tr.feed(sse(
            "content_block_start",
            serde_json::json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        ))
        .unwrap();
        let err = tr.finish().unwrap_err();
        assert!(matches!(err, ProviderError::Upstream { status: 0, .. }));
    }

    #[test]
    fn unknown_content_block_type_is_a_protocol_mismatch() {
        let mut tr = translator();
        let err = tr
            .feed(sse(
                "content_block_start",
                serde_json::json!({"type": "content_block_start", "index": 0, "content_block": {"type": "totally_new_block"}}),
            ))
            .unwrap_err();
        assert!(matches!(err, ProviderError::ProtocolMismatch { .. }));
    }

    #[test]
    fn max_tokens_stop_reason_maps_correctly() {
        let mut tr = translator();
        tr.feed(sse(
            "message_delta",
            serde_json::json!({"type": "message_delta", "delta": {"stop_reason": "max_tokens"}}),
        ))
        .unwrap();
        tr.feed(sse(
            "message_stop",
            serde_json::json!({"type": "message_stop"}),
        ))
        .unwrap();
        let AgentEvent::Completed { stop, .. } = tr.finish().unwrap() else {
            panic!("expected Completed");
        };
        assert_eq!(stop, StopReason::MaxTokens);
    }

    /// Feeds every SSE event in `tests/fixtures/<name>.sse` through a fresh `Translator` and
    /// returns the full ordered `AgentEvent` sequence, including the terminal event
    /// (`Completed` on success, or the propagated error — asserted by the caller). Fixture files
    /// live in `tests/fixtures` (not `src/`) per PATTERNS.md §5's crate layout, but are read from
    /// here (rather than an integration test under `tests/`) because `Translator` is
    /// `pub(crate)` and an integration test crate can't see it (INV-3: no wire type is `pub`).
    async fn run_fixture(name: &str) -> Result<Vec<AgentEvent>, ProviderError> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(format!("{name}.sse"));
        let sse_events = xlightcli_provider::testing::parse_sse_fixture(&path)
            .await
            .unwrap_or_else(|e| panic!("reading fixture {name}: {e}"));
        let mut tr = translator();
        let mut out = Vec::new();
        for event in sse_events {
            out.extend(tr.feed(event)?);
        }
        out.push(tr.finish()?);
        Ok(out)
    }

    #[tokio::test]
    async fn fixture_text_snapshot() {
        let events = run_fixture("text").await.unwrap();
        insta::assert_yaml_snapshot!(events);
    }

    #[tokio::test]
    async fn fixture_thinking_with_signature_snapshot() {
        let events = run_fixture("thinking_with_signature").await.unwrap();
        insta::assert_yaml_snapshot!(events);
    }

    #[tokio::test]
    async fn fixture_tool_use_chunked_snapshot() {
        let events = run_fixture("tool_use_chunked").await.unwrap();
        insta::assert_yaml_snapshot!(events);
    }

    #[tokio::test]
    async fn fixture_parallel_tool_use_snapshot() {
        let events = run_fixture("parallel_tool_use").await.unwrap();
        insta::assert_yaml_snapshot!(events);
    }

    #[tokio::test]
    async fn fixture_max_tokens_snapshot() {
        let events = run_fixture("max_tokens").await.unwrap();
        insta::assert_yaml_snapshot!(events);
    }

    #[tokio::test]
    async fn fixture_error_mid_stream_surfaces_as_upstream_error() {
        let err = run_fixture("error_mid_stream").await.unwrap_err();
        assert!(matches!(err, ProviderError::Upstream { status: 529, .. }));
    }
}
