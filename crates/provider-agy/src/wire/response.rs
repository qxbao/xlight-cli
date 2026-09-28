// SPDX-License-Identifier: GPL-3.0-only
// Portions derived from OpenCodex (MIT) @ 3cc34e1181926b64331490fdcfee162ffb62fe73:
// src/adapters/google.ts (parseStream)
// See THIRD_PARTY.md.

//! Gemini / Cloud Code Assist SSE → `AgentEvent` translator (Wave 2): pure state machine
//! (`feed`/`finish`, PATTERNS.md §5), tested against `tests/fixtures/*.sse` with `insta`
//! snapshots.
//!
//! Ported (structure, not code) from OpenCodex (MIT) `@ 3cc34e1181926b64331490fdcfee162ffb62fe73`
//! `src/adapters/google.ts` `parseStream`: the Cloud Code Assist wrapper shape (`{"response": {…}}`
//! per SSE data frame vs. the plain Gemini API shape), `finishReason` → stop-reason mapping, and
//! `usageMetadata` field names. See `THIRD_PARTY.md`.

use serde_json::Value;
use xlightcli_protocol::{
    AgentEvent, AuthFailure, ContentBlock, ImageSource, Message, OpaqueBlob, ProtocolVersion,
    ProviderError, ProviderId, RateLimitInfo, Role, StopReason, ToolCallId, TransportId, Usage,
};

/// In-progress content block being accumulated across one or more SSE frames, before it closes
/// into a final `ContentBlock` (PATTERNS.md §6: `Completed.message` is the full message, not
/// deltas — the runtime never reassembles it itself).
enum OpenBlock {
    Text {
        index: u32,
        buf: String,
    },
    Reasoning {
        index: u32,
        buf: String,
        signature: Option<String>,
    },
}

/// Pure state machine translating Gemini/Cloud-Code-Assist SSE data frames into `AgentEvent`s. No
/// IO: `feed`/`finish` only. One `Translator` per `stream()` call.
pub(crate) struct Translator {
    provider: ProviderId,
    transport: TransportId,
    /// `true` for `antigravity` (Cloud Code Assist wraps the Gemini payload under `response`),
    /// `false` for `gemini-api` (plain Gemini API shape at the frame root).
    wrapped: bool,
    protocol_version: ProtocolVersion,
    next_index: u32,
    content: Vec<ContentBlock>,
    open: Option<OpenBlock>,
    usage: Usage,
    finish_reason: Option<String>,
    blocked_reason: Option<String>,
    saw_any_frame: bool,
    tool_call_seq: u32,
}

impl Translator {
    pub(crate) fn new(
        provider: ProviderId,
        transport: TransportId,
        wrapped: bool,
        protocol_version: ProtocolVersion,
    ) -> Self {
        Self {
            provider,
            transport,
            wrapped,
            protocol_version,
            next_index: 0,
            content: Vec::new(),
            open: None,
            usage: Usage::default(),
            finish_reason: None,
            blocked_reason: None,
            saw_any_frame: false,
            tool_call_seq: 0,
        }
    }

    fn mismatch(&self, detail: impl Into<String>) -> ProviderError {
        ProviderError::ProtocolMismatch {
            expected: self.protocol_version,
            detail: detail.into(),
        }
    }

    fn alloc_index(&mut self) -> u32 {
        let index = self.next_index;
        self.next_index += 1;
        index
    }

    fn close_open_block(&mut self) {
        match self.open.take() {
            Some(OpenBlock::Text { buf, .. }) => {
                self.content.push(ContentBlock::Text { text: buf })
            }
            Some(OpenBlock::Reasoning { buf, signature, .. }) => {
                let opaque = signature.map(|sig| OpaqueBlob {
                    provider: self.provider.clone(),
                    transport: self.transport.clone(),
                    data: serde_json::json!({ "thoughtSignature": sig }),
                });
                self.content.push(ContentBlock::Reasoning {
                    text: if buf.is_empty() { None } else { Some(buf) },
                    opaque,
                });
            }
            None => {}
        }
    }

