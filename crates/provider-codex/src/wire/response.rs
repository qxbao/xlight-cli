// SPDX-License-Identifier: GPL-3.0-only

//! OpenAI Responses API SSE → `AgentEvent` translator: a pure state machine (PATTERNS.md §5),
//! `feed(&mut self, sse_event) -> Result<Vec<AgentEvent>, ProviderError>` /
//! `finish(self) -> Result<AgentEvent, ProviderError>`, tested against fixtures in
//! `tests/fixtures/*.sse` with `insta` snapshots.
//!
//! Event names/shapes (`response.created`, `response.output_item.added/done`,
//! `response.output_text.delta`, `response.reasoning_summary_text.delta`,
//! `response.function_call_arguments.delta/done`, `response.completed`, `response.failed`,
//! `error`) are the public Responses API streaming surface — **H**, not Codex-specific.
//! `codex.rate_limits` is **U** (docs/providers/codex.md): parsed leniently, never fails the turn.

use time::OffsetDateTime;
use xlightcli_protocol::{
    AgentEvent, ContentBlock, Message, ModelId, OpaqueBlob, ProviderError, ProviderId,
    RateLimitInfo, Role, StopReason, ToolCallId, TransportId, Usage,
};
use xlightcli_provider::sse::SseEvent;

use super::PROTOCOL_VERSION;

struct CompletedState {
    message: Message,
    stop: StopReason,
    usage: Usage,
}

/// Per-stream state machine. One instance per `TransportAdapter::stream()` call.
pub(crate) struct Translator {
    provider: ProviderId,
    transport: TransportId,
    fallback_model: ModelId,
    started: bool,
    completed: Option<CompletedState>,
}

impl Translator {
    pub(crate) fn new(
        provider: ProviderId,
        transport: TransportId,
        fallback_model: ModelId,
    ) -> Self {
        Self {
            provider,
            transport,
            fallback_model,
            started: false,
            completed: None,
        }
    }

    /// Feeds one parsed SSE event; returns zero or more canonical events to yield immediately.
    /// `response.completed` is *not* yielded here — it is only recorded, and surfaced once by
    /// `finish()` (PATTERNS.md §6: exactly one `Completed`, at the end).
    pub(crate) fn feed(&mut self, event: SseEvent) -> Result<Vec<AgentEvent>, ProviderError> {
        if event.data.is_empty() {
            return Ok(Vec::new());
        }
        let value: serde_json::Value =
            serde_json::from_str(&event.data).map_err(|e| ProviderError::ProtocolMismatch {
                expected: PROTOCOL_VERSION,
                detail: format!("invalid JSON in SSE event: {e}"),
            })?;
        let event_type = value
            .get("type")
            .and_then(|v| v.as_str())
            .or(event.event.as_deref())
            .unwrap_or("");
        match event_type {
            "response.created" => Ok(self.on_created(&value)),
            "response.output_item.added" => Ok(self.on_output_item_added(&value)),
            "response.output_text.delta" => Ok(self.on_text_delta(&value)),
            "response.reasoning_summary_text.delta" => Ok(self.on_reasoning_delta(&value)),
            "response.completed" => {
                self.on_completed(&value)?;
                Ok(Vec::new())
            }
            "response.failed" => Err(map_failed(&value)),
            "error" => Err(map_error(&value)),
            "codex.rate_limits" => Ok(self.on_rate_limits(&value)),
            other => {
                tracing::debug!(
                    event_type = other,
                    "codex wire: ignoring unrecognized SSE event"
                );
                Ok(Vec::new())
            }
        }
    }

    /// Called once the SSE stream ends. Errors if `response.completed` was never seen (dropped
    /// connection, etc.) rather than fabricating a `Completed` event.
    pub(crate) fn finish(self) -> Result<AgentEvent, ProviderError> {
        match self.completed {
            Some(state) => Ok(AgentEvent::Completed {
                message: state.message,
                stop: state.stop,
                usage: state.usage,
            }),
            None => Err(ProviderError::Network(
                "codex stream ended before response.completed".into(),
            )),
        }
    }

    fn on_created(&mut self, value: &serde_json::Value) -> Vec<AgentEvent> {
        if self.started {
            return Vec::new(); // defensive: contract is exactly one TurnStarted
        }
        self.started = true;
        let model = value
            .pointer("/response/model")
            .and_then(|v| v.as_str())
            .map(ModelId::new)
            .unwrap_or_else(|| self.fallback_model.clone());
        vec![AgentEvent::TurnStarted { model }]
    }

