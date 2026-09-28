// SPDX-License-Identifier: GPL-3.0-only

//! `Session` — the runtime-side view of a `storage::SessionRecord` plus live state
//! (docs/PLAN.md §9, §11.2).

use xlightcli_protocol::{ModelId, ProviderId, SessionId, TransportId, WorkspaceId};
use xlightcli_tools::ExecutionMode;

/// One conversation. Cheap to clone; the authoritative copy lives in `storage`, this is a
/// in-memory projection the agent loop and `RuntimeHandle` operate on.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub id: SessionId,
    pub workspace_id: WorkspaceId,
    pub provider: ProviderId,
    pub transport: TransportId,
    pub model: ModelId,
    pub title: Option<String>,
    pub mode: ExecutionMode,
}

impl Session {
    /// Builds the in-memory view from a persisted record (`Storage::create_session`/
    /// `Storage::get_session`), starting in the default execution mode.
    pub fn from_record(record: xlightcli_storage::SessionRecord) -> Self {
        Self {
            id: record.id,
            workspace_id: record.workspace_id,
            provider: record.provider,
            transport: record.transport,
            model: record.model,
            title: record.title,
            mode: ExecutionMode::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;
    use xlightcli_storage::SessionStatus;

    use super::*;

    #[test]
    fn from_record_starts_in_default_mode() {
        let record = xlightcli_storage::SessionRecord {
            id: SessionId::new(),
            workspace_id: WorkspaceId::new(),
            provider: ProviderId::new("codex"),
            transport: TransportId::new("chatgpt"),
            model: ModelId::new("gpt-5"),
            title: None,
            created_at: time::OffsetDateTime::now_utc(),
            updated_at: time::OffsetDateTime::now_utc(),
            status: SessionStatus::Active,
        };
        let session = Session::from_record(record);
        assert_eq!(session.mode, ExecutionMode::default());
    }
}