    /// Feeds one already-parsed SSE `data:` payload. Returns the `AgentEvent`s it produced (zero
    /// or more); errors (inline upstream error object, or a shape this translator can't safely
    /// interpret) short-circuit as `Err` rather than being guessed at (PATTERNS.md §5).
    pub(crate) fn feed(
        &mut self,
        event: xlightcli_provider::SseEvent,
    ) -> Result<Vec<AgentEvent>, ProviderError> {
        let data = event.data.trim();
        if data.is_empty() {
            return Ok(Vec::new());
        }
        let value: Value = serde_json::from_str(data)
            .map_err(|e| self.mismatch(format!("invalid JSON in SSE data frame: {e}")))?;

        if let Some(err_obj) = value.get("error") {
            return Err(map_inline_error(err_obj));
        }

        let root: &Value = if self.wrapped {
            value.get("response").ok_or_else(|| {
                self.mismatch("cloud-code-assist frame missing `response` wrapper")
            })?
        } else {
            &value
        };
        self.saw_any_frame = true;

        let mut events = Vec::new();
        if let Some(usage_meta) = root.get("usageMetadata").and_then(Value::as_object) {
            self.usage = usage_from_gemini(usage_meta);
            events.push(AgentEvent::Usage(self.usage));
        }
        if let Some(reason) = root
            .get("promptFeedback")
            .and_then(|v| v.get("blockReason"))
            .and_then(Value::as_str)
        {
            self.blocked_reason = Some(reason.to_string());
        }

        let candidates = match root.get("candidates") {
            None | Some(Value::Null) => return Ok(events),
            Some(Value::Array(arr)) if arr.is_empty() => return Ok(events),
            Some(Value::Array(arr)) => arr,
            Some(other) => {
                return Err(self.mismatch(format!("`candidates` is not an array (got {other})")));
            }
        };
        let candidate = candidates[0]
            .as_object()
            .ok_or_else(|| self.mismatch("candidates[0] is not an object"))?;
        if let Some(reason) = candidate.get("finishReason").and_then(Value::as_str)
            && !reason.is_empty()
        {
            self.finish_reason = Some(reason.to_string());
        }

        let parts = match candidate.get("content").and_then(|c| c.get("parts")) {
            None => None,
            Some(Value::Null) => None,
            Some(Value::Array(arr)) => Some(arr),
            Some(other) => {
                return Err(self.mismatch(format!("`content.parts` is not an array (got {other})")));
            }
        };
        if let Some(parts) = parts {
            for part in parts {
                self.handle_part(part, &mut events)?;
            }
        }
        Ok(events)
    }

    fn handle_part(
        &mut self,
        part: &Value,
        events: &mut Vec<AgentEvent>,
    ) -> Result<(), ProviderError> {
        let is_thought = part
            .get("thought")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let text = part.get("text").and_then(Value::as_str);
        let signature = part
            .get("thoughtSignature")
            .and_then(Value::as_str)
            .or_else(|| part.get("thought_signature").and_then(Value::as_str));

        if let Some(function_call) = part.get("functionCall") {
            self.close_open_block();
            let name = function_call
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| self.mismatch("functionCall part missing `name`"))?;
            let args = function_call
                .get("args")
                .cloned()
                .unwrap_or_else(|| Value::Object(Default::default()));
            self.tool_call_seq += 1;
            let id = ToolCallId::new(format!("call_{}", self.tool_call_seq));
            let index = self.alloc_index();
            // Gemini validates a replayed `thoughtSignature` against the exact functionCall part
            // it was issued on, so it rides this `ToolUse` block's own `opaque` (not a separate
            // `Reasoning` block) — see `wire::request::matching_thought_signature`.
            let opaque = signature.map(|sig| OpaqueBlob {
                provider: self.provider.clone(),
                transport: self.transport.clone(),
                data: serde_json::json!({ "thoughtSignature": sig }),
            });
            self.content.push(ContentBlock::ToolUse {
                id: id.clone(),
                name: name.to_string(),
                input: args,
                opaque,
            });
            events.push(AgentEvent::ToolCallStarted {
                index,
                id,
                name: name.to_string(),
            });
            return Ok(());
        }

