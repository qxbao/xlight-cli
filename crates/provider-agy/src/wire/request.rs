// SPDX-License-Identifier: GPL-3.0-only
// Portions derived from OpenCodex (MIT) @ 3cc34e1181926b64331490fdcfee162ffb62fe73:
// src/adapters/google.ts, src/adapters/google-antigravity-wire.ts, src/adapters/google-tool-schema.ts
// See THIRD_PARTY.md.

//! `TurnRequest` → Gemini `generateContent` body, and (for `antigravity`) the Cloud Code Assist
//! envelope wrapped around it.
//!
//! Envelope shape, `sessionId` derivation and the tool-schema allowlist are ported from OpenCodex
//! (MIT) `@ 3cc34e1181926b64331490fdcfee162ffb62fe73`: `src/adapters/google.ts` (envelope +
//! `generateContent` body construction), `src/adapters/google-antigravity-wire.ts`
//! (`antigravitySessionId`), `src/adapters/google-tool-schema.ts`
//! (`sanitizeGeminiToolParameters` — ported as a much smaller allowlist-only subset; no
//! depth/node budget, no `anyOf`/`$ref` inlining). See `THIRD_PARTY.md`.

use std::collections::HashMap;

use serde_json::{Map, Value, json};
#[cfg(feature = "antigravity-subscription")]
use sha2::{Digest, Sha256};
#[cfg(feature = "antigravity-subscription")]
use xlightcli_protocol::Message;
use xlightcli_protocol::{
    ContentBlock, ImageSource, OpaqueBlob, ProviderError, ProviderId, ReasoningEffort, Role,
    ToolCallId, ToolResultPart, TransportId, TurnRequest,
};

#[cfg(feature = "antigravity-subscription")]
use crate::consts::antigravity;

/// Gemini function-declaration `parameters` accept a documented JSON-Schema **subset**
/// (type/description/nullable/format/properties/items/required/enum) and reject unknown keywords
/// (`additionalProperties`, `$schema`, …) with a 400. Simplified port of OpenCodex's
/// `sanitizeGeminiToolParameters` — Phase 0 tools (`crates/tools`) don't yet emit `anyOf`/`$ref`,
/// so those aren't inlined here; extend this if a real tool schema needs them.
const ALLOWED_SCHEMA_KEYS: &[&str] = &[
    "type",
    "description",
    "nullable",
    "format",
    "properties",
    "items",
    "required",
    "enum",
];

fn sanitize_schema(schema: &Value) -> Value {
    prune_schema(schema)
}

/// Returns the `thoughtSignature` string carried by `opaque`, but only when it was produced by
/// this exact `(provider, transport)` (PATTERNS.md §5, D-009) — never replay a blob authored by a
/// different provider/transport. Shared by `Reasoning` and `ToolUse` blocks: Gemini attaches a
/// signature either to its own dedicated `thought` part or directly on the `functionCall` part
/// that produced it.
fn matching_thought_signature<'a>(
    opaque: &'a Option<OpaqueBlob>,
    provider: &ProviderId,
    transport: &TransportId,
) -> Option<&'a str> {
    let blob = opaque.as_ref()?;
    if &blob.provider != provider || &blob.transport != transport {
        return None;
    }
    blob.data.get("thoughtSignature").and_then(Value::as_str)
}

fn prune_schema(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, val) in map {
                if !ALLOWED_SCHEMA_KEYS.contains(&key.as_str()) {
                    continue;
                }
                let pruned = match key.as_str() {
                    "properties" => match val {
                        Value::Object(props) => Value::Object(
                            props
                                .iter()
                                .map(|(pk, pv)| (pk.clone(), prune_schema(pv)))
                                .collect(),
                        ),
                        other => other.clone(),
                    },
                    "items" => prune_schema(val),
                    _ => val.clone(),
                };
                out.insert(key.clone(), pruned);
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// Maps the canonical reasoning effort ladder onto Gemini's `thinkingConfig.thinkingLevel`.
/// Simplification vs. OpenCodex's `resolveAntigravityEffortWireModel`: we never resuffix/redirect
/// the model id for retired tiers, we always emit `thinkingLevel` directly. Documented in
/// `docs/providers/agy.md` and the Wave 2 report as a Phase 0 gap.
pub(crate) fn effort_to_thinking_level(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Minimal | ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
    }
}

