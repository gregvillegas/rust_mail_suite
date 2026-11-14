//! Sessions and the audit trail.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::enums::AuditOutcome;

/// A login session backed by a refresh token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct Session {
    /// Primary key.
    pub id: Uuid,
    /// Owning user.
    pub user_id: Uuid,
    /// Hash of the refresh token; the raw token is never stored.
    #[serde(skip_serializing)]
    pub refresh_token_hash: String,
    /// Client user agent at login.
    pub user_agent: Option<String>,
    /// Client IP address at login.
    pub ip_address: Option<String>,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last time the refresh token was used.
    pub last_used_at: DateTime<Utc>,
    /// The session is invalid after this time.
    pub expires_at: DateTime<Utc>,
    /// Set when the session was revoked (logout, rotation or security response).
    pub revoked_at: Option<DateTime<Utc>>,
    /// Set when this session's refresh token was rotated: the id of its
    /// successor. Distinguishes rotation from an ordinary logout.
    pub replaced_by: Option<Uuid>,
}

/// Input for creating a [`Session`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSession {
    /// Owning user.
    pub user_id: Uuid,
    /// Hash of the refresh token.
    pub refresh_token_hash: String,
    /// Client user agent.
    pub user_agent: Option<String>,
    /// Client IP address.
    pub ip_address: Option<String>,
    /// The session is invalid after this time.
    pub expires_at: DateTime<Utc>,
}

/// One immutable audit-log row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct AuditEntry {
    /// Monotonic identifier.
    pub id: i64,
    /// When the action happened.
    pub occurred_at: DateTime<Utc>,
    /// Acting user, if known and still existing.
    pub actor_user_id: Option<Uuid>,
    /// Snapshot of the actor's address.
    pub actor_email: Option<String>,
    /// Dotted action name, e.g. `auth.login`.
    pub action: String,
    /// Kind of object acted upon, e.g. `user`.
    pub target_type: Option<String>,
    /// Identifier of the object acted upon.
    pub target_id: Option<String>,
    /// Whether the action succeeded.
    pub outcome: AuditOutcome,
    /// Client IP address.
    pub ip_address: Option<String>,
    /// Client user agent.
    pub user_agent: Option<String>,
    /// Additional structured context. Must not contain secrets.
    pub details: serde_json::Value,
}

/// Input for appending an [`AuditEntry`].
#[derive(Debug, Clone, PartialEq)]
pub struct NewAuditEntry {
    /// Acting user, if known.
    pub actor_user_id: Option<Uuid>,
    /// Snapshot of the actor's address.
    pub actor_email: Option<String>,
    /// Dotted action name, e.g. `auth.login`.
    pub action: String,
    /// Kind of object acted upon.
    pub target_type: Option<String>,
    /// Identifier of the object acted upon.
    pub target_id: Option<String>,
    /// Whether the action succeeded.
    pub outcome: AuditOutcome,
    /// Client IP address.
    pub ip_address: Option<String>,
    /// Client user agent.
    pub user_agent: Option<String>,
    /// Additional structured context. Must not contain secrets.
    pub details: serde_json::Value,
}

impl NewAuditEntry {
    /// Starts an entry with only the required fields set. Use struct update
    /// syntax to fill in the rest.
    #[must_use]
    pub fn new(action: impl Into<String>, outcome: AuditOutcome) -> Self {
        Self {
            actor_user_id: None,
            actor_email: None,
            action: action.into(),
            target_type: None,
            target_id: None,
            outcome,
            ip_address: None,
            user_agent: None,
            details: serde_json::json!({}),
        }
    }
}

/// Filters and paging for listing audit entries (newest first).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditQuery {
    /// Only entries by this actor.
    pub actor_user_id: Option<Uuid>,
    /// Only entries with exactly this action.
    pub action: Option<String>,
    /// Page size; clamped to 1..=500.
    pub limit: i64,
    /// Rows to skip.
    pub offset: i64,
}

impl Default for AuditQuery {
    fn default() -> Self {
        Self {
            actor_user_id: None,
            action: None,
            limit: 50,
            offset: 0,
        }
    }
}
