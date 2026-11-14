//! Mail pipeline observability (Phase 5).
//!
//! Postfix and Amavis write what they did to log files. This module turns those
//! lines into rows in PostgreSQL:
//!
//! * [`parser`] recognises the interesting lines and extracts fields. It never
//!   keeps the raw line or any message content.
//! * [`follower`] reads a log file incrementally, surviving restarts and rotation.
//! * [`store`] writes events and the follower's position in one transaction, and
//!   indexes quarantined messages so the web UI can list them.
//!
//! Quarantine handling (list, read, release, delete) is in [`crate::quarantine`].

pub mod follower;
pub mod parser;
pub mod store;

use chrono::{DateTime, Utc};
use serde::Serialize;

/// Which daemon wrote a log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EventSource {
    /// The Amavis content filter.
    Amavis,
    /// Postfix itself.
    Postfix,
}

impl EventSource {
    /// The value stored in the database.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Amavis => "amavis",
            Self::Postfix => "postfix",
        }
    }
}

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EventKind {
    /// Amavis let the message through (possibly tagged as spam).
    Passed,
    /// Amavis held the message back.
    Blocked,
    /// Postfix reported a delivery attempt.
    Delivery,
    /// Postfix refused a message during the SMTP conversation.
    Reject,
}

impl EventKind {
    /// The value stored in the database.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Blocked => "blocked",
            Self::Delivery => "delivery",
            Self::Reject => "reject",
        }
    }
}

/// One thing that happened to one message for one recipient.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PipelineEvent {
    /// When the daemon logged it.
    pub occurred_at: DateTime<Utc>,
    /// Which daemon.
    pub source: EventSource,
    /// What happened.
    pub kind: EventKind,
    /// Postfix queue id, which links Postfix and Amavis lines for one message.
    pub queue_id: Option<String>,
    /// Amavis's own message id (also the quarantine file's name).
    pub mail_id: Option<String>,
    /// Amavis verdict such as `CLEAN`, `SPAMMY`, `SPAM`, `INFECTED`.
    pub verdict: Option<String>,
    /// Name of the virus found, if any.
    pub virus_name: Option<String>,
    /// SpamAssassin score.
    pub spam_score: Option<f64>,
    /// Envelope sender.
    pub sender: String,
    /// Envelope recipient.
    pub recipient: String,
    /// Delivery status (`sent`, `deferred`, `bounced`) for Postfix delivery lines.
    pub status: Option<String>,
    /// Short free text such as a rejection reason. At most 500 characters.
    pub detail: Option<String>,
    /// Message size in bytes.
    pub size_bytes: Option<i64>,
    /// Name of the quarantine file, when the message was quarantined.
    pub quarantine_ref: Option<String>,
}

/// Longest `detail` text stored.
pub const MAX_DETAIL_CHARS: usize = 500;

/// Truncates `text` to at most `max` characters.
#[must_use]
pub fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}
