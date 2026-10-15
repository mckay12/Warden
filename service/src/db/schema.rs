/// DDL for every table in the ERD. `CREATE TABLE IF NOT EXISTS` throughout —
/// this is the only migration mechanism Phase 5 needs; a real migration
/// system can come later if the schema needs to evolve after data exists.
pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS lock (
    id                          INTEGER PRIMARY KEY AUTOINCREMENT,
    status                      TEXT NOT NULL CHECK (status IN ('active', 'expired', 'forever')),
    is_forever                  INTEGER NOT NULL CHECK (is_forever IN (0, 1)),
    initial_duration_seconds    INTEGER,
    started_at                  TEXT NOT NULL,
    expires_at                  TEXT,
    ended_at                    TEXT,
    created_at                  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS lock_extension (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    lock_id       INTEGER NOT NULL REFERENCES lock(id),
    added_seconds INTEGER NOT NULL,
    extended_at   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS emergency_unlock_request (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    lock_id           INTEGER NOT NULL REFERENCES lock(id),
    requested_at      TEXT NOT NULL,
    cooldown_ends_at  TEXT NOT NULL,
    status            TEXT NOT NULL CHECK (status IN ('pending', 'cancelled', 'applied')),
    cancelled_at      TEXT,
    applied_at        TEXT
);

CREATE TABLE IF NOT EXISTS blocked_attempt (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    occurred_at  TEXT NOT NULL,
    match_type   TEXT NOT NULL CHECK (match_type IN ('blocklist', 'keyword')),
    matched_value TEXT NOT NULL,
    domain       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_blocked_attempt_occurred_at ON blocked_attempt(occurred_at);

CREATE TABLE IF NOT EXISTS blocklist_source (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    source_url         TEXT NOT NULL,
    last_refreshed_at  TEXT,
    version_hash       TEXT,
    is_active          INTEGER NOT NULL CHECK (is_active IN (0, 1))
);

CREATE TABLE IF NOT EXISTS blocked_domain (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    blocklist_source_id  INTEGER NOT NULL REFERENCES blocklist_source(id),
    domain               TEXT NOT NULL,
    added_at             TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_blocked_domain_domain ON blocked_domain(domain);

CREATE TABLE IF NOT EXISTS keyword_rule (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    pattern       TEXT NOT NULL,
    pattern_type  TEXT NOT NULL CHECK (pattern_type IN ('keyword', 'regex')),
    is_custom     INTEGER NOT NULL CHECK (is_custom IN (0, 1)),
    is_active     INTEGER NOT NULL CHECK (is_active IN (0, 1)),
    created_at    TEXT NOT NULL
);

-- Exceptions to the blocklist (false positives). Asking for one only starts a
-- cooldown (effective_at); it is not in force until then. Removing one is
-- instant, since that only makes blocking stricter.
CREATE TABLE IF NOT EXISTS allow_exception (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    domain        TEXT NOT NULL,
    requested_at  TEXT NOT NULL,
    effective_at  TEXT NOT NULL,
    status        TEXT NOT NULL CHECK (status IN ('active', 'removed')),
    removed_at    TEXT
);
CREATE INDEX IF NOT EXISTS idx_allow_exception_domain ON allow_exception(domain);

CREATE TABLE IF NOT EXISTS service_config (
    id                                 INTEGER PRIMARY KEY CHECK (id = 1),
    blocklist_refresh_interval_hours   INTEGER NOT NULL,
    registry_policy_merge_mode         TEXT NOT NULL CHECK (registry_policy_merge_mode IN ('overwrite', 'merge')),
    updated_at                         TEXT NOT NULL
);
"#;
