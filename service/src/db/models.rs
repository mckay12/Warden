//! Plain data structs mirroring the ERD tables. No behavior here — see the
//! per-entity modules for the CRUD functions that produce/consume these.

#[derive(Debug, Clone, PartialEq)]
pub struct Lock {
    pub id: i64,
    pub status: String, // "active" | "expired" | "forever"
    pub is_forever: bool,
    pub initial_duration_seconds: Option<i64>,
    pub started_at: String,
    pub expires_at: Option<String>,
    pub ended_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LockExtension {
    pub id: i64,
    pub lock_id: i64,
    pub added_seconds: i64,
    pub extended_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmergencyUnlockRequest {
    pub id: i64,
    pub lock_id: i64,
    pub requested_at: String,
    pub cooldown_ends_at: String,
    pub status: String, // "pending" | "cancelled" | "applied"
    pub cancelled_at: Option<String>,
    pub applied_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BlockedAttempt {
    pub id: i64,
    pub occurred_at: String,
    pub match_type: String, // "blocklist" | "keyword"
    pub matched_value: String,
    pub domain: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BlocklistSource {
    pub id: i64,
    pub source_url: String,
    pub last_refreshed_at: Option<String>,
    pub version_hash: Option<String>,
    pub is_active: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BlockedDomain {
    pub id: i64,
    pub blocklist_source_id: i64,
    pub domain: String,
    pub added_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeywordRule {
    pub id: i64,
    pub pattern: String,
    pub pattern_type: String, // "keyword" | "regex"
    pub is_custom: bool,
    pub is_active: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServiceConfig {
    pub id: i64, // always 1
    pub blocklist_refresh_interval_hours: i64,
    pub registry_policy_merge_mode: String, // "overwrite" | "merge"
    pub updated_at: String,
}
