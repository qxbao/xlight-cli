// SPDX-License-Identifier: GPL-3.0-only

//! The persistence writer thread (PATTERNS.md §10): the **only** place that holds the write
//! `rusqlite::Connection`. Every write goes through [`StorageCmd`] on a bounded
//! `tokio::sync::mpsc` channel; [`crate::storage::Storage`] is the async-facing wrapper that
//! sends commands and awaits their `oneshot` reply.
//!
//! **Scope decision (documented, Phase 1 Wave A):** each [`StorageCmd`] commits in its own sqlite
//! transaction (a multi-row command like [`StorageCmd::AppendEvents`] is still atomic — one
//! transaction for all of its rows). Coalescing *several distinct* `StorageCmd`s that arrived
//! close together into a single shared transaction (the "N events or 50 ms" batching PATTERNS.md
//! §10 describes) is a pure writer-thread implementation detail, invisible to every caller in this
//! file — it can be added in a later wave without changing [`StorageCmd`] or
//! `Storage`'s public API.

use rusqlite::{Connection, OptionalExtension, params};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::oneshot;
use xlightcli_protocol::{AgentId, ModelId, ProviderId, Role, SessionId, TransportId, WorkspaceId};

use crate::error::StorageError;
use crate::records::{
    AgentState, ArtifactRecord, EventRecord, MessagePage, MessageRecord, NewArtifact, NewEvent,
    NewToolCall, NewUsageRow, SessionRecord, SessionStatus, StoredEventKind, ToolCallRecord,
    ToolCallStatus, UsageRecord, WorkspaceRecord,
};

fn now() -> String {
    crate::accounts::now_rfc3339()
}

fn parse_time(s: &str) -> Result<OffsetDateTime, StorageError> {
    OffsetDateTime::parse(s, &Rfc3339)
        .map_err(|e| StorageError::InvalidStoredData(format!("timestamp: {e}")))
}

/// One write request. Every variant carries a `oneshot::Sender` for its reply; the writer thread
/// always sends exactly one reply per command (even on error), so callers never hang.
pub(crate) enum StorageCmd {
    CreateWorkspace {
        root: std::path::PathBuf,
        repo_id: Option<String>,
        reply: oneshot::Sender<Result<WorkspaceId, StorageError>>,
    },
    CreateSession {
        workspace_id: WorkspaceId,
        provider: ProviderId,
        transport: TransportId,
        model: ModelId,
        title: Option<String>,
        reply: oneshot::Sender<Result<SessionId, StorageError>>,
    },
    UpdateSessionStatus {
        session_id: SessionId,
        status: SessionStatus,
        reply: oneshot::Sender<Result<(), StorageError>>,
    },
    RegisterAgent {
        session_id: SessionId,
        parent_id: Option<AgentId>,
        profile: serde_json::Value,
        reply: oneshot::Sender<Result<AgentId, StorageError>>,
    },
    AppendEvents {
        session_id: SessionId,
        events: Vec<NewEvent>,
        reply: oneshot::Sender<Result<Vec<i64>, StorageError>>,
    },
    AppendMessage {
        session_id: SessionId,
        agent_id: AgentId,
        turn: i64,
        role: Role,
        content: Vec<xlightcli_protocol::ContentBlock>,
        reply: oneshot::Sender<Result<i64, StorageError>>,
    },
    RecordToolCall {
        call: NewToolCall,
        reply: oneshot::Sender<Result<i64, StorageError>>,
    },
    FinishToolCall {
        id: i64,
        status: ToolCallStatus,
        artifact_id: Option<i64>,
        reply: oneshot::Sender<Result<(), StorageError>>,
    },
    RegisterArtifact {
        artifact: NewArtifact,
        reply: oneshot::Sender<Result<i64, StorageError>>,
    },
    RecordUsage {
        row: NewUsageRow,
        reply: oneshot::Sender<Result<i64, StorageError>>,
    },
    MarkInterruptedOnOpen {
        reply: oneshot::Sender<Result<u64, StorageError>>,
    },
    // --- reads that must observe the writer's own uncommitted-by-nobody-else state exactly
    // (list/get session): cheap enough, and simpler than a second connection racing the writer
    // for these two. Bulk/paged reads (`load_messages`, `list_artifacts`) go through the
    // dedicated reader connection instead (`crate::storage::Storage`).
    ListSessions {
        workspace_id: Option<WorkspaceId>,
        reply: oneshot::Sender<Result<Vec<SessionRecord>, StorageError>>,
    },
    GetSession {
        session_id: SessionId,
        reply: oneshot::Sender<Result<Option<SessionRecord>, StorageError>>,
    },
}