        if let Some(inline) = part.get("inlineData") {
            self.close_open_block();
            let mime = inline
                .get("mimeType")
                .and_then(Value::as_str)
                .unwrap_or("application/octet-stream")
                .to_string();
            let data = inline
                .get("data")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            self.content.push(ContentBlock::Image {
                media_type: mime,
                source: ImageSource::Base64 { data },
            });
            return Ok(());
        }

        if is_thought {
            if !matches!(self.open, Some(OpenBlock::Reasoning { .. })) {
                self.close_open_block();
                let index = self.alloc_index();
                self.open = Some(OpenBlock::Reasoning {
                    index,
                    buf: String::new(),
                    signature: None,
                });
            }
            let index = match &mut self.open {
                Some(OpenBlock::Reasoning {
                    index,
                    buf,
                    signature: sig,
                }) => {
                    if let Some(t) = text {
                        buf.push_str(t);
                    }
                    if let Some(s) = signature {
                        *sig = Some(s.to_string());
                    }
                    *index
                }
                _ => unreachable!("just opened a Reasoning block above"),
            };
            if let Some(t) = text
                && !t.is_empty()
            {
                events.push(AgentEvent::ReasoningDelta {
                    index,
                    text: t.to_string(),
                });
            }
            return Ok(());
        }

        if let Some(t) = text {
            if !matches!(self.open, Some(OpenBlock::Text { .. })) {
                self.close_open_block();
                let index = self.alloc_index();
                self.open = Some(OpenBlock::Text {
                    index,
                    buf: String::new(),
                });
            }
            let index = match &mut self.open {
                Some(OpenBlock::Text { index, buf }) => {
                    buf.push_str(t);
                    *index
                }
                _ => unreachable!("just opened a Text block above"),
            };
            if !t.is_empty() {
                events.push(AgentEvent::TextDelta {
                    index,
                    text: t.to_string(),
                });
            }
            return Ok(());
        }

        // Harmless unknown/empty part (PATTERNS.md §5: "harmless unknown field ⇒ skip it"). A
        // `thoughtSignature` on a `functionCall` part is already captured above (as `ToolUse`'s
        // `opaque`); this only drops a signature on some other unrecognized part shape.
        tracing::debug!(
            ?part,
            "agy wire: ignoring response part with no recognized payload"
        );
        Ok(())
    }

    /// Closes any open block and produces the terminal `Completed` event. Consumes `self`: a
    /// `Translator` is used for exactly one `stream()` call.
    pub(crate) fn finish(mut self) -> Result<AgentEvent, ProviderError> {
        self.close_open_block();
        if !self.saw_any_frame {
            return Err(self.mismatch("stream ended without any recognizable frame"));
        }
        let stop = if self.blocked_reason.is_some() {
            StopReason::Refusal
        } else {
            match self.finish_reason.as_deref() {
                None => {
                    return Err(self.mismatch(
                        "stream ended without a finishReason or blockReason (possible truncation)",
                    ));
                }
                Some("STOP") => {
                    if self
                        .content
                        .iter()
                        .any(|c| matches!(c, ContentBlock::ToolUse { .. }))
                    {
                        StopReason::ToolUse
                    } else {
                        StopReason::EndTurn
                    }
                }
                Some("MAX_TOKENS") => StopReason::MaxTokens,
                Some(reason)
                    if [
                        "SAFETY",
                        "RECITATION",
                        "BLOCKLIST",
                        "PROHIBITED_CONTENT",
                        "SPII",
                    ]
                    .contains(&reason) =>
                {
                    StopReason::Refusal
                }
                Some(other) => StopReason::Other(other.to_string()),
            }
        };
        Ok(AgentEvent::Completed {
            message: Message {
                role: Role::Assistant,
                content: self.content,
            },
            stop,
            usage: self.usage,
        })
    }
}

