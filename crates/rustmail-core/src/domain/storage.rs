//! Mailbox storage accounting and the message index.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Current storage consumption of a mailbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct QuotaUsage {
    /// The mailbox measured.
    pub mailbox_id: Uuid,
    /// Bytes stored.
    pub bytes_used: i64,
    /// Messages stored.
    pub messages_count: i64,
    /// When the figures were last updated.
    pub updated_at: DateTime<Utc>,
}

/// Index row for one message. The body stays in Maildir.
///
/// The generated `search_vector` column is intentionally not mapped; it is
/// queried in SQL only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct MessageMeta {
    /// Monotonic identifier.
    pub id: i64,
    /// Owning mailbox.
    pub mailbox_id: Uuid,
    /// IMAP folder name.
    pub folder: String,
    /// IMAP UIDVALIDITY of the folder when indexed.
    pub uid_validity: i64,
    /// IMAP UID within the folder.
    pub imap_uid: i64,
    /// `Message-ID` header.
    pub message_id: Option<String>,
    /// `In-Reply-To` header.
    pub in_reply_to: Option<String>,
    /// Subject line.
    pub subject: String,
    /// `From` address.
    pub from_addr: String,
    /// `To` addresses, space or comma separated.
    pub to_addrs: String,
    /// `Cc` addresses, space or comma separated.
    pub cc_addrs: String,
    /// Short plain-text excerpt of the body.
    pub snippet: String,
    /// Message size in bytes.
    pub size_bytes: i64,
    /// IMAP flags, e.g. `\Seen`.
    pub flags: Vec<String>,
    /// Whether the message has attachments.
    pub has_attachments: bool,
    /// Parsed `Date` header.
    pub date_header: Option<DateTime<Utc>>,
    /// When the message was delivered/indexed.
    pub received_at: DateTime<Utc>,
}