/// Runs on a dedicated OS thread (spawned by `Storage::open`); blocks on `rx.blocking_recv()`
/// until the channel is closed (every `Storage` clone dropped), then returns.
pub(crate) fn run(mut conn: Connection, mut rx: tokio::sync::mpsc::Receiver<StorageCmd>) {
    while let Some(cmd) = rx.blocking_recv() {
        apply(&mut conn, cmd);
    }
}

fn apply(conn: &mut Connection, cmd: StorageCmd) {
    match cmd {
        StorageCmd::CreateWorkspace {
            root,
            repo_id,
            reply,
        } => {
            let _ = reply.send(create_workspace(conn, &root, repo_id.as_deref()));
        }
        StorageCmd::CreateSession {
            workspace_id,
            provider,
            transport,
            model,
            title,
            reply,
        } => {
            let _ = reply.send(create_session(
                conn,
                &workspace_id,
                &provider,
                &transport,
                &model,
                title.as_deref(),
            ));
        }
        StorageCmd::UpdateSessionStatus {
            session_id,
            status,
            reply,
        } => {
            let _ = reply.send(update_session_status(conn, &session_id, status));
        }
        StorageCmd::RegisterAgent {
            session_id,
            parent_id,
            profile,
            reply,
        } => {
            let _ = reply.send(register_agent(
                conn,
                &session_id,
                parent_id.as_ref(),
                &profile,
            ));
        }
        StorageCmd::AppendEvents {
            session_id,
            events,
            reply,
        } => {
            let _ = reply.send(append_events(conn, &session_id, events));
        }
        StorageCmd::AppendMessage {
            session_id,
            agent_id,
            turn,
            role,
            content,
            reply,
        } => {
            let _ = reply.send(append_message(
                conn,
                &session_id,
                &agent_id,
                turn,
                role,
                &content,
            ));
        }
        StorageCmd::RecordToolCall { call, reply } => {
            let _ = reply.send(record_tool_call(conn, call));
        }
        StorageCmd::FinishToolCall {
            id,
            status,
            artifact_id,
            reply,
        } => {
            let _ = reply.send(finish_tool_call(conn, id, status, artifact_id));
        }
        StorageCmd::RegisterArtifact { artifact, reply } => {
            let _ = reply.send(register_artifact(conn, artifact));
        }
        StorageCmd::RecordUsage { row, reply } => {
            let _ = reply.send(record_usage(conn, row));
        }
        StorageCmd::MarkInterruptedOnOpen { reply } => {
            let _ = reply.send(mark_interrupted_on_open(conn));
        }
        StorageCmd::ListSessions {
            workspace_id,
            reply,
        } => {
            let _ = reply.send(list_sessions(conn, workspace_id));
        }
        StorageCmd::GetSession { session_id, reply } => {
            let _ = reply.send(get_session(conn, &session_id));
        }
    }
}

fn create_workspace(
    conn: &Connection,
    root: &std::path::Path,
    repo_id: Option<&str>,
) -> Result<WorkspaceId, StorageError> {
    let id = WorkspaceId::new();
    conn.execute(
        "INSERT INTO workspaces (id, root, repo_id, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![
            id.as_uuid().to_string(),
            root.to_string_lossy(),
            repo_id,
            now()
        ],
    )?;
    Ok(id)
}

