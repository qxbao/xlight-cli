// SPDX-License-Identifier: GPL-3.0-only

//! `TurnRequest` → Anthropic Messages wire body (PATTERNS.md §5).
//!
//! Pure, IO-free translation (no network, no credential access — the transport inserts auth
//! headers separately via `CredentialHandle::authorize`). Shared by `transport_api` and
//! `transport_subscription`; the only behavioral difference between the two is [`RequestOptions`]
//! (`oauth_mode`), since Claude's OAuth ("Claude Pro/Max") tokens require an extra system
//! instruction and a tool-name prefix (docs/PLAN.md §4.5 port map, `consts.rs`).

use serde_json::{Map, Value, json};
use xlightcli_protocol::{
    ContentBlock, ImageSource, Message, ProviderError, ProviderId, ReasoningEffort, Role,
    ToolResultPart, TransportId, TurnRequest,
};

use crate::consts;

/// Behavioral knobs that differ between `anthropic-api` and `claude-subscription`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RequestOptions {
    /// `true` for `claude-subscription` (Claude Pro/Max OAuth): prepends
    /// `consts::OAUTH_SYSTEM_INSTRUCTION` and prefixes tool names (`consts::
    /// OAUTH_TOOL_NAME_PREFIX`), both required by Anthropic's OAuth token policy.
    pub oauth_mode: bool,
    /// Always `true` in Phase 0 (every transport streams); kept as a field rather than a
    /// hardcoded `true` so a future non-streaming call path doesn't need a second builder.
    pub stream: bool,
}

/// Maps the canonical `ReasoningEffort` to an extended-thinking token budget. **M** — a simple,
/// documented 4-tier mapping (not Anthropic's own scale); revisit once the live spike
/// (docs/providers/claude.md) confirms per-model minimums/ceilings.
fn effort_to_budget_tokens(effort: ReasoningEffort) -> u32 {
    match effort {
        ReasoningEffort::Minimal => 1024,
        ReasoningEffort::Low => 4096,
        ReasoningEffort::Medium => 8192,
        ReasoningEffort::High => 16384,
    }
}

fn is_oauth_builtin_tool(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    consts::OAUTH_BUILTIN_TOOL_NAMES.contains(&lower.as_str())
        || lower.starts_with(consts::OAUTH_TOOL_NAME_PREFIX)
}

/// Applies the OAuth tool-name prefix (docs/PLAN.md §4.5 port map: OpenCodex
/// `applyClaudeToolPrefix`) unless `oauth_mode` is off or the name is already exempt.
fn tool_name_to_wire(name: &str, oauth_mode: bool) -> String {
    if !oauth_mode || is_oauth_builtin_tool(name) {
        return name.to_string();
    }
    format!("{}{name}", consts::OAUTH_TOOL_NAME_PREFIX)
}

fn image_source_to_json(source: &ImageSource) -> Result<Value, ProviderError> {
    match source {
        ImageSource::Base64 { data } => Ok(json!({
            "type": "base64",
            // Anthropic requires a concrete media_type; Phase 0 canonical `ImageSource::Base64`
            // doesn't carry one yet, so this is deliberately the common default rather than a
            // guess at an unusual format. Revisit if `protocol::ImageSource` grows a media type.
            "media_type": "image/png",
            "data": data,
        })),
        ImageSource::Artifact { path } => Err(ProviderError::InvalidRequest(format!(
            "anthropic-api: image content sourced from an artifact path ({path}) is not supported \
             yet (Phase 0) — inline base64 only"
        ))),
    }
}

fn tool_result_part_to_json(part: &ToolResultPart) -> Result<Value, ProviderError> {
    match part {
        ToolResultPart::Text { text } => Ok(json!({"type": "text", "text": text})),
        ToolResultPart::Image { source, .. } => {
            Ok(json!({"type": "image", "source": image_source_to_json(source)?}))
        }
    }
}

