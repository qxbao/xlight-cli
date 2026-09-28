// SPDX-License-Identifier: GPL-3.0-only

//! Row types for the event-sourced schema (`migrations/0002_sessions.sql`, docs/PLAN.md §11.2).
//!
//! Every JSON column stores a canonical `xlightcli_protocol` type (never a provider's wire JSON,
//! PATTERNS.md §10): `payload_json` in `events` is `serde_json::to_value(&AgentEvent)` (or, for
//! kinds with no direct `AgentEvent` equivalent — e.g. `AgentCompleted` — a small ad hoc struct),
//! `content_json` in `messages` is `Vec<ContentBlock>`, `input_json`/`profile_json` are plain
//! `serde_json::Value`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use xlightcli_protocol::{AgentId, ModelId, ProviderId, Role, SessionId, TransportId, WorkspaceId};

/// `workspaces` row.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceRecord {
    pub id: WorkspaceId,
    pub root: PathBuf,
    pub repo_id: Option<String>,
    pub created_at: OffsetDateTime,
}

/// `sessions.status` (docs/PLAN.md §11.2: "an in-progress turn is marked Interrupted").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Completed,
    Interrupted,
    Failed,
}

impl SessionStatus {
    pub(crate) fn as_db_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Completed => "completed",
            Self::Interrupted => "interrupted",
            Self::Failed => "failed",
        }
    }

    pub(crate) fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "active" => Some(Self::Active),
            "completed" => Some(Self::Completed),
            "interrupted" => Some(Self::Interrupted),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// `sessions` row.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRecord {
    pub id: SessionId,
    pub workspace_id: WorkspaceId,
    pub provider: ProviderId,
    pub transport: TransportId,
    pub model: ModelId,
    pub title: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub status: SessionStatus,
}

/// `agents.state` (docs/PLAN.md §10.1 `AgentState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Queued,
    Running,
    WaitingTool,
    WaitingPermission,
    Done,
    Failed,
    Cancelled,
}

impl AgentState {
    pub(crate) fn as_db_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::WaitingTool => "waiting_tool",
            Self::WaitingPermission => "waiting_permission",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub(crate) fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "waiting_tool" => Some(Self::WaitingTool),
            "waiting_permission" => Some(Self::WaitingPermission),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// `agents` row.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentRecord {
    pub id: AgentId,
    pub session_id: SessionId,
    pub parent_id: Option<AgentId>,
    pub profile: serde_json::Value,
    pub state: AgentState,
    pub created_at: OffsetDateTime,
    pub finished_at: Option<OffsetDateTime>,
}

/// `events.kind` — the event-oriented lifecycle from docs/PLAN.md §11.2:
/// `TurnStarted -> TextDelta* (batched by chunk) -> ToolCalled -> ToolCompleted -> TurnCompleted
/// -> AgentCompleted`. Distinct from `xlightcli_protocol::AgentEvent` (which is the wire-facing
/// streaming enum): `StoredEventKind` is the persisted lifecycle tag used to reconstruct
/// `messages` after a crash, so it also covers events that never come from a provider stream
/// (`ToolCalled`/`ToolCompleted`/`AgentCompleted`, emitted by the runtime's tool executor /
/// scheduler, not by `TransportAdapter::stream`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredEventKind {
    TurnStarted,
    TextDelta,
    ReasoningDelta,
    ToolCallStarted,
    ToolCalled,
    ToolCompleted,
    Usage,
    RateLimit,
    TurnCompleted,
    AgentCompleted,
    Interrupted,
}