    fn on_output_item_added(&self, value: &serde_json::Value) -> Vec<AgentEvent> {
        let output_index = u32_field(value, "output_index");
        let Some(item) = value.get("item") else {
            return Vec::new();
        };
        if item.get("type").and_then(|v| v.as_str()) != Some("function_call") {
            return Vec::new();
        }
        let id = str_field(item, "call_id")
            .or_else(|| str_field(item, "id"))
            .unwrap_or_default();
        let name = str_field(item, "name").unwrap_or_default();
        vec![AgentEvent::ToolCallStarted {
            index: output_index,
            id: ToolCallId::new(id),
            name,
        }]
    }

    fn on_text_delta(&self, value: &serde_json::Value) -> Vec<AgentEvent> {
        vec![AgentEvent::TextDelta {
            index: u32_field(value, "output_index"),
            text: str_field(value, "delta").unwrap_or_default(),
        }]
    }

    fn on_reasoning_delta(&self, value: &serde_json::Value) -> Vec<AgentEvent> {
        vec![AgentEvent::ReasoningDelta {
            index: u32_field(value, "output_index"),
            text: str_field(value, "delta").unwrap_or_default(),
        }]
    }

    /// **U**: exact `codex.rate_limits` shape unverified; parsed leniently and never fatal
    /// (PATTERNS.md §5: harmless unknown shape → ignore + debug log, not `ProtocolMismatch`).
    fn on_rate_limits(&self, value: &serde_json::Value) -> Vec<AgentEvent> {
        let primary = value
            .pointer("/rate_limits/primary")
            .or_else(|| value.get("primary"));
        let Some(primary) = primary else {
            tracing::debug!("codex wire: codex.rate_limits with no recognizable primary window");
            return Vec::new();
        };
        let remaining = primary
            .get("used_percent")
            .and_then(|v| v.as_f64())
            .map(|used_percent| (100.0 - used_percent).max(0.0).round() as u64);
        let reset_at = primary
            .get("resets_in_seconds")
            .and_then(|v| v.as_i64())
            .map(|secs| OffsetDateTime::now_utc() + time::Duration::seconds(secs));
        vec![AgentEvent::RateLimit(RateLimitInfo {
            limit: Some(100),
            remaining,
            reset_at,
        })]
    }

    fn on_completed(&mut self, value: &serde_json::Value) -> Result<(), ProviderError> {
        let response = value
            .get("response")
            .ok_or_else(|| ProviderError::ProtocolMismatch {
                expected: PROTOCOL_VERSION,
                detail: "response.completed missing `response` object".into(),
            })?;
        let output = response
            .get("output")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut content = Vec::new();
        let mut has_function_call = false;
        let mut has_refusal = false;
        for item in &output {
            match item.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                "message" => self.push_message_content(item, &mut content, &mut has_refusal),
                "function_call" => {
                    has_function_call = true;
                    content.push(function_call_block(item));
                }
                "reasoning" => {
                    if let Some(block) = self.reasoning_block(item) {
                        content.push(block);
                    }
                }
                other => tracing::debug!(
                    item_type = other,
                    "codex wire: ignoring unrecognized output item"
                ),
            }
        }

        let status = str_field(response, "status").unwrap_or_else(|| "completed".to_string());
        let stop = stop_reason(&status, has_function_call, has_refusal, response);
        let usage = parse_usage(response.get("usage"));
        self.completed = Some(CompletedState {
            message: Message {
                role: Role::Assistant,
                content,
            },
            stop,
            usage,
        });
        Ok(())
    }

    fn push_message_content(
        &self,
        item: &serde_json::Value,
        content: &mut Vec<ContentBlock>,
        has_refusal: &mut bool,
    ) {
        for part in item
            .get("content")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            match part.get("type").and_then(|v| v.as_str()) {
                Some("output_text") => {
                    if let Some(text) = str_field(part, "text") {
                        content.push(ContentBlock::Text { text });
                    }
                }
                Some("refusal") => {
                    *has_refusal = true;
                    if let Some(text) = str_field(part, "refusal") {
                        content.push(ContentBlock::Text { text });
                    }
                }
                other => tracing::debug!(
                    part_type = other.unwrap_or("<missing>"),
                    "codex wire: ignoring unrecognized message content part"
                ),
            }
        }
    }

    fn reasoning_block(&self, item: &serde_json::Value) -> Option<ContentBlock> {
        let summary_value = item
            .get("summary")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([]));
        let summary_text: Vec<String> = summary_value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| str_field(p, "text"))
            .collect();
        let text = (!summary_text.is_empty()).then(|| summary_text.join("\n"));
        let encrypted_content = str_field(item, "encrypted_content");
        let opaque = encrypted_content.clone().map(|enc| OpaqueBlob {
            provider: self.provider.clone(),
            transport: self.transport.clone(),
            data: serde_json::json!({
                "id": str_field(item, "id"),
                "summary": summary_value,
                "encrypted_content": enc,
            }),
        });
        (text.is_some() || opaque.is_some()).then_some(ContentBlock::Reasoning { text, opaque })
    }
}