/// Translates one canonical `ContentBlock` into an Anthropic content block, or `Ok(None)` when the
/// block must be silently dropped (PATTERNS.md §5: an `OpaqueBlob` only replays to the
/// `(provider, transport)` that produced it; anything else is dropped, not guessed).
fn content_block_to_json(
    block: &ContentBlock,
    provider: &ProviderId,
    transport: &TransportId,
    oauth_mode: bool,
) -> Result<Option<Value>, ProviderError> {
    match block {
        ContentBlock::Text { text } => Ok(Some(json!({"type": "text", "text": text}))),
        ContentBlock::Image { source, .. } => Ok(Some(json!({
            "type": "image",
            "source": image_source_to_json(source)?,
        }))),
        ContentBlock::Reasoning { text, opaque } => {
            let Some(blob) = opaque else {
                // No continuity data at all: nothing Anthropic-shaped to replay.
                return Ok(None);
            };
            if &blob.provider != provider || &blob.transport != transport {
                // Continuity data minted by another (provider, transport) — never replay it
                // (PATTERNS.md §5).
                return Ok(None);
            }
            if let Some(data) = blob.data.get("redacted_thinking").and_then(Value::as_str) {
                return Ok(Some(json!({"type": "redacted_thinking", "data": data})));
            }
            if let Some(signature) = blob.data.get("signature").and_then(Value::as_str) {
                return Ok(Some(json!({
                    "type": "thinking",
                    "thinking": text.clone().unwrap_or_default(),
                    "signature": signature,
                })));
            }
            // Opaque blob shape we don't recognize (e.g. produced by a future translator
            // version): drop rather than guess a wire shape that would 400.
            Ok(None)
        }
        ContentBlock::ToolUse {
            id, name, input, ..
        } => Ok(Some(json!({
            "type": "tool_use",
            "id": id.as_str(),
            "name": tool_name_to_wire(name, oauth_mode),
            "input": input,
        }))),
        ContentBlock::ToolResult {
            call_id,
            content,
            is_error,
        } => {
            let parts = content
                .iter()
                .map(tool_result_part_to_json)
                .collect::<Result<Vec<_>, _>>()?;
            let mut value = json!({
                "type": "tool_result",
                "tool_use_id": call_id.as_str(),
                "content": parts,
            });
            if *is_error {
                value["is_error"] = json!(true);
            }
            Ok(Some(value))
        }
    }
}

fn role_str(role: Role) -> Option<&'static str> {
    match role {
        Role::User => Some("user"),
        Role::Assistant => Some("assistant"),
        // Handled separately: folded into the `system` field, not sent as a message.
        Role::System => None,
    }
}

fn message_to_json(
    message: &Message,
    provider: &ProviderId,
    transport: &TransportId,
    oauth_mode: bool,
) -> Result<Option<Value>, ProviderError> {
    let Some(role) = role_str(message.role) else {
        return Ok(None);
    };
    let mut blocks = Vec::with_capacity(message.content.len());
    for block in &message.content {
        if let Some(value) = content_block_to_json(block, provider, transport, oauth_mode)? {
            blocks.push(value);
        }
    }
    if blocks.is_empty() {
        // Every block in this message was dropped (e.g. a lone unreplayable reasoning block) —
        // Anthropic rejects an empty `content` array, and there's nothing left to say.
        tracing::debug!(
            role,
            "anthropic wire: message had no representable content, dropping"
        );
        return Ok(None);
    }
    Ok(Some(json!({"role": role, "content": blocks})))
}

fn system_blocks(req: &TurnRequest, oauth_mode: bool) -> Option<Value> {
    let mut texts = Vec::new();
    if oauth_mode {
        texts.push(consts::OAUTH_SYSTEM_INSTRUCTION.to_string());
    }
    if !req.system.text.is_empty() {
        texts.push(req.system.text.clone());
    }
    for message in &req.messages {
        if message.role != Role::System {
            continue;
        }
        for block in &message.content {
            if let ContentBlock::Text { text } = block {
                texts.push(text.clone());
            }
        }
    }
    if texts.is_empty() {
        return None;
    }
    Some(Value::Array(
        texts
            .into_iter()
            .map(|text| json!({"type": "text", "text": text}))
            .collect(),
    ))
}

