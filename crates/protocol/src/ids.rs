// SPDX-License-Identifier: GPL-3.0-only

//! Canonical identifier newtypes (CODEBASE.md §2).
//!
//! Wrapping every id in its own type prevents call sites from accidentally swapping e.g. a
//! `ProviderId` for a `TransportId` — a class of bug that is easy to introduce when both are
//! plain `String`s. No IO, no validation beyond "non-empty" is performed here.

use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }
    };
}

string_id!(
    /// Provider identifier, e.g. `"codex"`, `"claude"`, `"agy"`.
    ProviderId
);
string_id!(
    /// Transport identifier within a provider, e.g. `"chatgpt"`, `"anthropic-api"`.
    TransportId
);
string_id!(
    /// Model identifier as understood by a transport; opaque to core (docs/PLAN.md §4.1).
    ModelId
);
string_id!(
    /// Tool call identifier assigned by the provider wire protocol.
    ToolCallId
);

macro_rules! uuid_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(uuid::Uuid);

        impl $name {
            /// Generates a new random (v4) id.
            pub fn new() -> Self {
                Self(uuid::Uuid::new_v4())
            }

            pub fn from_uuid(id: uuid::Uuid) -> Self {
                Self(id)
            }

            pub fn as_uuid(&self) -> uuid::Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

uuid_id!(
    /// Persisted session identifier (`storage::sessions`).
    SessionId
);
uuid_id!(
    /// Identifier of one agent = one async task (INV-5).
    AgentId
);

/// Namespaced command identifier, e.g. `"core.mcp"` or `"claude.insights"` (PATTERNS.md §12).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CommandId(String);

impl CommandId {
    /// Builds a namespaced id from `(namespace, name)`, e.g. `CommandId::new("claude", "insights")`.
    pub fn new(namespace: impl AsRef<str>, name: impl AsRef<str>) -> Self {
        Self(format!("{}.{}", namespace.as_ref(), name.as_ref()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the part before the first `.`, e.g. `"claude"` for `"claude.insights"`.
    pub fn namespace(&self) -> &str {
        self.0.split('.').next().unwrap_or(&self.0)
    }
}

impl fmt::Display for CommandId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn string_id_roundtrips_through_json() {
        let id = ProviderId::new("codex");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"codex\"");
        let back: ProviderId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn uuid_id_roundtrips_through_json() {
        let id = SessionId::new();
        let json = serde_json::to_string(&id).unwrap();
        let back: SessionId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn command_id_namespace() {
        let id = CommandId::new("claude", "insights");
        assert_eq!(id.as_str(), "claude.insights");
        assert_eq!(id.namespace(), "claude");
    }
}