fn function_call_block(item: &serde_json::Value) -> ContentBlock {
    let call_id = str_field(item, "call_id")
        .or_else(|| str_field(item, "id"))
        .unwrap_or_default();
    let name = str_field(item, "name").unwrap_or_default();
    let arguments_raw = str_field(item, "arguments").unwrap_or_else(|| "{}".to_string());
    let input =
        serde_json::from_str(&arguments_raw).unwrap_or(serde_json::Value::String(arguments_raw));
    ContentBlock::ToolUse {
        id: ToolCallId::new(call_id),
        name,
        input,
        opaque: None,
    }
}

fn stop_reason(
    status: &str,
    has_function_call: bool,
    has_refusal: bool,
    response: &serde_json::Value,
) -> StopReason {
    if has_function_call {
        return StopReason::ToolUse;
    }
    match status {
        "incomplete" => {
            let reason = str_field(
                response
                    .pointer("/incomplete_details")
                    .unwrap_or(&serde_json::Value::Null),
                "reason",
            );
            match reason.as_deref() {
                Some("max_output_tokens") => StopReason::MaxTokens,
                Some(other) => StopReason::Other(other.to_string()),
                None => StopReason::Other("incomplete".to_string()),
            }
        }
        "completed" if has_refusal => StopReason::Refusal,
        "completed" => StopReason::EndTurn,
        "cancelled" => StopReason::Cancelled,
        other => StopReason::Other(other.to_string()),
    }
}

fn parse_usage(usage: Option<&serde_json::Value>) -> Usage {
    let Some(usage) = usage else {
        return Usage::default();
    };
    Usage {
        input_tokens: u64_field(usage, "input_tokens"),
        output_tokens: u64_field(usage, "output_tokens"),
        cached_input_tokens: usage
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        reasoning_tokens: usage
            .pointer("/output_tokens_details/reasoning_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
    }
}

fn map_failed(value: &serde_json::Value) -> ProviderError {
    let message = value
        .pointer("/response/error/message")
        .and_then(|v| v.as_str())
        .or_else(|| {
            value
                .pointer("/response/error/code")
                .and_then(|v| v.as_str())
        })
        .unwrap_or("response.failed with no error detail");
    ProviderError::Upstream {
        status: 0,
        body_excerpt: xlightcli_provider::error::body_excerpt(message),
    }
}

fn map_error(value: &serde_json::Value) -> ProviderError {
    let message = value
        .get("message")
        .and_then(|v| v.as_str())
        .or_else(|| value.pointer("/error/message").and_then(|v| v.as_str()))
        .unwrap_or("SSE error event with no message");
    ProviderError::Upstream {
        status: 0,
        body_excerpt: xlightcli_provider::error::body_excerpt(message),
    }
}

fn str_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

fn u32_field(value: &serde_json::Value, key: &str) -> u32 {
    value
        .get(key)
        .and_then(|v| v.as_u64())
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0)
}