impl StoredEventKind {
    pub(crate) fn as_db_str(self) -> &'static str {
        match self {
            Self::TurnStarted => "turn_started",
            Self::TextDelta => "text_delta",
            Self::ReasoningDelta => "reasoning_delta",
            Self::ToolCallStarted => "tool_call_started",
            Self::ToolCalled => "tool_called",
            Self::ToolCompleted => "tool_completed",
            Self::Usage => "usage",
            Self::RateLimit => "rate_limit",
            Self::TurnCompleted => "turn_completed",
            Self::AgentCompleted => "agent_completed",
            Self::Interrupted => "interrupted",
        }
    }

    pub(crate) fn from_db_str(s: &str) -> Option<Self> {
        Some(match s {
            "turn_started" => Self::TurnStarted,
            "text_delta" => Self::TextDelta,
            "reasoning_delta" => Self::ReasoningDelta,
            "tool_call_started" => Self::ToolCallStarted,
            "tool_called" => Self::ToolCalled,
            "tool_completed" => Self::ToolCompleted,
            "usage" => Self::Usage,
            "rate_limit" => Self::RateLimit,
            "turn_completed" => Self::TurnCompleted,
            "agent_completed" => Self::AgentCompleted,
            "interrupted" => Self::Interrupted,
            _ => return None,
        })
    }
}

/// One event to append, before a `seq`/`id`/timestamp is assigned (`Storage::append_events`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewEvent {
    pub agent_id: AgentId,
    pub kind: StoredEventKind,
    /// Canonical payload (never provider wire JSON) — typically
    /// `serde_json::to_value(&AgentEvent).expect("AgentEvent always serializes")`.
    pub payload: serde_json::Value,
}

/// `events` row, as read back.
#[derive(Debug, Clone, PartialEq)]
pub struct EventRecord {
    pub id: i64,
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub seq: i64,
    pub kind: StoredEventKind,
    pub payload: serde_json::Value,
    pub created_at: OffsetDateTime,
}

/// `messages` row (a projection; see the module doc in `crate::storage` for how it's populated in
/// Phase 1 Wave A).
#[derive(Debug, Clone, PartialEq)]
pub struct MessageRecord {
    pub id: i64,
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub turn: i64,
    pub role: Role,
    pub content: Vec<xlightcli_protocol::ContentBlock>,
    pub created_at: OffsetDateTime,
}

/// Pagination cursor for [`crate::storage::Storage::load_messages`]: returns up to `limit`
/// messages with `id < before_id` (or the most recent `limit` when `before_id` is `None`),
/// ordered newest-first — the shape `ContextManager` needs to lazily page in older history
/// (docs/PLAN.md §9.2 "Old sessions are not loaded entirely back into RAM").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MessagePage {
    pub before_id: Option<i64>,
    pub limit: u32,
}

/// `tool_calls.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl ToolCallStatus {
    pub(crate) fn as_db_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub(crate) fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "running" => Some(Self::Running),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// A tool call to record before it starts running.
#[derive(Debug, Clone)]
pub struct NewToolCall {
    pub agent_id: AgentId,
    /// The `ToolCallId` assigned by the provider wire protocol (stored as text: `tool_calls`
    /// doesn't use `xlightcli_protocol::ToolCallId` as its primary key because a provider may
    /// reuse call ids across turns — `id` is the surrogate, unique key).
    pub call_id: xlightcli_protocol::ToolCallId,
    pub name: String,
    pub input: serde_json::Value,
}

/// `tool_calls` row.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallRecord {
    pub id: i64,
    pub agent_id: AgentId,
    pub call_id: xlightcli_protocol::ToolCallId,
    pub name: String,
    pub input: serde_json::Value,
    pub status: ToolCallStatus,
    pub artifact_id: Option<i64>,
    pub started_at: OffsetDateTime,
    pub finished_at: Option<OffsetDateTime>,
}

/// An artifact to register (the tool/spool already wrote the file; this just indexes it).
#[derive(Debug, Clone)]
pub struct NewArtifact {
    pub session_id: SessionId,
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
    pub kind: String,
}

/// `artifacts` row.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactRecord {
    pub id: i64,
    pub session_id: SessionId,
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
    pub kind: String,
    pub created_at: OffsetDateTime,
}

/// A usage row to record (`provider_usage`). `usage` is the canonical
/// `xlightcli_protocol::Usage` from an `AgentEvent::Usage`/`Completed`.
#[derive(Debug, Clone)]
pub struct NewUsageRow {
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub transport: TransportId,
    pub model: ModelId,
    pub usage: xlightcli_protocol::Usage,
}

/// `provider_usage` row.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageRecord {
    pub id: i64,
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub transport: TransportId,
    pub model: ModelId,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub at: OffsetDateTime,
}
