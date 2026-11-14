//! Calendar and contact stubs. Behaviour is added in Phase 9.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A calendar event (stub model).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct CalendarEvent {
    /// Primary key.
    pub id: Uuid,
    /// Owning user.
    pub owner_user_id: Uuid,
    /// iCalendar UID, stable across clients.
    pub uid: String,
    /// Event title.
    pub title: String,
    /// Event description.
    pub description: String,
    /// Event location.
    pub location: String,
    /// Start time.
    pub starts_at: DateTime<Utc>,
    /// End time (not before `starts_at`).
    pub ends_at: DateTime<Utc>,
    /// Whether the event spans whole days.
    pub all_day: bool,
    /// iCalendar recurrence rule, if recurring.
    pub rrule: Option<String>,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last modification time.
    pub updated_at: DateTime<Utc>,
}

/// An address-book contact (stub model).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Contact {
    /// Primary key.
    pub id: Uuid,
    /// Owning user.
    pub owner_user_id: Uuid,
    /// vCard UID, stable across clients.
    pub uid: String,
    /// Full display name.
    pub full_name: String,
    /// Primary e-mail address.
    pub email: String,
    /// Primary phone number.
    pub phone: String,
    /// Organisation name.
    pub organization: String,
    /// Original vCard text, if imported.
    pub vcard: Option<String>,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last modification time.
    pub updated_at: DateTime<Utc>,
}