fn u64_field(value: &serde_json::Value, key: &str) -> u64 {
    value.get(key).and_then(|v| v.as_u64()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    fn sse(event_type: &str, data: serde_json::Value) -> SseEvent {
        SseEvent {
            event: Some(event_type.to_string()),
            data: data.to_string(),
            id: None,
            retry: None,
        }
    }

    fn translator() -> Translator {
        Translator::new(
            ProviderId::new("codex"),
            TransportId::new("chatgpt"),
            ModelId::new("gpt-5-codex"),
        )
    }

    #[test]
    fn created_emits_exactly_one_turn_started() {
        let mut tr = translator();
        let events = tr
            .feed(sse(
                "response.created",
                serde_json::json!({"type": "response.created", "response": {"model": "gpt-5-codex-2"}}),
            ))
            .unwrap();
        assert_eq!(
            events,
            vec![AgentEvent::TurnStarted {
                model: ModelId::new("gpt-5-codex-2")
            }]
        );
        // A second `response.created` must not emit a second TurnStarted.
        let events2 = tr
            .feed(sse(
                "response.created",
                serde_json::json!({"type": "response.created", "response": {"model": "other"}}),
            ))
            .unwrap();
        assert!(events2.is_empty());
    }

    #[test]
    fn created_falls_back_to_constructor_model_when_absent() {
        let mut tr = translator();
        let events = tr
            .feed(sse(
                "response.created",
                serde_json::json!({"type": "response.created"}),
            ))
            .unwrap();
        assert_eq!(
            events,
            vec![AgentEvent::TurnStarted {
                model: ModelId::new("gpt-5-codex")
            }]
        );
    }

    #[test]
    fn text_delta_is_forwarded() {
        let mut tr = translator();
        let events = tr
            .feed(sse(
                "response.output_text.delta",
                serde_json::json!({"type": "response.output_text.delta", "output_index": 0, "delta": "hel"}),
            ))
            .unwrap();
        assert_eq!(
            events,
            vec![AgentEvent::TextDelta {
                index: 0,
                text: "hel".into()
            }]
        );
    }

    #[test]
    fn reasoning_delta_is_forwarded() {
        let mut tr = translator();
        let events = tr
            .feed(sse(
                "response.reasoning_summary_text.delta",
                serde_json::json!({"type": "response.reasoning_summary_text.delta", "output_index": 1, "delta": "thinking"}),
            ))
            .unwrap();
        assert_eq!(
            events,
            vec![AgentEvent::ReasoningDelta {
                index: 1,
                text: "thinking".into()
            }]
        );
    }

    #[test]
    fn function_call_added_emits_tool_call_started() {
        let mut tr = translator();
        let events = tr
            .feed(sse(
                "response.output_item.added",
                serde_json::json!({
                    "type": "response.output_item.added",
                    "output_index": 2,
                    "item": {"type": "function_call", "call_id": "call-1", "name": "read_file"}
                }),
            ))
            .unwrap();
        assert_eq!(
            events,
            vec![AgentEvent::ToolCallStarted {
                index: 2,
                id: ToolCallId::new("call-1"),
                name: "read_file".into()
            }]
        );
    }

    #[test]
    fn message_item_added_emits_nothing() {
        let mut tr = translator();
        let events = tr
            .feed(sse(
                "response.output_item.added",
                serde_json::json!({
                    "type": "response.output_item.added",
                    "output_index": 0,
                    "item": {"type": "message", "role": "assistant"}
                }),
            ))
            .unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn finish_without_completed_is_an_error() {
        let tr = translator();
        assert!(matches!(tr.finish(), Err(ProviderError::Network(_))));
    }

    #[test]
    fn plain_text_turn_completes_with_end_turn() {
        let mut tr = translator();
        tr.feed(sse(
            "response.completed",
            serde_json::json!({
                "type": "response.completed",
                "response": {
                    "status": "completed",
                    "output": [
                        {"type": "message", "role": "assistant", "content": [
                            {"type": "output_text", "text": "hello"}
                        ]}
                    ],
                    "usage": {"input_tokens": 10, "output_tokens": 2}
                }
            }),
        ))
        .unwrap();
        let completed = tr.finish().unwrap();
        match completed {
            AgentEvent::Completed {
                message,
                stop,
                usage,
            } => {
                assert_eq!(
                    message.content,
                    vec![ContentBlock::Text {
                        text: "hello".into()
                    }]
                );
                assert_eq!(stop, StopReason::EndTurn);
                assert_eq!(usage.input_tokens, 10);
                assert_eq!(usage.output_tokens, 2);
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn function_call_output_item_sets_tool_use_stop_reason() {
        let mut tr = translator();
        tr.feed(sse(
            "response.completed",
            serde_json::json!({
                "type": "response.completed",
                "response": {
                    "status": "completed",
                    "output": [
                        {"type": "function_call", "call_id": "call-1", "name": "read_file", "arguments": "{\"path\":\"a.rs\"}"}
                    ],
                    "usage": {"input_tokens": 5, "output_tokens": 1}
                }
            }),
        ))
        .unwrap();
        let completed = tr.finish().unwrap();
        match completed {
            AgentEvent::Completed { message, stop, .. } => {
                assert_eq!(stop, StopReason::ToolUse);
                assert_eq!(
                    message.content,
                    vec![ContentBlock::ToolUse {
                        id: ToolCallId::new("call-1"),
                        name: "read_file".into(),
                        input: serde_json::json!({"path": "a.rs"}),
                        opaque: None,
                    }]
                );
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn reasoning_item_with_encrypted_content_becomes_opaque_blob() {
        let mut tr = translator();
        tr.feed(sse(
            "response.completed",
            serde_json::json!({
                "type": "response.completed",
                "response": {
                    "status": "completed",
                    "output": [
                        {"type": "reasoning", "id": "rs_1", "summary": [{"type": "summary_text", "text": "thinking..."}], "encrypted_content": "enc-blob"},
                        {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "done"}]}
                    ],
                    "usage": {}
                }
            }),
        ))
        .unwrap();
        let completed = tr.finish().unwrap();
        match completed {
            AgentEvent::Completed { message, .. } => {
                assert_eq!(message.content.len(), 2);
                match &message.content[0] {
                    ContentBlock::Reasoning { text, opaque } => {
                        assert_eq!(text.as_deref(), Some("thinking..."));
                        let blob = opaque.as_ref().unwrap();
                        assert_eq!(blob.provider, ProviderId::new("codex"));
                        assert_eq!(blob.transport, TransportId::new("chatgpt"));
                        assert_eq!(blob.data.get("encrypted_content").unwrap(), "enc-blob");
                    }
                    other => panic!("expected Reasoning, got {other:?}"),
                }
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn incomplete_max_output_tokens_maps_to_max_tokens_stop() {
        let mut tr = translator();
        tr.feed(sse(
            "response.completed",
            serde_json::json!({
                "type": "response.completed",
                "response": {
                    "status": "incomplete",
                    "incomplete_details": {"reason": "max_output_tokens"},
                    "output": [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "trunc"}]}],
                    "usage": {}
                }
            }),
        ))
        .unwrap();
        match tr.finish().unwrap() {
            AgentEvent::Completed { stop, .. } => assert_eq!(stop, StopReason::MaxTokens),
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn response_completed_missing_response_object_is_protocol_mismatch() {
        let mut tr = translator();
        let err = tr
            .feed(sse(
                "response.completed",
                serde_json::json!({"type": "response.completed"}),
            ))
            .unwrap_err();
        assert!(matches!(err, ProviderError::ProtocolMismatch { .. }));
    }

    #[test]
    fn response_failed_surfaces_as_error() {
        let mut tr = translator();
        let err = tr
            .feed(sse(
                "response.failed",
                serde_json::json!({"type": "response.failed", "response": {"error": {"code": "server_error", "message": "boom"}}}),
            ))
            .unwrap_err();
        match err {
            ProviderError::Upstream { body_excerpt, .. } => assert!(body_excerpt.contains("boom")),
            other => panic!("expected Upstream, got {other:?}"),
        }
    }

    #[test]
    fn top_level_error_event_surfaces_as_error() {
        let mut tr = translator();
        let err = tr
            .feed(sse(
                "error",
                serde_json::json!({"type": "error", "message": "invalid request"}),
            ))
            .unwrap_err();
        match err {
            ProviderError::Upstream { body_excerpt, .. } => {
                assert!(body_excerpt.contains("invalid request"))
            }
            other => panic!("expected Upstream, got {other:?}"),
        }
    }

    #[test]
    fn unrecognized_event_type_is_ignored_harmlessly() {
        let mut tr = translator();
        let events = tr
            .feed(sse(
                "codex.something_new",
                serde_json::json!({"type": "codex.something_new"}),
            ))
            .unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn rate_limits_event_is_parsed_leniently() {
        let mut tr = translator();
        let events = tr
            .feed(sse(
                "codex.rate_limits",
                serde_json::json!({"type": "codex.rate_limits", "rate_limits": {"primary": {"used_percent": 25.0, "resets_in_seconds": 3600}}}),
            ))
            .unwrap();
        match &events[0] {
            AgentEvent::RateLimit(info) => assert_eq!(info.remaining, Some(75)),
            other => panic!("expected RateLimit, got {other:?}"),
        }
    }

    #[test]
    fn rate_limits_event_with_unrecognized_shape_is_ignored() {
        let mut tr = translator();
        let events = tr
            .feed(sse(
                "codex.rate_limits",
                serde_json::json!({"type": "codex.rate_limits"}),
            ))
            .unwrap();
        assert!(events.is_empty());
    }
}
