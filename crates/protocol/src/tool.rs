// SPDX-License-Identifier: GPL-3.0-only

//! Canonical tool definition sent to a transport as part of a `TurnRequest`.

use serde::{Deserialize, Serialize};

/// Describes one tool the model may call. `input_schema` is generated from the tool's Rust
/// input struct via `schemars` in crate `tools`; `protocol` only carries the resulting value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn tool_definition_roundtrips_through_json() {
        let def = ToolDefinition {
            name: "read_file".into(),
            description: "Read a file".into(),
            input_schema: serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        };
        let json = serde_json::to_string(&def).unwrap();
        let back: ToolDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(back, def);
    }
}