/// Builds the Anthropic Messages API request body for `req`.
pub(crate) fn build_body(
    req: &TurnRequest,
    provider: &ProviderId,
    transport: &TransportId,
    options: RequestOptions,
) -> Result<Value, ProviderError> {
    let mut messages = Vec::with_capacity(req.messages.len());
    for message in &req.messages {
        if let Some(value) = message_to_json(message, provider, transport, options.oauth_mode)? {
            messages.push(value);
        }
    }

    let mut body = Map::new();
    body.insert("model".into(), json!(req.model.as_str()));
    body.insert("messages".into(), Value::Array(messages));
    body.insert("stream".into(), json!(options.stream));

    if let Some(system) = system_blocks(req, options.oauth_mode) {
        body.insert("system".into(), system);
    }

    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool_name_to_wire(&tool.name, options.oauth_mode),
                    "description": tool.description,
                    "input_schema": tool.input_schema,
                })
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
    }

    let base_max_tokens = req.max_output_tokens.unwrap_or(consts::DEFAULT_MAX_TOKENS);
    if let Some(reasoning) = &req.reasoning {
        let budget =
            effort_to_budget_tokens(reasoning.effort).max(consts::MIN_THINKING_BUDGET_TOKENS);
        let max_tokens = base_max_tokens.max(budget + consts::THINKING_OUTPUT_HEADROOM);
        body.insert(
            "thinking".into(),
            json!({"type": "enabled", "budget_tokens": budget}),
        );
        body.insert("max_tokens".into(), json!(max_tokens));
    } else {
        body.insert("max_tokens".into(), json!(base_max_tokens));
    }

    if !req.provider_options.is_empty() {
        // Opaque, transport-specific overrides (docs/PLAN.md §4.3): core never inspects this map,
        // so merge it last and let it win over anything computed above.
        for (key, value) in req.provider_options.as_map() {
            body.insert(key.clone(), value.clone());
        }
    }

    Ok(Value::Object(body))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;
    use xlightcli_protocol::{
        ModelId, OpaqueBlob, ReasoningConfig, SystemPrompt, ToolCallId, ToolDefinition,
    };

    use super::*;

    fn provider() -> ProviderId {
        ProviderId::new("claude")
    }

    fn transport() -> TransportId {
        TransportId::new("anthropic-api")
    }

    fn opts() -> RequestOptions {
        RequestOptions {
            oauth_mode: false,
            stream: true,
        }
    }

    #[test]
    fn simple_request_builds_expected_shape() {
        let req = TurnRequest::simple(ModelId::new("claude-sonnet-5"), "hello");
        let body = build_body(&req, &provider(), &transport(), opts()).unwrap();
        assert_eq!(body["model"], json!("claude-sonnet-5"));
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["messages"][0]["role"], json!("user"));
        assert_eq!(body["messages"][0]["content"][0]["type"], json!("text"));
        assert_eq!(body["max_tokens"], json!(consts::DEFAULT_MAX_TOKENS));
        assert!(body.get("system").is_none());
    }

    #[test]
    fn system_prompt_and_oauth_instruction_are_both_included() {
        let mut req = TurnRequest::simple(ModelId::new("m"), "hi");
        req.system = SystemPrompt::new("Follow project rules.");
        let body = build_body(
            &req,
            &provider(),
            &transport(),
            RequestOptions {
                oauth_mode: true,
                stream: true,
            },
        )
        .unwrap();
        let system = body["system"].as_array().unwrap();
        assert_eq!(system.len(), 2);
        assert_eq!(system[0]["text"], json!(consts::OAUTH_SYSTEM_INSTRUCTION));
        assert_eq!(system[1]["text"], json!("Follow project rules."));
    }

    #[test]
    fn reasoning_config_sets_thinking_budget_and_grows_max_tokens() {
        let mut req = TurnRequest::simple(ModelId::new("m"), "hi");
        req.reasoning = Some(ReasoningConfig {
            effort: ReasoningEffort::High,
            include_text: true,
        });
        let body = build_body(&req, &provider(), &transport(), opts()).unwrap();
        assert_eq!(body["thinking"]["type"], json!("enabled"));
        assert_eq!(body["thinking"]["budget_tokens"], json!(16384));
        let max_tokens = body["max_tokens"].as_u64().unwrap();
        assert!(
            max_tokens > 16384,
            "max_tokens must exceed the thinking budget"
        );
    }

    #[test]
    fn matching_opaque_blob_replays_as_thinking_block_with_signature() {
        let mut req = TurnRequest::simple(ModelId::new("m"), "hi");
        req.messages.push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Reasoning {
                text: Some("because...".into()),
                opaque: Some(OpaqueBlob {
                    provider: provider(),
                    transport: transport(),
                    data: json!({"signature": "sig-abc"}),
                }),
            }],
        });
        let body = build_body(&req, &provider(), &transport(), opts()).unwrap();
        let assistant_msg = &body["messages"][1];
        assert_eq!(assistant_msg["content"][0]["type"], json!("thinking"));
        assert_eq!(assistant_msg["content"][0]["signature"], json!("sig-abc"));
    }

    #[test]
    fn opaque_blob_from_a_different_transport_is_dropped() {
        let mut req = TurnRequest::simple(ModelId::new("m"), "hi");
        req.messages.push(Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "kept".into(),
                },
                ContentBlock::Reasoning {
                    text: Some("because...".into()),
                    opaque: Some(OpaqueBlob {
                        provider: provider(),
                        transport: TransportId::new("claude-subscription"),
                        data: json!({"signature": "sig-abc"}),
                    }),
                },
            ],
        });
        let body = build_body(&req, &provider(), &transport(), opts()).unwrap();
        let assistant_content = body["messages"][1]["content"].as_array().unwrap();
        assert_eq!(assistant_content.len(), 1);
        assert_eq!(assistant_content[0]["type"], json!("text"));
    }

    #[test]
    fn redacted_thinking_blob_replays_verbatim() {
        let mut req = TurnRequest::simple(ModelId::new("m"), "hi");
        req.messages.push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Reasoning {
                text: None,
                opaque: Some(OpaqueBlob {
                    provider: provider(),
                    transport: transport(),
                    data: json!({"redacted_thinking": "opaque-bytes"}),
                }),
            }],
        });
        let body = build_body(&req, &provider(), &transport(), opts()).unwrap();
        let block = &body["messages"][1]["content"][0];
        assert_eq!(block["type"], json!("redacted_thinking"));
        assert_eq!(block["data"], json!("opaque-bytes"));
    }

    #[test]
    fn tool_use_and_tool_result_roundtrip() {
        let mut req = TurnRequest::simple(ModelId::new("m"), "hi");
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
        let body = build_body(&req, &provider(), &transport(), opts()).unwrap();
        assert_eq!(body["messages"][1]["content"][0]["type"], json!("tool_use"));
        assert_eq!(body["messages"][1]["content"][0]["id"], json!("call-1"));
        assert_eq!(
            body["messages"][2]["content"][0]["type"],
            json!("tool_result")
        );
        assert_eq!(
            body["messages"][2]["content"][0]["tool_use_id"],
            json!("call-1")
        );
    }

    #[test]
    fn oauth_mode_prefixes_tool_names_but_exempts_builtins() {
        let mut req = TurnRequest::simple(ModelId::new("m"), "hi");
        req.tools.push(ToolDefinition {
            name: "read_file".into(),
            description: "read".into(),
            input_schema: json!({"type": "object"}),
        });
        req.tools.push(ToolDefinition {
            name: "web_search".into(),
            description: "search".into(),
            input_schema: json!({"type": "object"}),
        });
        let body = build_body(
            &req,
            &provider(),
            &transport(),
            RequestOptions {
                oauth_mode: true,
                stream: true,
            },
        )
        .unwrap();
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools[0]["name"], json!("custom_read_file"));
        assert_eq!(tools[1]["name"], json!("web_search"));
    }

    #[test]
    fn artifact_image_source_is_rejected_as_invalid_request_for_now() {
        let mut req = TurnRequest::simple(ModelId::new("m"), "hi");
        req.messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::Image {
                media_type: "image/png".into(),
                source: ImageSource::Artifact {
                    path: "/tmp/x.png".into(),
                },
            }],
        });
        let err = build_body(&req, &provider(), &transport(), opts()).unwrap_err();
        assert!(matches!(err, ProviderError::InvalidRequest(_)));
    }
}
