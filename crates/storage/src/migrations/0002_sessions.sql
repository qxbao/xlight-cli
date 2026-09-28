-- Migration 0002: event-sourced session schema (docs/PLAN.md §11.2).
--
-- `events` is append-only and the source of truth; `messages` is a projection (populated
-- explicitly by callers via `Storage::append_message`, docs/CONTRACTS.md §4.1 for the Wave 1
-- scope decision). No column here ever holds a secret (INV-4); `provider`/`transport`/`model`
-- are ids/labels, never credentials.
CREATE TABLE IF NOT EXISTS workspaces (
    id         TEXT PRIMARY KEY,
    root       TEXT NOT NULL,
    repo_id    TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
    id           TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces (id),
    provider     TEXT NOT NULL,
    transport    TEXT NOT NULL,
    model        TEXT NOT NULL,
    title        TEXT,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'active'
);

CREATE INDEX IF NOT EXISTS idx_sessions_workspace ON sessions (workspace_id);

CREATE TABLE IF NOT EXISTS agents (
    id           TEXT PRIMARY KEY,
    session_id   TEXT NOT NULL REFERENCES sessions (id),
    parent_id    TEXT REFERENCES agents (id),
    profile_json TEXT NOT NULL DEFAULT '{}',
    state        TEXT NOT NULL DEFAULT 'queued',
    created_at   TEXT NOT NULL,
    finished_at  TEXT
);

CREATE INDEX IF NOT EXISTS idx_agents_session ON agents (session_id);

-- Append-only, source of truth. `seq` is per-session, monotonically increasing (assigned by the
-- writer thread, never by the caller) so a crash can reconstruct `messages` in order.
CREATE TABLE IF NOT EXISTS events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id   TEXT NOT NULL REFERENCES sessions (id),
    agent_id     TEXT NOT NULL REFERENCES agents (id),
    seq          INTEGER NOT NULL,
    kind         TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    UNIQUE (session_id, seq)
);

CREATE INDEX IF NOT EXISTS idx_events_session_seq ON events (session_id, seq);

-- Projection from events (docs/PLAN.md §11.2); `content_json` is a canonical
-- `xlightcli_protocol::ContentBlock` array (never provider wire JSON, PATTERNS.md §10).
CREATE TABLE IF NOT EXISTS messages (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id   TEXT NOT NULL REFERENCES sessions (id),
    agent_id     TEXT NOT NULL REFERENCES agents (id),
    turn         INTEGER NOT NULL,
    role         TEXT NOT NULL,
    content_json TEXT NOT NULL,
    created_at   TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_messages_session ON messages (session_id, id);

CREATE TABLE IF NOT EXISTS tool_calls (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    agent_id    TEXT NOT NULL REFERENCES agents (id),
    call_id     TEXT NOT NULL,
    name        TEXT NOT NULL,
    input_json  TEXT NOT NULL,
    status      TEXT NOT NULL DEFAULT 'running',
    artifact_id INTEGER REFERENCES artifacts (id),
    started_at  TEXT NOT NULL,
    finished_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_tool_calls_agent ON tool_calls (agent_id);

CREATE TABLE IF NOT EXISTS artifacts (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES sessions (id),
    path       TEXT NOT NULL,
    bytes      INTEGER NOT NULL,
    sha256     TEXT NOT NULL,
    kind       TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_artifacts_session ON artifacts (session_id);

CREATE TABLE IF NOT EXISTS summaries (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id       TEXT NOT NULL REFERENCES sessions (id),
    agent_id         TEXT NOT NULL REFERENCES agents (id),
    covers_until_seq INTEGER NOT NULL,
    text             TEXT NOT NULL,
    created_at       TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_summaries_session ON summaries (session_id);

CREATE TABLE IF NOT EXISTS provider_usage (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id     TEXT NOT NULL REFERENCES sessions (id),
    agent_id       TEXT NOT NULL REFERENCES agents (id),
    transport      TEXT NOT NULL,
    model          TEXT NOT NULL,
    input_tokens   INTEGER NOT NULL,
    output_tokens  INTEGER NOT NULL,
    cached_tokens  INTEGER NOT NULL,
    at             TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_provider_usage_session ON provider_usage (session_id);