fn create_session(
    conn: &Connection,
    workspace_id: &WorkspaceId,
    provider: &ProviderId,
    transport: &TransportId,
    model: &ModelId,
    title: Option<&str>,
) -> Result<SessionId, StorageError> {
    let id = SessionId::new();
    let now = now();
    conn.execute(
        "INSERT INTO sessions
            (id, workspace_id, provider, transport, model, title, created_at, updated_at, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8)",
        params![
            id.as_uuid().to_string(),
            workspace_id.as_uuid().to_string(),
            provider.as_str(),
            transport.as_str(),
            model.as_str(),
            title,
            now,
            SessionStatus::Active.as_db_str(),
        ],
    )?;
    Ok(id)
}

fn update_session_status(
    conn: &Connection,
    session_id: &SessionId,
    status: SessionStatus,
) -> Result<(), StorageError> {
    conn.execute(
        "UPDATE sessions SET status = ?1, updated_at = ?2 WHERE id = ?3",
        params![status.as_db_str(), now(), session_id.as_uuid().to_string()],
    )?;
    Ok(())
}

fn register_agent(
    conn: &Connection,
    session_id: &SessionId,
    parent_id: Option<&AgentId>,
    profile: &serde_json::Value,
) -> Result<AgentId, StorageError> {
    let id = AgentId::new();
    conn.execute(
        "INSERT INTO agents (id, session_id, parent_id, profile_json, state, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            id.as_uuid().to_string(),
            session_id.as_uuid().to_string(),
            parent_id.map(|p| p.as_uuid().to_string()),
            profile.to_string(),
            AgentState::Queued.as_db_str(),
            now(),
        ],
    )?;
    Ok(id)
}

fn append_events(
    conn: &mut Connection,
    session_id: &SessionId,
    events: Vec<NewEvent>,
) -> Result<Vec<i64>, StorageError> {
    if events.is_empty() {
        return Ok(Vec::new());
    }
    let tx = conn.transaction()?;
    let first_seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM events WHERE session_id = ?1",
        params![session_id.as_uuid().to_string()],
        |row| row.get(0),
    )?;
    let mut ids = Vec::with_capacity(events.len());
    for (offset, event) in events.into_iter().enumerate() {
        let created_at = now();
        tx.execute(
            "INSERT INTO events (session_id, agent_id, seq, kind, payload_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                session_id.as_uuid().to_string(),
                event.agent_id.as_uuid().to_string(),
                first_seq + offset as i64,
                event.kind.as_db_str(),
                event.payload.to_string(),
                created_at,
            ],
        )?;
        ids.push(tx.last_insert_rowid());
    }
    tx.commit()?;
    Ok(ids)
}