fn usage_from_gemini(meta: &serde_json::Map<String, Value>) -> Usage {
    Usage {
        input_tokens: meta
            .get("promptTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: meta
            .get("candidatesTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cached_input_tokens: meta
            .get("cachedContentTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        reasoning_tokens: meta
            .get("thoughtsTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    }
}

/// Maps an inline `{"error": {...}}` frame (a 200 OK SSE stream carrying a terminal upstream error,
/// vs. a non-2xx HTTP status handled by `provider::map_status` at the transport boundary).
fn map_inline_error(err: &Value) -> ProviderError {
    let code = err.get("code").and_then(Value::as_u64).unwrap_or(0);
    let message = err
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("upstream error");
    let excerpt = xlightcli_provider::body_excerpt(message);
    match code {
        401 => ProviderError::Auth(AuthFailure::Rejected),
        403 => ProviderError::Upstream {
            status: 403,
            body_excerpt: excerpt,
        },
        429 => ProviderError::RateLimited {
            retry_after: None,
            info: RateLimitInfo::default(),
        },
        400 => ProviderError::InvalidRequest(excerpt),
        _ => ProviderError::Upstream {
            status: code as u16,
            body_excerpt: excerpt,
        },
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;
    use xlightcli_provider::SseEvent;

    use super::*;

    fn translator(wrapped: bool) -> Translator {
        Translator::new(
            ProviderId::new("agy"),
            TransportId::new(if wrapped { "antigravity" } else { "gemini-api" }),
            wrapped,
            ProtocolVersion(1),
        )
    }

    fn sse(data: impl Into<String>) -> SseEvent {
        SseEvent {
            event: None,
            data: data.into(),
            id: None,
            retry: None,
        }
    }

    #[test]
    fn plain_text_and_stop_produces_completed_end_turn() {
        let mut tr = translator(false);
        let events = tr
            .feed(sse(
                r#"{"candidates":[{"content":{"parts":[{"text":"hi"}]},"finishReason":"STOP"}]}"#,
            ))
            .unwrap();
        assert_eq!(
            events,
            vec![AgentEvent::TextDelta {
                index: 0,
                text: "hi".into()
            }]
        );
        let completed = tr.finish().unwrap();
        match completed {
            AgentEvent::Completed { message, stop, .. } => {
                assert_eq!(stop, StopReason::EndTurn);
                assert_eq!(
                    message.content,
                    vec![ContentBlock::Text { text: "hi".into() }]
                );
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn cloud_code_assist_wrapper_is_unwrapped() {
        let mut tr = translator(true);
        let events = tr
            .feed(sse(
                r#"{"response":{"candidates":[{"content":{"parts":[{"text":"hi"}]},"finishReason":"STOP"}]}}"#,
            ))
            .unwrap();
        assert_eq!(
            events,
            vec![AgentEvent::TextDelta {
                index: 0,
                text: "hi".into()
            }]
        );
    }

    #[test]
    fn missing_wrapper_on_cloud_code_assist_is_protocol_mismatch() {
        let mut tr = translator(true);
        let err = tr
            .feed(sse(
                r#"{"candidates":[{"content":{"parts":[{"text":"hi"}]}}]}"#,
            ))
            .unwrap_err();
        assert!(matches!(err, ProviderError::ProtocolMismatch { .. }));
    }

    #[test]
    fn thought_part_with_signature_becomes_reasoning_block_with_opaque_blob() {
        let mut tr = translator(false);
        tr.feed(sse(
            r#"{"candidates":[{"content":{"parts":[{"text":"pondering","thought":true,"thoughtSignature":"sig-1"}]}}]}"#,
        ))
        .unwrap();
        tr.feed(sse(
            r#"{"candidates":[{"content":{"parts":[{"text":"answer"}]},"finishReason":"STOP"}]}"#,
        ))
        .unwrap();
        let completed = tr.finish().unwrap();
        let AgentEvent::Completed { message, .. } = completed else {
            panic!("expected Completed")
        };
        match &message.content[0] {
            ContentBlock::Reasoning { text, opaque } => {
                assert_eq!(text.as_deref(), Some("pondering"));
                let opaque = opaque.as_ref().unwrap();
                assert_eq!(opaque.provider, ProviderId::new("agy"));
                assert_eq!(opaque.data["thoughtSignature"], "sig-1");
            }
            other => panic!("expected Reasoning block, got {other:?}"),
        }
        assert_eq!(
            message.content[1],
            ContentBlock::Text {
                text: "answer".into()
            }
        );
    }

    #[test]
    fn function_call_part_emits_tool_call_started_and_tool_use_block() {
        let mut tr = translator(false);
        let events = tr
            .feed(sse(
                r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"read_file","args":{"path":"a.rs"}}}]},"finishReason":"STOP"}]}"#,
            ))
            .unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], AgentEvent::ToolCallStarted { .. }));
        let completed = tr.finish().unwrap();
        let AgentEvent::Completed { message, stop, .. } = completed else {
            panic!("expected Completed")
        };
        assert_eq!(stop, StopReason::ToolUse);
        match &message.content[0] {
            ContentBlock::ToolUse { name, input, .. } => {
                assert_eq!(name, "read_file");
                assert_eq!(input["path"].as_str().unwrap(), "a.rs");
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn function_call_thought_signature_is_carried_on_the_tool_use_blocks_opaque() {
        let mut tr = translator(false);
        tr.feed(sse(
            r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"read_file","args":{}},"thoughtSignature":"call-sig-1"}]},"finishReason":"STOP"}]}"#,
        ))
        .unwrap();
        let AgentEvent::Completed { message, .. } = tr.finish().unwrap() else {
            panic!("expected Completed")
        };
        match &message.content[0] {
            ContentBlock::ToolUse { opaque, .. } => {
                let opaque = opaque.as_ref().expect("signature must be captured");
                assert_eq!(opaque.provider, ProviderId::new("agy"));
                assert_eq!(opaque.transport, TransportId::new("gemini-api"));
                assert_eq!(opaque.data["thoughtSignature"], "call-sig-1");
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn parallel_function_calls_each_get_their_own_tool_use_block() {
        let mut tr = translator(false);
        tr.feed(sse(
            r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"a","args":{}}},{"functionCall":{"name":"b","args":{}}}]},"finishReason":"STOP"}]}"#,
        ))
        .unwrap();
        let completed = tr.finish().unwrap();
        let AgentEvent::Completed { message, .. } = completed else {
            panic!("expected Completed")
        };
        assert_eq!(message.content.len(), 2);
    }

    #[test]
    fn usage_metadata_is_translated() {
        let mut tr = translator(false);
        let events = tr
            .feed(sse(
                r#"{"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":5,"thoughtsTokenCount":2},"candidates":[{"finishReason":"STOP"}]}"#,
            ))
            .unwrap();
        assert!(matches!(events[0], AgentEvent::Usage(_)));
        if let AgentEvent::Usage(usage) = &events[0] {
            assert_eq!(usage.input_tokens, 10);
            assert_eq!(usage.output_tokens, 5);
            assert_eq!(usage.reasoning_tokens, 2);
        }
    }

    #[test]
    fn safety_block_finish_reason_becomes_refusal() {
        let mut tr = translator(false);
        tr.feed(sse(r#"{"candidates":[{"finishReason":"SAFETY"}]}"#))
            .unwrap();
        let completed = tr.finish().unwrap();
        let AgentEvent::Completed { stop, .. } = completed else {
            panic!("expected Completed")
        };
        assert_eq!(stop, StopReason::Refusal);
    }

    #[test]
    fn prompt_feedback_block_reason_becomes_refusal() {
        let mut tr = translator(false);
        tr.feed(sse(
            r#"{"promptFeedback":{"blockReason":"SAFETY"},"candidates":[]}"#,
        ))
        .unwrap();
        tr.feed(sse(r#"{"candidates":[{"finishReason":"STOP"}]}"#))
            .unwrap();
        let completed = tr.finish().unwrap();
        let AgentEvent::Completed { stop, .. } = completed else {
            panic!("expected Completed")
        };
        assert_eq!(stop, StopReason::Refusal);
    }

    #[test]
    fn inline_error_frame_maps_to_rate_limited() {
        let mut tr = translator(false);
        let err = tr
            .feed(sse(r#"{"error":{"code":429,"message":"quota exceeded"}}"#))
            .unwrap_err();
        assert!(matches!(err, ProviderError::RateLimited { .. }));
    }

    #[test]
    fn max_tokens_finish_reason_is_mapped() {
        let mut tr = translator(false);
        tr.feed(sse(r#"{"candidates":[{"content":{"parts":[{"text":"partial"}]},"finishReason":"MAX_TOKENS"}]}"#))
            .unwrap();
        let completed = tr.finish().unwrap();
        let AgentEvent::Completed { stop, .. } = completed else {
            panic!("expected Completed")
        };
        assert_eq!(stop, StopReason::MaxTokens);
    }

    #[test]
    fn stream_with_no_terminal_signal_is_protocol_mismatch() {
        let mut tr = translator(false);
        tr.feed(sse(
            r#"{"candidates":[{"content":{"parts":[{"text":"partial"}]}}]}"#,
        ))
        .unwrap();
        let err = tr.finish().unwrap_err();
        assert!(matches!(err, ProviderError::ProtocolMismatch { .. }));
    }

    #[test]
    fn empty_data_frame_is_ignored() {
        let mut tr = translator(false);
        let events = tr.feed(sse("   ")).unwrap();
        assert!(events.is_empty());
    }

    // -- Fixture-driven tests (PATTERNS.md §13): `tests/fixtures/*.sse`, redacted synthetic data
    // (no real account), asserted via `insta` snapshots of the translated `AgentEvent`s.

    fn fixture_path(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    async fn translate_fixture(name: &str, wrapped: bool) -> Vec<AgentEvent> {
        let sse_events = xlightcli_provider::testing::parse_sse_fixture(&fixture_path(name))
            .await
            .unwrap();
        let mut tr = translator(wrapped);
        let mut out = Vec::new();
        for ev in sse_events {
            out.extend(tr.feed(ev).unwrap());
        }
        out.push(tr.finish().unwrap());
        out
    }

    #[tokio::test]
    async fn fixture_gemini_text_snapshot() {
        let events = translate_fixture("gemini_text.sse", false).await;
        insta::assert_yaml_snapshot!(events);
    }

    #[tokio::test]
    async fn fixture_cca_wrapped_text_snapshot() {
        let events = translate_fixture("cca_wrapped_text.sse", true).await;
        insta::assert_yaml_snapshot!(events);
    }

    #[tokio::test]
    async fn fixture_thought_signature_snapshot() {
        let events = translate_fixture("thought_signature.sse", false).await;
        insta::assert_yaml_snapshot!(events);
    }

    #[tokio::test]
    async fn fixture_function_call_snapshot() {
        let events = translate_fixture("function_call.sse", false).await;
        insta::assert_yaml_snapshot!(events);
    }

    #[tokio::test]
    async fn fixture_parallel_function_calls_snapshot() {
        let events = translate_fixture("parallel_function_calls.sse", false).await;
        insta::assert_yaml_snapshot!(events);
    }

    #[tokio::test]
    async fn fixture_safety_block_snapshot() {
        let events = translate_fixture("safety_block.sse", false).await;
        insta::assert_yaml_snapshot!(events);
    }

    #[tokio::test]
    async fn fixture_error_maps_to_rate_limited() {
        let sse_events = xlightcli_provider::testing::parse_sse_fixture(&fixture_path("error.sse"))
            .await
            .unwrap();
        let mut tr = translator(false);
        let mut last_err = None;
        for ev in sse_events {
            if let Err(e) = tr.feed(ev) {
                last_err = Some(e);
            }
        }
        assert!(matches!(last_err, Some(ProviderError::RateLimited { .. })));
    }
}