// The four items below are only used by `transport_antigravity` (feature
// `antigravity-subscription`); gating them at the item level (rather than just their re-export in
// `wire/mod.rs`) keeps `cargo clippy --all-targets -- -D warnings` (default features, no
// `antigravity-subscription`) free of `dead_code` warnings.
#[cfg(feature = "antigravity-subscription")]
fn first_user_text(messages: &[Message]) -> Option<String> {
    for message in messages {
        if message.role != Role::User {
            continue;
        }
        for block in &message.content {
            if let ContentBlock::Text { text } = block {
                return Some(text.clone());
            }
        }
    }
    None
}

/// Deterministic Cloud Code Assist session id derived from the first user message's text
/// (`sha256(text)` → big-endian `u64` masked with `0x7FFF_FFFF_FFFF_FFFF`, prefixed with `-`),
/// mirroring OpenCodex's `antigravitySessionId`/CLIProxyAPI `generateStableSessionID`. It must stay
/// **stable across turns of the same conversation** (the CCA session id keys reasoning-signature
/// replay) — falls back to a random id when no user text exists yet (first turn with no text, or a
/// document-only opening turn; see the ported source for the fuller anchor logic we do not port).
#[cfg(feature = "antigravity-subscription")]
pub(crate) fn antigravity_session_id(messages: &[Message]) -> String {
    let seed = first_user_text(messages).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let digest = Sha256::digest(seed.as_bytes());
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&digest[..8]);
    let masked = u64::from_be_bytes(buf) & 0x7fff_ffff_ffff_ffff;
    format!("-{masked}")
}

/// `agent-<uuid>` request id, matching the envelope's `requestId` field.
#[cfg(feature = "antigravity-subscription")]
pub(crate) fn new_request_id() -> String {
    format!("agent-{}", uuid::Uuid::new_v4())
}