fn append_message(
    conn: &Connection,
    session_id: &SessionId,
    agent_id: &AgentId,
    turn: i64,
    role: Role,
    content: &[xlightcli_protocol::ContentBlock],
) -> Result<i64, StorageError> {
    let role_str = serde_json::to_value(role)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .ok_or_else(|| StorageError::InvalidStoredData("role".to_string()))?;
    let content_json = serde_json::to_string(content)
        .map_err(|e| StorageError::InvalidStoredData(format!("content_json: {e}")))?;
    conn.execute(
        "INSERT INTO messages (session_id, agent_id, turn, role, content_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            session_id.as_uuid().to_string(),
            agent_id.as_uuid().to_string(),
            turn,
            role_str,
            content_json,
            now(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn record_tool_call(conn: &Connection, call: NewToolCall) -> Result<i64, StorageError> {
    conn.execute(
        "INSERT INTO tool_calls (agent_id, call_id, name, input_json, status, started_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            call.agent_id.as_uuid().to_string(),
            call.call_id.as_str(),
            call.name,
            call.input.to_string(),
            ToolCallStatus::Running.as_db_str(),
            now(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn finish_tool_call(
    conn: &Connection,
    id: i64,
    status: ToolCallStatus,
    artifact_id: Option<i64>,
) -> Result<(), StorageError> {
    conn.execute(
        "UPDATE tool_calls SET status = ?1, artifact_id = ?2, finished_at = ?3 WHERE id = ?4",
        params![status.as_db_str(), artifact_id, now(), id],
    )?;
    Ok(())
}

fn register_artifact(conn: &Connection, artifact: NewArtifact) -> Result<i64, StorageError> {
    conn.execute(
        "INSERT INTO artifacts (session_id, path, bytes, sha256, kind, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            artifact.session_id.as_uuid().to_string(),
            artifact.path.to_string_lossy(),
            artifact.bytes as i64,
            artifact.sha256,
            artifact.kind,
            now(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn record_usage(conn: &Connection, row: NewUsageRow) -> Result<i64, StorageError> {
    conn.execute(
        "INSERT INTO provider_usage
            (session_id, agent_id, transport, model, input_tokens, output_tokens, cached_tokens, at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            row.session_id.as_uuid().to_string(),
            row.agent_id.as_uuid().to_string(),
            row.transport.as_str(),
            row.model.as_str(),
            row.usage.input_tokens as i64,
            row.usage.output_tokens as i64,
            row.usage.cached_input_tokens as i64,
            now(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Called once at `Storage::open`: any session still `active` is from a process that crashed
/// mid-turn (docs/PLAN.md §11.2). Returns the number of sessions marked.
fn mark_interrupted_on_open(conn: &Connection) -> Result<u64, StorageError> {
    let updated = conn.execute(
        "UPDATE sessions SET status = ?1, updated_at = ?2 WHERE status = ?3",
        params![
            SessionStatus::Interrupted.as_db_str(),
            now(),
            SessionStatus::Active.as_db_str(),
        ],
    )?;
    Ok(updated as u64)
}

fn session_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Result<SessionRecord, StorageError>> {
    Ok((|| {
        let id: String = row.get(0)?;
        let workspace_id: String = row.get(1)?;
        let provider: String = row.get(2)?;
        let transport: String = row.get(3)?;
        let model: String = row.get(4)?;
        let title: Option<String> = row.get(5)?;
        let created_at: String = row.get(6)?;
        let updated_at: String = row.get(7)?;
        let status: String = row.get(8)?;

        Ok(SessionRecord {
            id: SessionId::from_uuid(
                uuid::Uuid::parse_str(&id)
                    .map_err(|e| StorageError::InvalidStoredData(format!("session id: {e}")))?,
            ),
            workspace_id: WorkspaceId::from_uuid(
                uuid::Uuid::parse_str(&workspace_id)
                    .map_err(|e| StorageError::InvalidStoredData(format!("workspace id: {e}")))?,
            ),
            provider: ProviderId::new(provider),
            transport: TransportId::new(transport),
            model: ModelId::new(model),
            title,
            created_at: parse_time(&created_at)?,
            updated_at: parse_time(&updated_at)?,
            status: SessionStatus::from_db_str(&status).ok_or_else(|| {
                StorageError::InvalidStoredData(format!("session status: {status}"))
            })?,
        })
    })())
}

const SESSION_COLUMNS: &str =
    "id, workspace_id, provider, transport, model, title, created_at, updated_at, status";

fn list_sessions(
    conn: &Connection,
    workspace_id: Option<WorkspaceId>,
) -> Result<Vec<SessionRecord>, StorageError> {
    match workspace_id {
        Some(workspace_id) => {
            let mut stmt = conn.prepare(&format!(
                "SELECT {SESSION_COLUMNS} FROM sessions WHERE workspace_id = ?1 ORDER BY created_at"
            ))?;
            let rows = stmt
                .query_map(
                    params![workspace_id.as_uuid().to_string()],
                    session_from_row,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter().collect()
        }
        None => {
            let mut stmt = conn.prepare(&format!(
                "SELECT {SESSION_COLUMNS} FROM sessions ORDER BY created_at"
            ))?;
            let rows = stmt
                .query_map([], session_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter().collect()
        }
    }
}

fn get_session(
    conn: &Connection,
    session_id: &SessionId,
) -> Result<Option<SessionRecord>, StorageError> {
    conn.query_row(
        &format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE id = ?1"),
        params![session_id.as_uuid().to_string()],
        session_from_row,
    )
    .optional()?
    .transpose()
}

/// Shared with `crate::storage`'s reader-side queries (paged messages, artifacts) since they read
/// the same row shapes.
pub(crate) fn message_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Result<MessageRecord, StorageError>> {
    Ok((|| {
        let id: i64 = row.get(0)?;
        let session_id: String = row.get(1)?;
        let agent_id: String = row.get(2)?;
        let turn: i64 = row.get(3)?;
        let role: String = row.get(4)?;
        let content_json: String = row.get(5)?;
        let created_at: String = row.get(6)?;

        let role: Role = serde_json::from_value(serde_json::Value::String(role))
            .map_err(|e| StorageError::InvalidStoredData(format!("role: {e}")))?;
        let content: Vec<xlightcli_protocol::ContentBlock> = serde_json::from_str(&content_json)
            .map_err(|e| StorageError::InvalidStoredData(format!("content_json: {e}")))?;

        Ok(MessageRecord {
            id,
            session_id: SessionId::from_uuid(
                uuid::Uuid::parse_str(&session_id)
                    .map_err(|e| StorageError::InvalidStoredData(format!("session id: {e}")))?,
            ),
            agent_id: AgentId::from_uuid(
                uuid::Uuid::parse_str(&agent_id)
                    .map_err(|e| StorageError::InvalidStoredData(format!("agent id: {e}")))?,
            ),
            turn,
            role,
            content,
            created_at: parse_time(&created_at)?,
        })
    })())
}

pub(crate) fn load_messages(
    conn: &Connection,
    session_id: &SessionId,
    page: MessagePage,
) -> Result<Vec<MessageRecord>, StorageError> {
    let limit = if page.limit == 0 { 50 } else { page.limit };
    let rows = match page.before_id {
        Some(before_id) => {
            let mut stmt = conn.prepare(
                "SELECT id, session_id, agent_id, turn, role, content_json, created_at
                 FROM messages WHERE session_id = ?1 AND id < ?2 ORDER BY id DESC LIMIT ?3",
            )?;
            stmt.query_map(
                params![session_id.as_uuid().to_string(), before_id, limit],
                message_from_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?
        }
        None => {
            let mut stmt = conn.prepare(
                "SELECT id, session_id, agent_id, turn, role, content_json, created_at
                 FROM messages WHERE session_id = ?1 ORDER BY id DESC LIMIT ?2",
            )?;
            stmt.query_map(
                params![session_id.as_uuid().to_string(), limit],
                message_from_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?
        }
    };
    let mut out = rows.into_iter().collect::<Result<Vec<_>, StorageError>>()?;
    out.reverse(); // oldest-first within the page, matching conversational order
    Ok(out)
}

pub(crate) fn list_artifacts(
    conn: &Connection,
    session_id: &SessionId,
) -> Result<Vec<ArtifactRecord>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, path, bytes, sha256, kind, created_at
         FROM artifacts WHERE session_id = ?1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map(params![session_id.as_uuid().to_string()], |row| {
            Ok((|| {
                let id: i64 = row.get(0)?;
                let session_id: String = row.get(1)?;
                let path: String = row.get(2)?;
                let bytes: i64 = row.get(3)?;
                let sha256: String = row.get(4)?;
                let kind: String = row.get(5)?;
                let created_at: String = row.get(6)?;
                Ok(ArtifactRecord {
                    id,
                    session_id: SessionId::from_uuid(uuid::Uuid::parse_str(&session_id).map_err(
                        |e| StorageError::InvalidStoredData(format!("session id: {e}")),
                    )?),
                    path: std::path::PathBuf::from(path),
                    bytes: bytes as u64,
                    sha256,
                    kind,
                    created_at: parse_time(&created_at)?,
                })
            })())
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().collect()
}

pub(crate) fn list_tool_calls(
    conn: &Connection,
    agent_id: &AgentId,
) -> Result<Vec<ToolCallRecord>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT id, agent_id, call_id, name, input_json, status, artifact_id, started_at, finished_at
         FROM tool_calls WHERE agent_id = ?1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map(params![agent_id.as_uuid().to_string()], |row| {
            Ok((|| {
                let id: i64 = row.get(0)?;
                let agent_id: String = row.get(1)?;
                let call_id: String = row.get(2)?;
                let name: String = row.get(3)?;
                let input_json: String = row.get(4)?;
                let status: String = row.get(5)?;
                let artifact_id: Option<i64> = row.get(6)?;
                let started_at: String = row.get(7)?;
                let finished_at: Option<String> = row.get(8)?;
                Ok(ToolCallRecord {
                    id,
                    agent_id: AgentId::from_uuid(
                        uuid::Uuid::parse_str(&agent_id).map_err(|e| {
                            StorageError::InvalidStoredData(format!("agent id: {e}"))
                        })?,
                    ),
                    call_id: xlightcli_protocol::ToolCallId::new(call_id),
                    name,
                    input: serde_json::from_str(&input_json)
                        .map_err(|e| StorageError::InvalidStoredData(format!("input_json: {e}")))?,
                    status: ToolCallStatus::from_db_str(&status).ok_or_else(|| {
                        StorageError::InvalidStoredData(format!("tool_call status: {status}"))
                    })?,
                    artifact_id,
                    started_at: parse_time(&started_at)?,
                    finished_at: finished_at.map(|s| parse_time(&s)).transpose()?,
                })
            })())
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().collect()
}

pub(crate) fn list_usage(
    conn: &Connection,
    session_id: &SessionId,
) -> Result<Vec<UsageRecord>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, agent_id, transport, model, input_tokens, output_tokens, cached_tokens, at
         FROM provider_usage WHERE session_id = ?1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map(params![session_id.as_uuid().to_string()], |row| {
            Ok((|| {
                let id: i64 = row.get(0)?;
                let session_id: String = row.get(1)?;
                let agent_id: String = row.get(2)?;
                let transport: String = row.get(3)?;
                let model: String = row.get(4)?;
                let input_tokens: i64 = row.get(5)?;
                let output_tokens: i64 = row.get(6)?;
                let cached_tokens: i64 = row.get(7)?;
                let at: String = row.get(8)?;
                Ok(UsageRecord {
                    id,
                    session_id: SessionId::from_uuid(uuid::Uuid::parse_str(&session_id).map_err(
                        |e| StorageError::InvalidStoredData(format!("session id: {e}")),
                    )?),
                    agent_id: AgentId::from_uuid(
                        uuid::Uuid::parse_str(&agent_id).map_err(|e| {
                            StorageError::InvalidStoredData(format!("agent id: {e}"))
                        })?,
                    ),
                    transport: TransportId::new(transport),
                    model: ModelId::new(model),
                    input_tokens: input_tokens as u64,
                    output_tokens: output_tokens as u64,
                    cached_tokens: cached_tokens as u64,
                    at: parse_time(&at)?,
                })
            })())
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().collect()
}

fn agent_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Result<crate::records::AgentRecord, StorageError>> {
    Ok((|| {
        let id: String = row.get(0)?;
        let session_id: String = row.get(1)?;
        let parent_id: Option<String> = row.get(2)?;
        let profile_json: String = row.get(3)?;
        let state: String = row.get(4)?;
        let created_at: String = row.get(5)?;
        let finished_at: Option<String> = row.get(6)?;

        Ok(crate::records::AgentRecord {
            id: AgentId::from_uuid(
                uuid::Uuid::parse_str(&id)
                    .map_err(|e| StorageError::InvalidStoredData(format!("agent id: {e}")))?,
            ),
            session_id: SessionId::from_uuid(
                uuid::Uuid::parse_str(&session_id)
                    .map_err(|e| StorageError::InvalidStoredData(format!("session id: {e}")))?,
            ),
            parent_id: parent_id
                .map(|p| {
                    uuid::Uuid::parse_str(&p)
                        .map(AgentId::from_uuid)
                        .map_err(|e| {
                            StorageError::InvalidStoredData(format!("parent agent id: {e}"))
                        })
                })
                .transpose()?,
            profile: serde_json::from_str(&profile_json)
                .map_err(|e| StorageError::InvalidStoredData(format!("profile_json: {e}")))?,
            state: AgentState::from_db_str(&state)
                .ok_or_else(|| StorageError::InvalidStoredData(format!("agent state: {state}")))?,
            created_at: parse_time(&created_at)?,
            finished_at: finished_at.map(|s| parse_time(&s)).transpose()?,
        })
    })())
}

pub(crate) fn get_agent(
    conn: &Connection,
    agent_id: &AgentId,
) -> Result<Option<crate::records::AgentRecord>, StorageError> {
    conn.query_row(
        "SELECT id, session_id, parent_id, profile_json, state, created_at, finished_at
         FROM agents WHERE id = ?1",
        params![agent_id.as_uuid().to_string()],
        agent_from_row,
    )
    .optional()?
    .transpose()
}

pub(crate) fn list_agents(
    conn: &Connection,
    session_id: &SessionId,
) -> Result<Vec<crate::records::AgentRecord>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, parent_id, profile_json, state, created_at, finished_at
         FROM agents WHERE session_id = ?1 ORDER BY created_at",
    )?;
    let rows = stmt
        .query_map(params![session_id.as_uuid().to_string()], agent_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().collect()
}

pub(crate) fn list_events(
    conn: &Connection,
    session_id: &SessionId,
) -> Result<Vec<EventRecord>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, agent_id, seq, kind, payload_json, created_at
         FROM events WHERE session_id = ?1 ORDER BY seq",
    )?;
    let rows = stmt
        .query_map(params![session_id.as_uuid().to_string()], |row| {
            Ok((|| {
                let id: i64 = row.get(0)?;
                let session_id: String = row.get(1)?;
                let agent_id: String = row.get(2)?;
                let seq: i64 = row.get(3)?;
                let kind: String = row.get(4)?;
                let payload_json: String = row.get(5)?;
                let created_at: String = row.get(6)?;
                Ok(EventRecord {
                    id,
                    session_id: SessionId::from_uuid(uuid::Uuid::parse_str(&session_id).map_err(
                        |e| StorageError::InvalidStoredData(format!("session id: {e}")),
                    )?),
                    agent_id: AgentId::from_uuid(
                        uuid::Uuid::parse_str(&agent_id).map_err(|e| {
                            StorageError::InvalidStoredData(format!("agent id: {e}"))
                        })?,
                    ),
                    seq,
                    kind: StoredEventKind::from_db_str(&kind).ok_or_else(|| {
                        StorageError::InvalidStoredData(format!("event kind: {kind}"))
                    })?,
                    payload: serde_json::from_str(&payload_json).map_err(|e| {
                        StorageError::InvalidStoredData(format!("payload_json: {e}"))
                    })?,
                    created_at: parse_time(&created_at)?,
                })
            })())
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().collect()
}

#[allow(dead_code)] // kept for symmetry / Wave B use; not every accessor is exercised by Wave A tests
pub(crate) fn workspace_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Result<WorkspaceRecord, StorageError>> {
    Ok((|| {
        let id: String = row.get(0)?;
        let root: String = row.get(1)?;
        let repo_id: Option<String> = row.get(2)?;
        let created_at: String = row.get(3)?;
        Ok(WorkspaceRecord {
            id: WorkspaceId::from_uuid(
                uuid::Uuid::parse_str(&id)
                    .map_err(|e| StorageError::InvalidStoredData(format!("workspace id: {e}")))?,
            ),
            root: std::path::PathBuf::from(root),
            repo_id,
            created_at: parse_time(&created_at)?,
        })
    })())
}
