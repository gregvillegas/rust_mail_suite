//! Mail routing: virtual aliases and groups (distribution lists).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One `address -> destination` mapping. An alias with several destinations
/// is stored as several rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Alias {
    /// Primary key.
    pub id: Uuid,
    /// Owning domain.
    pub domain_id: Uuid,
    /// Lower-case alias address; must belong to `domain_id`.
    pub address: String,
    /// Lower-case destination address (internal or external).
    pub destination: String,
    /// Disabled aliases are ignored when routing.
    pub enabled: bool,
    /// Creation time.
    pub created_at: DateTime<Utc>,
}

/// Input for creating an [`Alias`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewAlias {
    /// Owning domain.
    pub domain_id: Uuid,
    /// Alias address; normalised on insert and must belong to `domain_id`.
    pub address: String,
    /// Destination address; normalised on insert.
    pub destination: String,
}

/// A group of users, optionally reachable as a distribution list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Group {
    /// Primary key.
    pub id: Uuid,
    /// Owning domain.
    pub domain_id: Uuid,
    /// Display name, unique within the domain.
    pub name: String,
    /// Distribution-list address, if the group can receive mail.
    pub address: Option<String>,
    /// Free-form description.
    pub description: String,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last modification time.
    pub updated_at: DateTime<Utc>,
}

/// Input for creating a [`Group`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewGroup {
    /// Owning domain.
    pub domain_id: Uuid,
    /// Display name, unique within the domain.
    pub name: String,
    /// Distribution-list address; normalised on insert.
    pub address: Option<String>,
    /// Free-form description.
    pub description: String,
}
