// SPDX-License-Identifier: GPL-3.0-only

//! `TurnRequest` → OpenAI Responses API wire body.
//!
//! Body shape (`store: false`, `stream: true`, `include: ["reasoning.encrypted_content"]`,
//! `tool_choice: "auto"`) confirmed **H** 2026-09-28 against `openai/codex`
//! (Apache-2.0) @ `1cc7e2361237ce7244430ee1d581c77f95c57ac8`,
//! `codex-rs/core/src/client.rs::build_responses_request`. Item/content-part type names
//! (`message`/`function_call`/`function_call_output`/`reasoning`, `input_text`/`input_image`) are
//! the public Responses API surface (platform.openai.com/docs/api-reference/responses) — **H**,
//! not Codex-specific.

use serde::Serialize;
use xlightcli_protocol::{
    ContentBlock, ImageSource, Message, OpaqueBlob, ProviderId, ReasoningEffort, Role,
    ToolDefinition, ToolResultPart, TransportId, TurnRequest,
};

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct ResponsesRequestBody {
    pub model: String,
    pub instructions: String,
    pub input: Vec<InputItem>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolSpec>,
    pub tool_choice: &'static str,
    pub parallel_tool_calls: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningParam>,
    pub store: bool,
    pub stream: bool,
    pub include: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum InputItem {
    Message {
        role: String,
        content: Vec<InputContentPart>,
    },
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        call_id: String,
        output: String,
    },
    Reasoning {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        summary: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum InputContentPart {
    InputText { text: String },
    InputImage { image_url: String },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct ToolSpec {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub strict: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct ReasoningParam {
    pub effort: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<&'static str>,
}

/// Builds the wire body for one turn. `provider`/`transport` gate which `OpaqueBlob`s get
/// replayed (PATTERNS.md §5: "only replay a blob whose (provider, transport) matches").
pub(crate) fn build_request_body(
    req: &TurnRequest,
    model: &str,
    provider: &ProviderId,
    transport: &TransportId,
) -> ResponsesRequestBody {
    let input = req
        .messages
        .iter()
        .flat_map(|m| translate_message(m, provider, transport))
        .collect();
    let tools: Vec<ToolSpec> = req.tools.iter().map(translate_tool).collect();
    let reasoning = req.reasoning.as_ref().map(|r| ReasoningParam {
        effort: map_effort(r.effort),
        summary: r.include_text.then_some("auto"),
    });
    ResponsesRequestBody {
        model: model.to_string(),
        instructions: req.system.text.clone(),
        input,
        parallel_tool_calls: !tools.is_empty(),
        tools,
        tool_choice: "auto",
        reasoning,
        store: false,
        stream: true,
        include: vec!["reasoning.encrypted_content".to_string()],
    }
}

fn map_effort(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Minimal => "minimal",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
    }
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

/// Translates one canonical `Message` into zero or more wire input items, preserving the
/// original block order: contiguous `Text`/`Image` blocks become one `message` item, while
/// `Reasoning`/`ToolUse`/`ToolResult` each become their own item at the point they occur.
fn translate_message(
    msg: &Message,
    provider: &ProviderId,
    transport: &TransportId,
) -> Vec<InputItem> {
    let role = role_str(msg.role);
    let mut items = Vec::new();
    let mut pending_text: Vec<InputContentPart> = Vec::new();
    for block in &msg.content {
        match block {
            ContentBlock::Text { text } => {
                pending_text.push(InputContentPart::InputText { text: text.clone() });
            }
            ContentBlock::Image { media_type, source } => {
                pending_text.push(image_content_part(media_type, source));
            }
            ContentBlock::Reasoning { opaque, .. } => {
                flush_text(&mut items, &mut pending_text, role);
                if let Some(item) = opaque
                    .as_ref()
                    .filter(|blob| &blob.provider == provider && &blob.transport == transport)
                    .and_then(reasoning_input_item_from_blob)
                {
                    items.push(item);
                }
            }
            ContentBlock::ToolUse {
                id, name, input, ..
            } => {
                flush_text(&mut items, &mut pending_text, role);
                items.push(InputItem::FunctionCall {
                    call_id: id.as_str().to_string(),
                    name: name.clone(),
                    arguments: input.to_string(),
                });
            }
            ContentBlock::ToolResult {
                call_id,
                content,
                is_error,
            } => {
                flush_text(&mut items, &mut pending_text, role);
                items.push(InputItem::FunctionCallOutput {
                    call_id: call_id.as_str().to_string(),
                    output: tool_result_output(content, *is_error),
                });
            }
        }
    }
    flush_text(&mut items, &mut pending_text, role);
    items
}

fn flush_text(items: &mut Vec<InputItem>, pending: &mut Vec<InputContentPart>, role: &str) {
    if !pending.is_empty() {
        items.push(InputItem::Message {
            role: role.to_string(),
            content: std::mem::take(pending),
        });
    }
}

fn image_content_part(media_type: &str, source: &ImageSource) -> InputContentPart {
    match source {
        ImageSource::Base64 { data } => InputContentPart::InputImage {
            image_url: format!("data:{media_type};base64,{data}"),
        },
        ImageSource::Artifact { path } => {
            // Phase 0 limitation: the wire request builder has no IO (PATTERNS.md §5: translator
            // is a pure state machine), so it cannot read an artifact file itself. A future phase
            // resolves artifact images before they reach the transport.
            tracing::debug!(
                path = %path,
                "codex wire: artifact image not embedded (Phase 0 limitation), sending a text placeholder"
            );
            InputContentPart::InputText {
                text: format!("[image omitted: artifact {path} not embedded]"),
            }
        }
    }
}

fn tool_result_output(parts: &[ToolResultPart], is_error: bool) -> String {
    let joined = parts
        .iter()
        .filter_map(|p| match p {
            ToolResultPart::Text { text } => Some(text.clone()),
            // Responses `function_call_output.output` is a plain string (Phase 0 limitation);
            // an image tool result has no wire representation here yet.
            ToolResultPart::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if is_error {
        format!("Error: {joined}")
    } else {
        joined
    }
}

/// Rebuilds a `reasoning` input item from the `OpaqueBlob` produced by `wire::response`
/// (`data: {id, summary, encrypted_content}`). Malformed/foreign-shaped data is dropped rather
/// than guessed (PATTERNS.md §5).
fn reasoning_input_item_from_blob(blob: &OpaqueBlob) -> Option<InputItem> {
    let id = blob
        .data
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let summary = blob
        .data
        .get("summary")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let encrypted_content = blob
        .data
        .get("encrypted_content")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Some(InputItem::Reasoning {
        id,
        summary,
        encrypted_content,
    })
}

fn translate_tool(def: &ToolDefinition) -> ToolSpec {
    ToolSpec {
        kind: "function",
        name: def.name.clone(),
        description: def.description.clone(),
        parameters: def.input_schema.clone(),
        strict: false,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;
    use xlightcli_protocol::{ModelId, ReasoningConfig, SystemPrompt, ToolCallId};

    use super::*;

    fn provider() -> ProviderId {
        ProviderId::new("codex")
    }
    fn transport() -> TransportId {
        TransportId::new("chatgpt")
    }

    #[test]
    fn simple_text_turn_has_one_user_message_item() {
        let req = TurnRequest::simple(ModelId::new("gpt-5-codex"), "hello");
        let body = build_request_body(&req, "gpt-5-codex", &provider(), &transport());
        assert_eq!(body.model, "gpt-5-codex");
        assert!(!body.store);
        assert!(body.stream);
        assert_eq!(
            body.include,
            vec!["reasoning.encrypted_content".to_string()]
        );
        assert_eq!(body.input.len(), 1);
        match &body.input[0] {
            InputItem::Message { role, content } => {
                assert_eq!(role, "user");
                assert_eq!(
                    content,
                    &vec![InputContentPart::InputText {
                        text: "hello".into()
                    }]
                );
            }
            other => panic!("expected Message, got {other:?}"),
        }
    }

    #[test]
    fn reasoning_config_maps_effort_and_summary() {
        let mut req = TurnRequest::simple(ModelId::new("gpt-5-codex"), "hi");
        req.reasoning = Some(ReasoningConfig {
            effort: ReasoningEffort::High,
            include_text: true,
        });
        let body = build_request_body(&req, "gpt-5-codex", &provider(), &transport());
        let reasoning = body.reasoning.unwrap();
        assert_eq!(reasoning.effort, "high");
        assert_eq!(reasoning.summary, Some("auto"));
    }

    #[test]
    fn tool_definitions_become_flat_function_specs() {
        let mut req = TurnRequest::simple(ModelId::new("gpt-5-codex"), "hi");
        req.tools.push(ToolDefinition {
            name: "read_file".into(),
            description: "Read a file".into(),
            input_schema: serde_json::json!({"type": "object"}),
        });
        let body = build_request_body(&req, "gpt-5-codex", &provider(), &transport());
        assert!(body.parallel_tool_calls);
        assert_eq!(body.tools.len(), 1);
        assert_eq!(body.tools[0].kind, "function");
        assert_eq!(body.tools[0].name, "read_file");
    }

    #[test]
    fn tool_use_and_tool_result_round_trip_as_function_call_items() {
        let req = TurnRequest {
            model: ModelId::new("gpt-5-codex"),
            system: SystemPrompt::default(),
            messages: vec![
                Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::ToolUse {
                        id: ToolCallId::new("call-1"),
                        name: "read_file".into(),
                        input: serde_json::json!({"path": "a.rs"}),
                        opaque: None,
                    }],
                },
                Message {
                    role: Role::User,
                    content: vec![ContentBlock::ToolResult {
                        call_id: ToolCallId::new("call-1"),
                        content: vec![ToolResultPart::Text {
                            text: "contents".into(),
                        }],
                        is_error: false,
                    }],
                },
            ],
            tools: vec![],
            reasoning: None,
            max_output_tokens: None,
            provider_options: Default::default(),
        };
        let body = build_request_body(&req, "gpt-5-codex", &provider(), &transport());
        assert_eq!(body.input.len(), 2);
        match &body.input[0] {
            InputItem::FunctionCall {
                call_id,
                name,
                arguments,
            } => {
                assert_eq!(call_id, "call-1");
                assert_eq!(name, "read_file");
                assert_eq!(arguments, "{\"path\":\"a.rs\"}");
            }
            other => panic!("expected FunctionCall, got {other:?}"),
        }
        match &body.input[1] {
            InputItem::FunctionCallOutput { call_id, output } => {
                assert_eq!(call_id, "call-1");
                assert_eq!(output, "contents");
            }
            other => panic!("expected FunctionCallOutput, got {other:?}"),
        }
    }

    #[test]
    fn tool_result_error_is_prefixed() {
        let req = TurnRequest {
            model: ModelId::new("gpt-5-codex"),
            system: SystemPrompt::default(),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    call_id: ToolCallId::new("call-1"),
                    content: vec![ToolResultPart::Text {
                        text: "boom".into(),
                    }],
                    is_error: true,
                }],
            }],
            tools: vec![],
            reasoning: None,
            max_output_tokens: None,
            provider_options: Default::default(),
        };
        let body = build_request_body(&req, "gpt-5-codex", &provider(), &transport());
        match &body.input[0] {
            InputItem::FunctionCallOutput { output, .. } => assert_eq!(output, "Error: boom"),
            other => panic!("expected FunctionCallOutput, got {other:?}"),
        }
    }

    #[test]
    fn opaque_blob_is_replayed_only_for_matching_provider_and_transport() {
        let matching = OpaqueBlob {
            provider: provider(),
            transport: transport(),
            data: serde_json::json!({"id": "rs_1", "summary": [], "encrypted_content": "enc-1"}),
        };
        let foreign = OpaqueBlob {
            provider: ProviderId::new("claude"),
            transport: TransportId::new("anthropic-api"),
            data: serde_json::json!({"id": "rs_2", "summary": [], "encrypted_content": "enc-2"}),
        };
        let req = TurnRequest {
            model: ModelId::new("gpt-5-codex"),
            system: SystemPrompt::default(),
            messages: vec![
                Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Reasoning {
                        text: None,
                        opaque: Some(matching),
                    }],
                },
                Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Reasoning {
                        text: None,
                        opaque: Some(foreign),
                    }],
                },
            ],
            tools: vec![],
            reasoning: None,
            max_output_tokens: None,
            provider_options: Default::default(),
        };
        let body = build_request_body(&req, "gpt-5-codex", &provider(), &transport());
        // The foreign-transport blob must never be replayed.
        assert_eq!(body.input.len(), 1);
        match &body.input[0] {
            InputItem::Reasoning {
                encrypted_content, ..
            } => assert_eq!(encrypted_content.as_deref(), Some("enc-1")),
            other => panic!("expected Reasoning, got {other:?}"),
        }
    }

    #[test]
    fn artifact_image_becomes_a_text_placeholder() {
        let req = TurnRequest {
            model: ModelId::new("gpt-5-codex"),
            system: SystemPrompt::default(),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Image {
                    media_type: "image/png".into(),
                    source: ImageSource::Artifact {
                        path: "/tmp/x.png".into(),
                    },
                }],
            }],
            tools: vec![],
            reasoning: None,
            max_output_tokens: None,
            provider_options: Default::default(),
        };
        let body = build_request_body(&req, "gpt-5-codex", &provider(), &transport());
        match &body.input[0] {
            InputItem::Message { content, .. } => match &content[0] {
                InputContentPart::InputText { text } => assert!(text.contains("/tmp/x.png")),
                other => panic!("expected InputText placeholder, got {other:?}"),
            },
            other => panic!("expected Message, got {other:?}"),
        }
    }

    #[test]
    fn request_body_serializes_to_the_expected_wire_shape() {
        let mut req = TurnRequest::simple(ModelId::new("gpt-5-codex"), "Reply with hello");
        req.reasoning = Some(ReasoningConfig {
            effort: ReasoningEffort::Medium,
            include_text: false,
        });
        let body = build_request_body(&req, "gpt-5-codex", &provider(), &transport());
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "model": "gpt-5-codex",
                "instructions": "",
                "input": [
                    {"type": "message", "role": "user", "content": [
                        {"type": "input_text", "text": "Reply with hello"}
                    ]}
                ],
                "tool_choice": "auto",
                "parallel_tool_calls": false,
                "reasoning": {"effort": "medium"},
                "store": false,
                "stream": true,
                "include": ["reasoning.encrypted_content"]
            })
        );
    }
}
