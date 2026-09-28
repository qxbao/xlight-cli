// SPDX-License-Identifier: GPL-3.0-only

//! Canonical conversation types (docs/PLAN.md §4.3): `Message`, `ContentBlock` and friends.
//!
//! These are the only conversation types the agent runtime ever sees (INV-3): provider wire
//! JSON is translated to/from these types inside each `provider-*` crate and never leaks out.

use serde::{Deserialize, Serialize};

use crate::ids::{ProviderId, ToolCallId, TransportId};

/// Message role in a canonical conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
}

/// A single canonical message, made of one or more content blocks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }
}

/// One block of message content. A `Message` is `Vec<ContentBlock>` rather than a single enum
/// variant because a turn can interleave text, reasoning, tool calls and tool results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        media_type: String,
        source: ImageSource,
    },
    /// Reasoning/thinking content. `opaque` carries continuity data (D-009) that only the
    /// producing `(provider, transport)` can replay.
    Reasoning {
        text: Option<String>,
        opaque: Option<OpaqueBlob>,
    },
    /// `opaque` carries per-tool-call continuity data (D-009), e.g. Gemini's `thoughtSignature`
    /// attached directly to a `functionCall` part — analogous to `Reasoning.opaque` but scoped to
    /// this one call rather than a reasoning block. `#[serde(default, skip_serializing_if =
    /// "Option::is_none")]` so payloads persisted before this field existed still deserialize
    /// (and old-shape output stays byte-identical when there's nothing to carry).
    ToolUse {
        id: ToolCallId,
        name: String,
        input: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        opaque: Option<OpaqueBlob>,
    },
    ToolResult {
        call_id: ToolCallId,
        content: Vec<ToolResultPart>,
        is_error: bool,
    },
}

/// Where an image's bytes live. Large images go through the artifact store (INV-7); only small
/// images are inlined as base64.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    /// Small inline image (already base64-encoded).
    Base64 { data: String },
    /// Reference to an artifact file managed by `OutputSpool` / the artifact store.
    Artifact { path: String },
}

/// One part of a tool result's content (a tool can return text and/or images).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultPart {
    Text {
        text: String,
    },
    Image {
        media_type: String,
        source: ImageSource,
    },
}

/// Provider-specific continuity data: Claude thinking signatures, Codex encrypted reasoning,
/// Gemini thought signatures, etc. (D-009). Core treats this as opaque; when building the next
/// `TurnRequest`, a transport may only replay a blob whose `(provider, transport)` matches its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpaqueBlob {
    pub provider: ProviderId,
    pub transport: TransportId,
    pub data: serde_json::Value,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn message_roundtrips_through_json() {
        let msg = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "hello".into(),
                },
                ContentBlock::Reasoning {
                    text: Some("thinking...".into()),
                    opaque: Some(OpaqueBlob {
                        provider: ProviderId::new("claude"),
                        transport: TransportId::new("anthropic-api"),
                        data: serde_json::json!({"sig": "abc"}),
                    }),
                },
                ContentBlock::ToolUse {
                    id: ToolCallId::new("call-1"),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "a.rs"}),
                    opaque: Some(OpaqueBlob {
                        provider: ProviderId::new("agy"),
                        transport: TransportId::new("antigravity"),
                        data: serde_json::json!({"thoughtSignature": "sig"}),
                    }),
                },
                ContentBlock::ToolResult {
                    call_id: ToolCallId::new("call-1"),
                    content: vec![ToolResultPart::Text {
                        text: "contents".into(),
                    }],
                    is_error: false,
                },
            ],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn tool_use_without_opaque_omits_the_field_and_old_payloads_still_deserialize() {
        let block = ContentBlock::ToolUse {
            id: ToolCallId::new("call-1"),
            name: "read_file".into(),
            input: serde_json::json!({"path": "a.rs"}),
            opaque: None,
        };
        let json = serde_json::to_value(&block).unwrap();
        assert!(
            json.get("opaque").is_none(),
            "opaque: None must not appear in the serialized output"
        );

        // A payload persisted before `opaque` existed (no such key at all) must still parse.
        let pre_existing_payload = serde_json::json!({
            "type": "tool_use",
            "id": "call-1",
            "name": "read_file",
            "input": {"path": "a.rs"},
        });
        let parsed: ContentBlock = serde_json::from_value(pre_existing_payload).unwrap();
        assert_eq!(parsed, block);
    }

    #[test]
    fn image_source_variants_roundtrip() {
        for src in [
            ImageSource::Base64 {
                data: "QQ==".into(),
            },
            ImageSource::Artifact {
                path: "/tmp/x.png".into(),
            },
        ] {
            let json = serde_json::to_string(&src).unwrap();
            let back: ImageSource = serde_json::from_str(&json).unwrap();
            assert_eq!(back, src);
        }
    }
}
