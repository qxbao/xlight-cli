// SPDX-License-Identifier: GPL-3.0-only

//! `Storage` — the async-facing persistence handle (PATTERNS.md §10, docs/PLAN.md §11.2).
//!
//! Writes go to the dedicated writer thread (`crate::worker`) over a bounded channel; bulk/paged
//! reads (`load_messages`, `list_artifacts`, `list_tool_calls`, `list_usage`, `list_events`) use a
//! second, independent connection to the same WAL-mode database file, so a long read never blocks
//! the writer (PATTERNS.md §10 "the reader uses its own read-only connection").
//!
//! **`messages` projection scope decision (Phase 1 Wave A):** rather than have the writer thread
//! infer `messages` rows from `events` payloads (which would require it to understand every
//! `AgentEvent`/`StoredEventKind` shape), callers write both explicitly: `append_events` for the
//! append-only log, and [`Storage::append_message`] for the projection, whenever they have a
//! complete `Message` to persist (typically once per turn, from `AgentEvent::Completed`, plus once
//! for the user's own turn). This keeps `events` as the true source of truth while keeping the
//! writer's SQL simple; Wave B can move the projection logic server-side later without changing
//! this file's public signatures.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use tokio::sync::{mpsc, oneshot};
use xlightcli_protocol::{
    AgentId, ContentBlock, ModelId, ProviderId, Role, SessionId, TransportId, WorkspaceId,
};

use crate::error::StorageError;
use crate::records::{
    ArtifactRecord, EventRecord, MessagePage, MessageRecord, NewArtifact, NewEvent, NewToolCall,
    NewUsageRow, SessionRecord, SessionStatus, ToolCallRecord, ToolCallStatus, UsageRecord,
};
use crate::worker::{self, StorageCmd};

/// Bounded writer-command channel capacity (PATTERNS.md §3: always bounded, sized deliberately).
/// 256 keeps a healthy backlog for bursts (e.g. `append_events` from several concurrently
/// streaming agents) without unbounded memory growth if the writer thread stalls.
const CHANNEL_CAPACITY: usize = 256;

/// Async handle to the persistence layer. Cheap to clone (shares the writer channel and the
/// reader connection); safe to share across tasks — this is the `Arc`'d resource `app::wiring`
/// hands to `runtime` (CODEBASE.md §5).
#[derive(Clone)]
pub struct Storage {
    cmd_tx: mpsc::Sender<StorageCmd>,
    reader: Arc<Mutex<Connection>>,
}

impl std::fmt::Debug for Storage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Storage").finish_non_exhaustive()
    }
}

impl Storage {
    /// Opens (creating if needed) the database at `path`, applies pending migrations, spawns the
    /// writer thread, opens a second reader connection, and marks any session left `active` from
    /// a previous crash as `interrupted` (docs/PLAN.md §11.2).
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let path = path.into();
        let write_path = path.clone();
        let write_conn = tokio::task::spawn_blocking(move || crate::db::open_and_migrate(&write_path)).await??;
        let read_path = path.clone();
        let reader_conn = tokio::task::spawn_blocking(move || crate::db::open_reader(&read_path)).await??;

        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        std::thread::Builder::new()
            .name("xlightcli-storage-writer".to_string())
            .spawn(move || worker::run(write_conn, rx))
            .map_err(|e| StorageError::InvalidStoredData(format!("failed to spawn storage writer thread: {e}")))?;