/// Builds the flat Gemini `generateContent`/`streamGenerateContent` body from a canonical
/// `TurnRequest`. Shared by both transports; `antigravity` wraps the result in the CCA envelope
/// via [`build_antigravity_envelope`].
///
/// `(provider, transport)` identify **this** adapter so `Reasoning` blocks only replay a
/// `thoughtSignature` from an `OpaqueBlob` that this same `(provider, transport)` produced
/// (PATTERNS.md §5, D-009) — never a blob authored by a different provider/transport.
pub(crate) fn build_generate_content_body(
    provider: &ProviderId,
    transport: &TransportId,
    req: &TurnRequest,
) -> Result<Value, ProviderError> {
    let mut contents = Vec::new();
    let mut call_names: HashMap<ToolCallId, String> = HashMap::new();
    let mut system_text = req.system.text.clone();

    for message in &req.messages {
        if message.role == Role::System {
            for block in &message.content {
                if let ContentBlock::Text { text } = block {
                    if !system_text.is_empty() {
                        system_text.push('\n');
                    }
                    system_text.push_str(text);
                }
            }
            continue;
        }

        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "model",
            Role::System => unreachable!("System role handled above"),
        };
        let mut parts = Vec::new();
        for block in &message.content {
            match block {
                ContentBlock::Text { text } => parts.push(json!({ "text": text })),
                ContentBlock::Image { media_type, source } => match source {
                    ImageSource::Base64 { data } => {
                        parts.push(
                            json!({ "inlineData": { "mimeType": media_type, "data": data } }),
                        );
                    }
                    ImageSource::Artifact { path } => {
                        return Err(ProviderError::InvalidRequest(format!(
                            "agy wire: image artifacts are not embedded from disk yet (path {path}); Phase 0 only supports inline base64 images"
                        )));
                    }
                },
                ContentBlock::Reasoning { text, opaque } => {
                    let mut part = Map::new();
                    part.insert("thought".into(), json!(true));
                    if let Some(text) = text {
                        part.insert("text".into(), json!(text));
                    }
                    if let Some(sig) = matching_thought_signature(opaque, provider, transport) {
                        part.insert("thoughtSignature".into(), json!(sig));
                    }
                    parts.push(Value::Object(part));
                }
                ContentBlock::ToolUse {
                    id,
                    name,
                    input,
                    opaque,
                } => {
                    call_names.insert(id.clone(), name.clone());
                    let mut function_call = Map::new();
                    function_call.insert("name".into(), json!(name));
                    function_call.insert("args".into(), input.clone());
                    let mut part = Map::new();
                    part.insert("functionCall".into(), Value::Object(function_call));
                    // Gemini validates a replayed `thoughtSignature` against the exact functionCall
                    // part that produced it, so — unlike `Reasoning`'s blob, which sits on its own
                    // part — this one rides the `thoughtSignature` key directly on the part that
                    // carries the `functionCall` itself (matches the shape `wire::response`
                    // observed it in; see OpenCodex `googleToolCallMetadataFromPart`).
                    if let Some(sig) = matching_thought_signature(opaque, provider, transport) {
                        part.insert("thoughtSignature".into(), json!(sig));
                    }
                    parts.push(Value::Object(part));
                }
                ContentBlock::ToolResult {
                    call_id,
                    content,
                    is_error,
                } => {
                    let name = call_names.get(call_id).cloned().unwrap_or_else(|| {
                        tracing::debug!(
                            call_id = %call_id,
                            "agy wire: tool result with no matching prior functionCall name in this request"
                        );
                        String::new()
                    });
                    let mut response_text = String::new();
                    for part in content {
                        match part {
                            ToolResultPart::Text { text } => {
                                if !response_text.is_empty() {
                                    response_text.push('\n');
                                }
                                response_text.push_str(text);
                            }
                            ToolResultPart::Image { .. } => {
                                // Gemini `functionResponse.response` has no image slot; dropped.
                                tracing::debug!(
                                    "agy wire: dropping image content of a tool result (Gemini functionResponse has no image slot)"
                                );
                            }
                        }
                    }
                    let response_value = if *is_error {
                        json!({ "error": response_text })
                    } else {
                        json!({ "output": response_text })
                    };
                    parts.push(
                        json!({ "functionResponse": { "name": name, "response": response_value } }),
                    );
                }
            }
        }
        if parts.is_empty() {
            continue;
        }
        contents.push(json!({ "role": role, "parts": parts }));
    }

    let mut body = Map::new();
    body.insert("contents".into(), Value::Array(contents));
    if !system_text.is_empty() {
        body.insert(
            "systemInstruction".into(),
            json!({ "parts": [{ "text": system_text }] }),
        );
    }
    if !req.tools.is_empty() {
        let declarations: Vec<Value> = req
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": sanitize_schema(&tool.input_schema),
                })
            })
            .collect();
        body.insert(
            "tools".into(),
            json!([{ "functionDeclarations": declarations }]),
        );
    }

    let mut generation_config = Map::new();
    if let Some(max) = req.max_output_tokens {
        generation_config.insert("maxOutputTokens".into(), json!(max));
    }
    if let Some(reasoning) = &req.reasoning {
        let mut thinking = Map::new();
        thinking.insert(
            "thinkingLevel".into(),
            json!(effort_to_thinking_level(reasoning.effort)),
        );
        if reasoning.include_text {
            thinking.insert("includeThoughts".into(), json!(true));
        }
        generation_config.insert("thinkingConfig".into(), Value::Object(thinking));
    }
    if !generation_config.is_empty() {
        body.insert("generationConfig".into(), Value::Object(generation_config));
    }

    Ok(Value::Object(body))
}

