-- Migration 0001: accounts index (docs/PLAN.md §11.2).
--
-- Metadata only — NO secret columns. `keyring_ref` is a lookup key into
-- `xlightcli_auth::store::SecretStore`, never a credential value (PATTERNS.md §10).
CREATE TABLE IF NOT EXISTS accounts (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    provider      TEXT NOT NULL,
    transport     TEXT NOT NULL,
    account_id    TEXT NOT NULL,
    label         TEXT,
    auth_kind     TEXT NOT NULL,
    expiry        TEXT,
    keyring_ref   TEXT NOT NULL,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    is_default    INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    UNIQUE (provider, transport, account_id)
);

CREATE INDEX IF NOT EXISTS idx_accounts_provider_transport
    ON accounts (provider, transport);