        let storage = Self {
            cmd_tx: tx,
            reader: Arc::new(Mutex::new(reader_conn)),
        };
        storage.mark_interrupted_on_open().await?;
        Ok(storage)
    }

    #[cfg(test)]
    async fn open_in_memory() -> Result<Self, StorageError> {
        // Two independent in-memory connections would be two independent (empty) databases, so
        // the reader side of an in-memory `Storage` intentionally isn't exercised by tests —
        // `open_in_memory` exists only for the writer-side unit tests in this module.
        let write_conn = tokio::task::spawn_blocking(crate::db::open_in_memory_and_migrate).await??;
        let reader_conn = tokio::task::spawn_blocking(crate::db::open_in_memory_and_migrate).await??;
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        std::thread::Builder::new()
            .name("xlightcli-storage-writer-test".to_string())
            .spawn(move || worker::run(write_conn, rx))
            .map_err(|e| StorageError::InvalidStoredData(format!("failed to spawn storage writer thread: {e}")))?;
        Ok(Self {
            cmd_tx: tx,
            reader: Arc::new(Mutex::new(reader_conn)),
        })
    }

    async fn call<T, F>(&self, build: F) -> Result<T, StorageError>
    where
        T: Send + 'static,
        F: FnOnce(oneshot::Sender<Result<T, StorageError>>) -> StorageCmd,
    {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(build(reply_tx))
            .await
            .map_err(|_| StorageError::WriterUnavailable)?;
        reply_rx.await.map_err(|_| StorageError::WriterUnavailable)?
    }

    async fn with_reader<T, F>(&self, f: F) -> Result<T, StorageError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T, StorageError> + Send + 'static,
    {
        let conn = Arc::clone(&self.reader);
        tokio::task::spawn_blocking(move || {
            let guard = conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            f(&guard)
        })
        .await?
    }

    // --- workspaces / sessions ---------------------------------------------------------------

    pub async fn create_workspace(&self, root: PathBuf, repo_id: Option<String>) -> Result<WorkspaceId, StorageError> {
        self.call(|reply| StorageCmd::CreateWorkspace { root, repo_id, reply }).await
    }

    pub async fn create_session(
        &self,
        workspace_id: WorkspaceId,
        provider: ProviderId,
        transport: TransportId,
        model: ModelId,
        title: Option<String>,
    ) -> Result<SessionId, StorageError> {
        self.call(|reply| StorageCmd::CreateSession {
            workspace_id,
            provider,
            transport,
            model,
            title,
            reply,
        })
        .await
    }

    pub async fn list_sessions(&self, workspace_id: Option<WorkspaceId>) -> Result<Vec<SessionRecord>, StorageError> {
        self.call(|reply| StorageCmd::ListSessions { workspace_id, reply }).await
    }

    pub async fn get_session(&self, session_id: SessionId) -> Result<Option<SessionRecord>, StorageError> {
        self.call(|reply| StorageCmd::GetSession { session_id, reply }).await
    }

    pub async fn update_session_status(&self, session_id: SessionId, status: SessionStatus) -> Result<(), StorageError> {
        self.call(|reply| StorageCmd::UpdateSessionStatus { session_id, status, reply }).await
    }

    /// Marks every `active` session `interrupted` (docs/PLAN.md §11.2). Called automatically by
    /// [`Self::open`]; exposed so tests (and `core.status`/diagnostics) can call it explicitly.
    pub async fn mark_interrupted_on_open(&self) -> Result<u64, StorageError> {
        self.call(|reply| StorageCmd::MarkInterruptedOnOpen { reply }).await
    }

    // --- agents --------------------------------------------------------------------------------

    pub async fn register_agent(
        &self,
        session_id: SessionId,
        parent_id: Option<AgentId>,
        profile: serde_json::Value,
    ) -> Result<AgentId, StorageError> {
        self.call(|reply| StorageCmd::RegisterAgent {
            session_id,
            parent_id,
            profile,
            reply,
        })
        .await
    }

    // --- events / messages -----------------------------------------------------------------

    /// Appends `events` to the append-only log in one atomic batch, assigning each a
    /// per-session-monotonic `seq`. Returns the surrogate row id of each inserted event, in order.
    pub async fn append_events(&self, session_id: SessionId, events: Vec<NewEvent>) -> Result<Vec<i64>, StorageError> {
        self.call(|reply| StorageCmd::AppendEvents { session_id, events, reply }).await
    }

    pub async fn list_events(&self, session_id: SessionId) -> Result<Vec<EventRecord>, StorageError> {
        self.with_reader(move |conn| worker::list_events(conn, &session_id)).await
    }

    /// Appends one row to the `messages` projection (see the module doc for the Wave A scope
    /// decision on how `messages` relates to `events`).
    pub async fn append_message(
        &self,
        session_id: SessionId,
        agent_id: AgentId,
        turn: i64,
        role: Role,
        content: Vec<ContentBlock>,
    ) -> Result<i64, StorageError> {
        self.call(|reply| StorageCmd::AppendMessage {
            session_id,
            agent_id,
            turn,
            role,
            content,
            reply,
        })
        .await
    }

    /// Loads up to `page.limit` messages older than `page.before_id` (or the most recent
    /// `page.limit` when `before_id` is `None`), oldest-first — the shape `ContextManager` needs
    /// to lazily page in history (docs/PLAN.md §9.2).
    pub async fn load_messages(&self, session_id: SessionId, page: MessagePage) -> Result<Vec<MessageRecord>, StorageError> {
        self.with_reader(move |conn| worker::load_messages(conn, &session_id, page)).await
    }

    // --- tool calls ----------------------------------------------------------------------------

    pub async fn record_tool_call(&self, call: NewToolCall) -> Result<i64, StorageError> {
        self.call(|reply| StorageCmd::RecordToolCall { call, reply }).await
    }

    pub async fn finish_tool_call(
        &self,
        id: i64,
        status: ToolCallStatus,
        artifact_id: Option<i64>,
    ) -> Result<(), StorageError> {
        self.call(|reply| StorageCmd::FinishToolCall { id, status, artifact_id, reply }).await
    }

    pub async fn list_tool_calls(&self, agent_id: AgentId) -> Result<Vec<ToolCallRecord>, StorageError> {
        self.with_reader(move |conn| worker::list_tool_calls(conn, &agent_id)).await
    }

    // --- artifacts -----------------------------------------------------------------------------

    pub async fn register_artifact(&self, artifact: NewArtifact) -> Result<i64, StorageError> {
        self.call(|reply| StorageCmd::RegisterArtifact { artifact, reply }).await
    }

    pub async fn list_artifacts(&self, session_id: SessionId) -> Result<Vec<ArtifactRecord>, StorageError> {
        self.with_reader(move |conn| worker::list_artifacts(conn, &session_id)).await
    }

    // --- usage ---------------------------------------------------------------------------------

    pub async fn record_usage(&self, row: NewUsageRow) -> Result<i64, StorageError> {
        self.call(|reply| StorageCmd::RecordUsage { row, reply }).await
    }

    pub async fn list_usage(&self, session_id: SessionId) -> Result<Vec<UsageRecord>, StorageError> {
        self.with_reader(move |conn| worker::list_usage(conn, &session_id)).await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;
    use serde_json::json;
    use xlightcli_protocol::ToolCallId;

    use super::*;
    use crate::records::StoredEventKind;

    async fn sample_session(storage: &Storage) -> (WorkspaceId, SessionId, AgentId) {
        let workspace = storage
            .create_workspace(PathBuf::from("/repo"), Some("repo-1".to_string()))
            .await
            .unwrap();
        let session = storage
            .create_session(
                workspace.clone(),
                ProviderId::new("codex"),
                TransportId::new("chatgpt"),
                ModelId::new("gpt-5"),
                Some("test session".to_string()),
            )
            .await
            .unwrap();
        let agent = storage
            .register_agent(session.clone(), None, json!({"provider": "codex"}))
            .await
            .unwrap();
        (workspace, session, agent)
    }

    #[tokio::test]
    async fn create_list_get_session_roundtrip() {
        let storage = Storage::open_in_memory().await.unwrap();
        let (workspace, session, _agent) = sample_session(&storage).await;

        let fetched = storage.get_session(session.clone()).await.unwrap().unwrap();
        assert_eq!(fetched.id, session);
        assert_eq!(fetched.workspace_id, workspace);
        assert_eq!(fetched.status, SessionStatus::Active);

        let listed = storage.list_sessions(Some(workspace)).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, session);
    }

    #[tokio::test]
    async fn get_missing_session_is_none() {
        let storage = Storage::open_in_memory().await.unwrap();
        assert!(storage.get_session(SessionId::new()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn append_events_assigns_monotonic_seq() {
        let storage = Storage::open_in_memory().await.unwrap();
        let (_workspace, session, agent) = sample_session(&storage).await;

        let events = vec![
            NewEvent {
                agent_id: agent.clone(),
                kind: StoredEventKind::TurnStarted,
                payload: json!({"model": "gpt-5"}),
            },
            NewEvent {
                agent_id: agent.clone(),
                kind: StoredEventKind::TextDelta,
                payload: json!({"index": 0, "text": "hi"}),
            },
        ];
        let ids = storage.append_events(session.clone(), events).await.unwrap();
        assert_eq!(ids.len(), 2);

        let stored = storage.list_events(session).await.unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].seq, 1);
        assert_eq!(stored[1].seq, 2);
        assert_eq!(stored[0].kind, StoredEventKind::TurnStarted);
    }

    #[tokio::test]
    async fn append_message_and_load_messages_orders_oldest_first() {
        let storage = Storage::open_in_memory().await.unwrap();
        let (_workspace, session, agent) = sample_session(&storage).await;

        storage
            .append_message(session.clone(), agent.clone(), 1, Role::User, vec![ContentBlock::Text {
                text: "hello".to_string(),
            }])
            .await
            .unwrap();
        storage
            .append_message(session.clone(), agent.clone(), 1, Role::Assistant, vec![ContentBlock::Text {
                text: "hi there".to_string(),
            }])
            .await
            .unwrap();

        let messages = storage
            .load_messages(session, MessagePage::default())
            .await
            .unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[1].role, Role::Assistant);
    }

    #[tokio::test]
    async fn load_messages_pages_with_before_id() {
        let storage = Storage::open_in_memory().await.unwrap();
        let (_workspace, session, agent) = sample_session(&storage).await;
        for i in 0..5 {
            storage
                .append_message(session.clone(), agent.clone(), i, Role::User, vec![ContentBlock::Text {
                    text: format!("message {i}"),
                }])
                .await
                .unwrap();
        }

        let first_page = storage
            .load_messages(session.clone(), MessagePage { before_id: None, limit: 2 })
            .await
            .unwrap();
        assert_eq!(first_page.len(), 2);
        // Most recent 2, oldest-first within the page: turns 3 then 4.
        assert_eq!(first_page[0].turn, 3);
        assert_eq!(first_page[1].turn, 4);

        let second_page = storage
            .load_messages(
                session,
                MessagePage {
                    before_id: Some(first_page[0].id),
                    limit: 2,
                },
            )
            .await
            .unwrap();
        assert_eq!(second_page.len(), 2);
        assert_eq!(second_page[0].turn, 1);
        assert_eq!(second_page[1].turn, 2);
    }

    #[tokio::test]
    async fn tool_call_lifecycle() {
        let storage = Storage::open_in_memory().await.unwrap();
        let (_workspace, session, agent) = sample_session(&storage).await;

        let id = storage
            .record_tool_call(NewToolCall {
                agent_id: agent.clone(),
                call_id: ToolCallId::new("call-1"),
                name: "read_file".to_string(),
                input: json!({"path": "README.md"}),
            })
            .await
            .unwrap();

        let artifact_id = storage
            .register_artifact(NewArtifact {
                session_id: session.clone(),
                path: PathBuf::from("/artifacts/call-1.log"),
                bytes: 42,
                sha256: "deadbeef".to_string(),
                kind: "tool_output".to_string(),
            })
            .await
            .unwrap();

        storage
            .finish_tool_call(id, ToolCallStatus::Succeeded, Some(artifact_id))
            .await
            .unwrap();

        let calls = storage.list_tool_calls(agent).await.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].status, ToolCallStatus::Succeeded);
        assert_eq!(calls[0].artifact_id, Some(artifact_id));

        let artifacts = storage.list_artifacts(session).await.unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].sha256, "deadbeef");
    }

    #[tokio::test]
    async fn usage_rows_are_recorded_and_listed() {
        let storage = Storage::open_in_memory().await.unwrap();
        let (_workspace, session, agent) = sample_session(&storage).await;

        storage
            .record_usage(NewUsageRow {
                session_id: session.clone(),
                agent_id: agent,
                transport: TransportId::new("chatgpt"),
                model: ModelId::new("gpt-5"),
                usage: xlightcli_protocol::Usage {
                    input_tokens: 100,
                    output_tokens: 20,
                    cached_input_tokens: 5,
                    reasoning_tokens: 0,
                },
            })
            .await
            .unwrap();

        let rows = storage.list_usage(session).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].input_tokens, 100);
        assert_eq!(rows[0].output_tokens, 20);
    }

    #[tokio::test]
    async fn mark_interrupted_on_open_flips_active_sessions() {
        let storage = Storage::open_in_memory().await.unwrap();
        let (_workspace, session, _agent) = sample_session(&storage).await;

        let flipped = storage.mark_interrupted_on_open().await.unwrap();
        assert_eq!(flipped, 1);

        let fetched = storage.get_session(session).await.unwrap().unwrap();
        assert_eq!(fetched.status, SessionStatus::Interrupted);
    }
}