/// Wraps a flat Gemini body in the Cloud Code Assist envelope
/// (`{model, userAgent: "antigravity", requestType: "agent", project, requestId, request}`), with
/// `sessionId` nested inside `request` (camelCase, matching the real Antigravity client — an extra
/// top-level/snake_case spelling is a non-first-party key, per the ported source).
#[cfg(feature = "antigravity-subscription")]
pub(crate) fn build_antigravity_envelope(
    model: &str,
    project: &str,
    request_id: &str,
    session_id: &str,
    mut request_body: Value,
) -> Value {
    if let Value::Object(map) = &mut request_body {
        map.insert("sessionId".into(), json!(session_id));
    }
    json!({
        "model": model,
        "userAgent": antigravity::ENVELOPE_USER_AGENT_FIELD,
        "requestType": antigravity::ENVELOPE_REQUEST_TYPE,
        "project": project,
        "requestId": request_id,
        "request": request_body,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;
    use xlightcli_protocol::{
        Message, ModelId, OpaqueBlob, ReasoningConfig, SystemPrompt, ToolDefinition,
    };

    use super::*;

    fn provider_transport() -> (ProviderId, TransportId) {
        (ProviderId::new("agy"), TransportId::new("gemini-api"))
    }

    #[test]
    fn simple_request_becomes_single_user_content() {
        let (provider, transport) = provider_transport();
        let req = TurnRequest::simple(ModelId::new("gemini-3.1-pro"), "hello");
        let body = build_generate_content_body(&provider, &transport, &req).unwrap();
        assert_eq!(body["contents"][0]["role"].as_str().unwrap(), "user");
        assert_eq!(
            body["contents"][0]["parts"][0]["text"].as_str().unwrap(),
            "hello"
        );
        assert!(body.get("systemInstruction").is_none());
    }

    #[test]
    fn system_prompt_becomes_system_instruction() {
        let (provider, transport) = provider_transport();
        let mut req = TurnRequest::simple(ModelId::new("gemini-3.1-pro"), "hi");
        req.system = SystemPrompt::new("be terse");
        let body = build_generate_content_body(&provider, &transport, &req).unwrap();
        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"]
                .as_str()
                .unwrap(),
            "be terse"
        );
    }

    #[test]
    fn tool_use_and_result_round_trip_into_function_call_and_response() {
        let (provider, transport) = provider_transport();
        let mut req = TurnRequest::simple(ModelId::new("gemini-3.1-pro"), "read file");
        req.messages.push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: ToolCallId::new("call-1"),
                name: "read_file".into(),
                input: json!({"path": "a.rs"}),
                opaque: None,
            }],
        });
        req.messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                call_id: ToolCallId::new("call-1"),
                content: vec![ToolResultPart::Text {
                    text: "contents".into(),
                }],
                is_error: false,
            }],
        });
        let body = build_generate_content_body(&provider, &transport, &req).unwrap();
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(
            contents[1]["parts"][0]["functionCall"]["name"]
                .as_str()
                .unwrap(),
            "read_file"
        );
        assert_eq!(
            contents[2]["parts"][0]["functionResponse"]["name"]
                .as_str()
                .unwrap(),
            "read_file"
        );
        assert_eq!(
            contents[2]["parts"][0]["functionResponse"]["response"]["output"]
                .as_str()
                .unwrap(),
            "contents"
        );
    }

    #[test]
    fn reasoning_replays_signature_only_for_matching_provider_and_transport() {
        let (provider, transport) = provider_transport();
        let mut req = TurnRequest::simple(ModelId::new("gemini-3.1-pro"), "hi");
        req.messages.push(Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Reasoning {
                    text: Some("thinking".into()),
                    opaque: Some(OpaqueBlob {
                        provider: provider.clone(),
                        transport: transport.clone(),
                        data: json!({"thoughtSignature": "sig-abc"}),
                    }),
                },
                ContentBlock::Reasoning {
                    text: Some("foreign thinking".into()),
                    opaque: Some(OpaqueBlob {
                        provider: ProviderId::new("claude"),
                        transport: TransportId::new("anthropic-api"),
                        data: json!({"thoughtSignature": "should-not-replay"}),
                    }),
                },
            ],
        });
        let body = build_generate_content_body(&provider, &transport, &req).unwrap();
        let parts = body["contents"][1]["parts"].as_array().unwrap();
        assert_eq!(parts[0]["thoughtSignature"].as_str().unwrap(), "sig-abc");
        assert!(parts[1].get("thoughtSignature").is_none());
    }

    #[test]
    fn tool_use_replays_thought_signature_only_for_matching_provider_and_transport() {
        let (provider, transport) = provider_transport();
        let mut req = TurnRequest::simple(ModelId::new("gemini-3.1-pro"), "hi");
        req.messages.push(Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::ToolUse {
                    id: ToolCallId::new("call-1"),
                    name: "read_file".into(),
                    input: json!({"path": "a.rs"}),
                    opaque: Some(OpaqueBlob {
                        provider: provider.clone(),
                        transport: transport.clone(),
                        data: json!({"thoughtSignature": "call-sig"}),
                    }),
                },
                ContentBlock::ToolUse {
                    id: ToolCallId::new("call-2"),
                    name: "read_file".into(),
                    input: json!({"path": "b.rs"}),
                    opaque: Some(OpaqueBlob {
                        provider: ProviderId::new("claude"),
                        transport: TransportId::new("anthropic-api"),
                        data: json!({"thoughtSignature": "should-not-replay"}),
                    }),
                },
            ],
        });
        let body = build_generate_content_body(&provider, &transport, &req).unwrap();
        let parts = body["contents"][1]["parts"].as_array().unwrap();
        assert_eq!(parts[0]["thoughtSignature"].as_str().unwrap(), "call-sig");
        assert_eq!(
            parts[0]["functionCall"]["name"].as_str().unwrap(),
            "read_file"
        );
        assert!(parts[1].get("thoughtSignature").is_none());
    }

    #[test]
    fn artifact_image_is_rejected_as_invalid_request() {
        let (provider, transport) = provider_transport();
        let mut req = TurnRequest::simple(ModelId::new("gemini-3.1-pro"), "hi");
        req.messages[0].content.push(ContentBlock::Image {
            media_type: "image/png".into(),
            source: ImageSource::Artifact {
                path: "/tmp/x.png".into(),
            },
        });
        let err = build_generate_content_body(&provider, &transport, &req).unwrap_err();
        assert!(matches!(err, ProviderError::InvalidRequest(_)));
    }

    #[test]
    fn tools_are_sanitized_to_the_allowed_schema_subset() {
        let (provider, transport) = provider_transport();
        let mut req = TurnRequest::simple(ModelId::new("gemini-3.1-pro"), "hi");
        req.tools.push(ToolDefinition {
            name: "read_file".into(),
            description: "Read a file".into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "path": {"type": "string", "pattern": "^/"}
                },
                "required": ["path"]
            }),
        });
        let body = build_generate_content_body(&provider, &transport, &req).unwrap();
        let decl = &body["tools"][0]["functionDeclarations"][0];
        assert_eq!(decl["name"].as_str().unwrap(), "read_file");
        assert!(decl["parameters"].get("additionalProperties").is_none());
        assert!(
            decl["parameters"]["properties"]["path"]
                .get("pattern")
                .is_none()
        );
        assert_eq!(
            decl["parameters"]["properties"]["path"]["type"]
                .as_str()
                .unwrap(),
            "string"
        );
    }

    #[test]
    fn reasoning_effort_maps_to_thinking_level() {
        let (provider, transport) = provider_transport();
        let mut req = TurnRequest::simple(ModelId::new("gemini-3.1-pro"), "hi");
        req.reasoning = Some(ReasoningConfig {
            effort: ReasoningEffort::High,
            include_text: true,
        });
        let body = build_generate_content_body(&provider, &transport, &req).unwrap();
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingLevel"]
                .as_str()
                .unwrap(),
            "high"
        );
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["includeThoughts"],
            json!(true)
        );
    }

    #[cfg(feature = "antigravity-subscription")]
    #[test]
    fn antigravity_session_id_is_stable_for_the_same_first_user_text() {
        let messages = vec![Message::user_text("hello world")];
        let a = antigravity_session_id(&messages);
        let b = antigravity_session_id(&messages);
        assert_eq!(a, b);
        assert!(a.starts_with('-'));
    }

    #[cfg(feature = "antigravity-subscription")]
    #[test]
    fn antigravity_envelope_nests_session_id_under_request() {
        let body = json!({"contents": []});
        let envelope =
            build_antigravity_envelope("gemini-3.1-pro", "proj-1", "agent-1", "-42", body);
        assert_eq!(envelope["model"].as_str().unwrap(), "gemini-3.1-pro");
        assert_eq!(envelope["userAgent"].as_str().unwrap(), "antigravity");
        assert_eq!(envelope["requestType"].as_str().unwrap(), "agent");
        assert_eq!(envelope["project"].as_str().unwrap(), "proj-1");
        assert_eq!(envelope["requestId"].as_str().unwrap(), "agent-1");
        assert_eq!(envelope["request"]["sessionId"].as_str().unwrap(), "-42");
        assert!(
            envelope["sessionId"].is_null(),
            "sessionId must not appear at the top level"
        );
    }
}
