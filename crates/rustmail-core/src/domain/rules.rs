//! Per-user mail rules: Sieve scripts, autoresponders and UI filters.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A user's Sieve script. At most one script per user is active.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct SieveScript {
    /// Primary key.
    pub id: Uuid,
    /// Owning user.
    pub user_id: Uuid,
    /// Script name, unique per user.
    pub name: String,
    /// Sieve source.
    pub content: String,
    /// Whether this is the user's active script.
    pub is_active: bool,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last modification time.
    pub updated_at: DateTime<Utc>,
}

/// A user's out-of-office responder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Autoresponder {
    /// Primary key.
    pub id: Uuid,
    /// Owning user (one responder per user).
    pub user_id: Uuid,
    /// Whether the responder is switched on.
    pub enabled: bool,
    /// Reply subject.
    pub subject: String,
    /// Reply body.
    pub body: String,
    /// Responder is active from this time, if set.
    pub starts_at: Option<DateTime<Utc>>,
    /// Responder is active until this time, if set.
    pub ends_at: Option<DateTime<Utc>>,
    /// Minimum days between replies to the same sender.
    pub reply_interval_days: i32,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last modification time.
    pub updated_at: DateTime<Utc>,
}

/// A rule defined in the UI. `conditions` and `actions` are JSON documents
/// whose schema is owned by the rules engine (Phase 8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct MailFilter {
    /// Primary key.
    pub id: Uuid,
    /// Owning user.
    pub user_id: Uuid,
    /// Rule name.
    pub name: String,
    /// Evaluation order (ascending).
    pub position: i32,
    /// Disabled rules are skipped.
    pub enabled: bool,
    /// Match conditions.
    pub conditions: serde_json::Value,
    /// Actions to run on a match.
    pub actions: serde_json::Value,
    /// Stop evaluating further rules after this one matches.
    pub stop_processing: bool,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last modification time.
    pub updated_at: DateTime<Utc>,
}
