//! Quarantine: listing, inspecting, releasing and deleting held messages.
//!
//! Amavis writes each held message to a file in the quarantine directory. The
//! log follower ([`crate::pipeline`]) records an index row per file and recipient
//! in `quarantine_items`. This service works on that index and on the files.
//!
//! Safety rules enforced here:
//! * a file name from the database is only ever used as a bare name inside the
//!   quarantine directory (no separators, no `..`), and symlinks are refused;
//! * released mail goes only to the one recipient the row is for;
//! * a file is deleted from disk only when no other recipient still needs it.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use tokio::io::AsyncReadExt;
use uuid::Uuid;

use crate::domain::{AuditOutcome, NewAuditEntry};
use crate::error::{Error, Result};
use crate::repository::{AuditLogRepository, PgAuditLogRepository};
use crate::smtp;

/// Largest message file that will be read into memory.
pub const MAX_MESSAGE_BYTES: u64 = 64 * 1024 * 1024;
/// Page size limits for listings.
pub const MAX_PAGE_SIZE: i64 = 200;

/// Headers Amavis adds to a quarantined copy that must not travel on when the
/// message is released: they describe the quarantine, not the original mail.
const AMAVIS_ENVELOPE_HEADERS: [&str; 6] = [
    "return-path",
    "delivered-to",
    "x-envelope-from",
    "x-envelope-to",
    "x-envelope-to-blocked",
    "x-quarantine-id",
];

/// Whether `name` is a plain file name that is safe to join onto the quarantine
/// directory: letters, digits, `.`, `_`, `-`, starting with a letter or digit,
/// with no `..` and at most 128 characters.
#[must_use]
pub fn is_safe_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.contains("..")
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Why a message was held back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum QuarantineKind {
    /// A virus was found.
    Virus,
    /// Scored above the quarantine threshold.
    Spam,
    /// Contained a banned file type.
    Banned,
    /// Had a malformed header.
    BadHeader,
    /// Anything else.
    Other,
}

impl QuarantineKind {
    /// The value stored in the database and used in URLs.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Virus => "virus",
            Self::Spam => "spam",
            Self::Banned => "banned",
            Self::BadHeader => "bad-header",
            Self::Other => "other",
        }
    }

    /// Parses the stored value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "virus" => Self::Virus,
            "spam" => Self::Spam,
            "banned" => Self::Banned,
            "bad-header" => Self::BadHeader,
            "other" => Self::Other,
            _ => return None,
        })
    }
}

/// Where a quarantined message stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum QuarantineStatus {
    /// Waiting for a decision.
    Held,
    /// Delivered to the recipient on request.
    Released,
    /// Removed on request.
    Deleted,
}

impl QuarantineStatus {
    /// The value stored in the database.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::Released => "released",
            Self::Deleted => "deleted",
        }
    }

    /// Parses the stored value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "held" => Self::Held,
            "released" => Self::Released,
            "deleted" => Self::Deleted,
            _ => return None,
        })
    }
}

/// One held message for one recipient.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuarantineItem {
    /// Identifier used in URLs.
    pub id: Uuid,
    /// Amavis message id.
    pub mail_id: String,
    /// Who the message was for.
    pub recipient: String,
    /// Envelope sender.
    pub sender: String,
    /// Why it was held.
    pub kind: QuarantineKind,
    /// Virus name, score or banned type.
    pub reason: String,
    /// SpamAssassin score, for spam.
    pub spam_score: Option<f64>,
    /// Size in bytes.
    pub size_bytes: Option<i64>,
    /// When it was held.
    pub received_at: DateTime<Utc>,
    /// Current state.
    pub status: QuarantineStatus,
    /// When it was released or deleted.
    pub resolved_at: Option<DateTime<Utc>>,
}

impl QuarantineItem {
    /// The recipient's domain, lower-case.
    #[must_use]
    pub fn domain(&self) -> String {
        self.recipient
            .rsplit_once('@')
            .map_or_else(String::new, |(_, d)| d.to_lowercase())
    }
}

/// Which items to list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantineFilter {
    /// State to show; `None` for all.
    pub status: Option<QuarantineStatus>,
    /// Category to show; `None` for all.
    pub kind: Option<QuarantineKind>,
    /// Text to look for in sender or recipient.
    pub search: Option<String>,
    /// Only messages for recipients in this domain (set for domain administrators).
    pub domain: Option<String>,
    /// Rows per page, at most [`MAX_PAGE_SIZE`].
    pub limit: i64,
    /// Rows to skip.
    pub offset: i64,
}

impl Default for QuarantineFilter {
    fn default() -> Self {
        Self {
            status: Some(QuarantineStatus::Held),
            kind: None,
            search: None,
            domain: None,
            limit: 25,
            offset: 0,
        }
    }
}

/// A page of items.
#[derive(Debug, Clone, PartialEq)]
pub struct QuarantinePage {
    /// The rows.
    pub items: Vec<QuarantineItem>,
    /// How many rows match the filter in total.
    pub total: i64,
}

/// Counts of held messages per category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct QuarantineCounts {
    /// Held viruses.
    pub virus: i64,
    /// Held spam.
    pub spam: i64,
    /// Everything else that is held.
    pub other: i64,
}

/// A read-only view of a quarantined message: headers and a short text excerpt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessagePreview {
    /// Header fields in order, unfolded.
    pub headers: Vec<(String, String)>,
    /// The start of the body if the message is plain text, else `None`.
    pub excerpt: Option<String>,
    /// Whether the excerpt was cut short.
    pub truncated: bool,
}

/// Who is acting, for the audit trail.
#[derive(Debug, Clone)]
pub struct Actor {
    /// The signed-in user.
    pub user_id: Uuid,
    /// Their address, recorded alongside.
    pub email: String,
    /// Client address, if known.
    pub ip_address: Option<String>,
}

/// Where the quarantine lives and how to hand released mail back to Postfix.
#[derive(Debug, Clone)]
pub struct QuarantineConfig {
    /// Directory Amavis writes to.
    pub dir: PathBuf,
    /// Postfix re-entry SMTP listener, `host:port`.
    pub reinject_addr: String,
    /// Name announced in `EHLO`.
    pub helo: String,
}

impl QuarantineConfig {
    /// Reads `QUARANTINE_DIR` (required), `REINJECT_ADDR` (default
    /// `127.0.0.1:10025`) and `REINJECT_HELO` (default `rustmail.local`).
    ///
    /// # Errors
    /// Fails if `QUARANTINE_DIR` is not set.
    pub fn from_env() -> Result<Self> {
        let dir = std::env::var("QUARANTINE_DIR")
            .map_err(|_| Error::Config("QUARANTINE_DIR is not set".into()))?;
        Ok(Self {
            dir: PathBuf::from(dir),
            reinject_addr: std::env::var("REINJECT_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:10025".to_owned()),
            helo: std::env::var("REINJECT_HELO").unwrap_or_else(|_| "rustmail.local".to_owned()),
        })
    }
}

/// Quarantine operations.
#[derive(Debug, Clone)]
pub struct QuarantineService {
    pool: PgPool,
    config: QuarantineConfig,
}

/// A row as read from the database.
struct Row {
    id: Uuid,
    mail_id: String,
    recipient: String,
    sender: String,
    kind: String,
    reason: String,
    spam_score: Option<f64>,
    size_bytes: Option<i64>,
    file_name: String,
    received_at: DateTime<Utc>,
    status: String,
    resolved_at: Option<DateTime<Utc>>,
}

impl Row {
    fn into_item(self) -> (QuarantineItem, String) {
        let item = QuarantineItem {
            id: self.id,
            mail_id: self.mail_id,
            recipient: self.recipient,
            sender: self.sender,
            kind: QuarantineKind::parse(&self.kind).unwrap_or(QuarantineKind::Other),
            reason: self.reason,
            spam_score: self.spam_score,
            size_bytes: self.size_bytes,
            received_at: self.received_at,
            status: QuarantineStatus::parse(&self.status).unwrap_or(QuarantineStatus::Held),
            resolved_at: self.resolved_at,
        };
        (item, self.file_name)
    }
}

/// Escapes `%`, `_` and `\` so user text is matched literally by `ILIKE`.
fn like_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

impl QuarantineService {
    /// Creates the service.
    #[must_use]
    pub fn new(pool: PgPool, config: QuarantineConfig) -> Self {
        Self { pool, config }
    }

    /// Lists items matching `filter`, newest first.
    ///
    /// # Errors
    /// Fails on database errors.
    pub async fn list(&self, filter: &QuarantineFilter) -> Result<QuarantinePage> {
        let status = filter.status.map(QuarantineStatus::as_str);
        let kind = filter.kind.map(QuarantineKind::as_str);
        let search = filter
            .search
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| format!("%{}%", like_escape(s)));
        let domain = filter.domain.as_deref().map(str::to_lowercase);
        let limit = filter.limit.clamp(1, MAX_PAGE_SIZE);
        let offset = filter.offset.max(0);

        let total = sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!" FROM quarantine_items
               WHERE ($1::text IS NULL OR status = $1)
                 AND ($2::text IS NULL OR kind = $2)
                 AND ($3::text IS NULL OR recipient ILIKE $3 OR sender ILIKE $3)
                 AND ($4::text IS NULL OR split_part(recipient, '@', 2) = $4)"#,
            status,
            kind,
            search,
            domain
        )
        .fetch_one(&self.pool)
        .await?;

        let rows = sqlx::query_as!(
            Row,
            r#"SELECT id, mail_id, recipient, sender, kind, reason,
                      spam_score::float8 AS "spam_score?", size_bytes, file_name, received_at,
                      status, resolved_at
               FROM quarantine_items
               WHERE ($1::text IS NULL OR status = $1)
                 AND ($2::text IS NULL OR kind = $2)
                 AND ($3::text IS NULL OR recipient ILIKE $3 OR sender ILIKE $3)
                 AND ($4::text IS NULL OR split_part(recipient, '@', 2) = $4)
               ORDER BY received_at DESC, id
               LIMIT $5 OFFSET $6"#,
            status,
            kind,
            search,
            domain,
            limit,
            offset
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(QuarantinePage {
            items: rows.into_iter().map(|r| r.into_item().0).collect(),
            total,
        })
    }

    /// Counts held messages by category, optionally only for one recipient domain.
    ///
    /// # Errors
    /// Fails on database errors.
    pub async fn counts(&self, domain: Option<&str>) -> Result<QuarantineCounts> {
        let domain = domain.map(str::to_lowercase);
        let rows = sqlx::query!(
            r#"SELECT kind, count(*) AS "n!" FROM quarantine_items
               WHERE status = 'held' AND ($1::text IS NULL OR split_part(recipient, '@', 2) = $1)
               GROUP BY kind"#,
            domain
        )
        .fetch_all(&self.pool)
        .await?;
        let mut counts = QuarantineCounts::default();
        for row in rows {
            match row.kind.as_str() {
                "virus" => counts.virus += row.n,
                "spam" => counts.spam += row.n,
                _ => counts.other += row.n,
            }
        }
        Ok(counts)
    }

    async fn fetch(&self, id: Uuid) -> Result<(QuarantineItem, String)> {
        let row = sqlx::query_as!(
            Row,
            r#"SELECT id, mail_id, recipient, sender, kind, reason,
                      spam_score::float8 AS "spam_score?", size_bytes, file_name, received_at,
                      status, resolved_at
               FROM quarantine_items WHERE id = $1"#,
            id
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or(Error::NotFound {
            entity: "quarantine item",
        })?;
        Ok(row.into_item())
    }

    /// One item by id.
    ///
    /// # Errors
    /// [`Error::NotFound`] if there is no such item.
    pub async fn get(&self, id: Uuid) -> Result<QuarantineItem> {
        Ok(self.fetch(id).await?.0)
    }

    /// Headers and a short excerpt of the held message.
    ///
    /// # Errors
    /// [`Error::NotFound`] for an unknown id; [`Error::Provisioning`] if the file
    /// is missing, not a regular file, or too large.
    pub async fn preview(&self, id: Uuid) -> Result<MessagePreview> {
        let (_, file) = self.fetch(id).await?;
        let bytes = self.read_file(&file).await?;
        Ok(preview_of(&bytes))
    }

    /// Delivers the held message to its recipient, bypassing the filters, and
    /// marks it released.
    ///
    /// # Errors
    /// [`Error::Conflict`] if the item is not held; [`Error::Provisioning`] if the
    /// file or the SMTP hand-off fails (the item then stays held).
    pub async fn release(&self, id: Uuid, actor: &Actor) -> Result<QuarantineItem> {
        let mut tx = self.pool.begin().await?;
        // Lock the row so two administrators cannot release the same message twice.
        let locked = sqlx::query_as!(
            Row,
            r#"SELECT id, mail_id, recipient, sender, kind, reason,
                      spam_score::float8 AS "spam_score?", size_bytes, file_name, received_at,
                      status, resolved_at
               FROM quarantine_items WHERE id = $1 FOR UPDATE"#,
            id
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(Error::NotFound {
            entity: "quarantine item",
        })?;
        let (item, file) = locked.into_item();
        if item.status != QuarantineStatus::Held {
            return Err(Error::Conflict("the message is no longer held".into()));
        }
        let bytes = self.read_file(&file).await?;
        let message = strip_envelope_headers(&bytes);
        smtp::submit(
            &self.config.reinject_addr,
            &self.config.helo,
            &item.sender,
            &item.recipient,
            &message,
        )
        .await?;
        sqlx::query!(
            "UPDATE quarantine_items SET status = 'released', resolved_by = $2, resolved_at = now() WHERE id = $1",
            id,
            actor.user_id
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        self.audit("quarantine.release", &item, actor).await;
        self.get(id).await
    }

    /// Marks the item deleted and removes its file if no other recipient needs it.
    ///
    /// # Errors
    /// [`Error::Conflict`] if the item is not held.
    pub async fn delete(&self, id: Uuid, actor: &Actor) -> Result<()> {
        let (item, file) = self.fetch(id).await?;
        let changed = sqlx::query!(
            "UPDATE quarantine_items SET status = 'deleted', resolved_by = $2, resolved_at = now()
             WHERE id = $1 AND status = 'held'",
            id,
            actor.user_id
        )
        .execute(&self.pool)
        .await?;
        if changed.rows_affected() == 0 {
            return Err(Error::Conflict("the message is no longer held".into()));
        }
        self.remove_file_if_unused(&file).await?;
        self.audit("quarantine.delete", &item, actor).await;
        Ok(())
    }

    /// Deletes held messages older than `days` days (retention) and returns how
    /// many items were removed. Not attributed to a user.
    ///
    /// # Errors
    /// Fails on database errors or if `days` is not positive.
    pub async fn purge_older_than(&self, days: i32) -> Result<u64> {
        if days < 1 {
            return Err(Error::Validation("retention must be at least 1 day".into()));
        }
        let rows = sqlx::query!(
            r#"UPDATE quarantine_items
               SET status = 'deleted', resolved_at = now()
               WHERE status = 'held' AND received_at < now() - make_interval(days => $1)
               RETURNING file_name"#,
            days
        )
        .fetch_all(&self.pool)
        .await?;
        let count = rows.len() as u64;
        for row in rows {
            self.remove_file_if_unused(&row.file_name).await?;
        }
        Ok(count)
    }

    async fn remove_file_if_unused(&self, file: &str) -> Result<()> {
        let still_needed = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM quarantine_items WHERE file_name = $1 AND status = 'held') AS "x!""#,
            file
        )
        .fetch_one(&self.pool)
        .await?;
        if still_needed {
            return Ok(());
        }
        let path = self.path_for(file)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::Provisioning(format!(
                "could not remove the quarantined file: {e}"
            ))),
        }
    }

    fn path_for(&self, file: &str) -> Result<PathBuf> {
        if !is_safe_file_name(file) {
            return Err(Error::Validation("unsafe quarantine file name".into()));
        }
        Ok(self.config.dir.join(file))
    }

    async fn read_file(&self, file: &str) -> Result<Vec<u8>> {
        read_regular_file(&self.path_for(file)?).await
    }

    /// Best effort: a failure to write the audit entry is logged, not returned,
    /// because the action itself has already happened.
    async fn audit(&self, action: &str, item: &QuarantineItem, actor: &Actor) {
        let entry = NewAuditEntry {
            actor_user_id: Some(actor.user_id),
            actor_email: Some(actor.email.clone()),
            action: action.to_owned(),
            target_type: Some("quarantine_item".to_owned()),
            target_id: Some(item.id.to_string()),
            outcome: AuditOutcome::Success,
            ip_address: actor.ip_address.clone(),
            user_agent: None,
            details: serde_json::json!({
                "kind": item.kind.as_str(),
                "recipient": item.recipient,
                "mail_id": item.mail_id,
            }),
        };
        if let Err(error) = PgAuditLogRepository::new(self.pool.clone())
            .append(&entry)
            .await
        {
            tracing::error!(%error, action, "could not write the audit entry");
        }
    }
}

/// Reads a regular file (not a symlink, not a directory) of bounded size.
async fn read_regular_file(path: &Path) -> Result<Vec<u8>> {
    let missing = |e: std::io::Error| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::Provisioning("the quarantined message file is missing".into())
        } else {
            Error::Provisioning(format!("could not read the quarantined file: {e}"))
        }
    };
    let meta = tokio::fs::symlink_metadata(path).await.map_err(missing)?;
    if !meta.is_file() {
        return Err(Error::Provisioning(
            "the quarantined message is not a regular file".into(),
        ));
    }
    if meta.len() > MAX_MESSAGE_BYTES {
        return Err(Error::Provisioning(
            "the quarantined message is too large".into(),
        ));
    }
    let mut data = Vec::new();
    tokio::fs::File::open(path)
        .await
        .map_err(missing)?
        .take(MAX_MESSAGE_BYTES)
        .read_to_end(&mut data)
        .await
        .map_err(missing)?;
    Ok(data)
}

/// Splits a message into the header block and the body at the first blank line.
fn split_message(bytes: &[u8]) -> (&[u8], &[u8]) {
    let mut i = 0;
    while i < bytes.len() {
        let line_end = bytes[i..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |p| i + p);
        let line = &bytes[i..line_end];
        if line.is_empty() || line == b"\r" {
            return (&bytes[..i], &bytes[(line_end + 1).min(bytes.len())..]);
        }
        i = line_end + 1;
    }
    (bytes, &[])
}

/// Removes the header fields Amavis put on top of the original message.
///
/// Only the leading run of such fields is touched; once a field that is not
/// one of them appears, everything after it is kept exactly as it was.
#[must_use]
pub fn strip_envelope_headers(bytes: &[u8]) -> Vec<u8> {
    let (head, _) = split_message(bytes);
    let mut keep_from = 0;
    let mut offset = 0;
    let mut skipping = false;
    for line in head.split_inclusive(|&b| b == b'\n') {
        let is_continuation = line.first().is_some_and(|&b| b == b' ' || b == b'\t');
        if is_continuation {
            if skipping {
                offset += line.len();
                keep_from = offset;
            } else {
                break;
            }
            continue;
        }
        let name = line
            .iter()
            .position(|&b| b == b':')
            .map(|p| String::from_utf8_lossy(&line[..p]).to_ascii_lowercase());
        match name {
            Some(n) if AMAVIS_ENVELOPE_HEADERS.contains(&n.as_str()) => {
                skipping = true;
                offset += line.len();
                keep_from = offset;
            }
            _ => break,
        }
    }
    bytes[keep_from..].to_vec()
}

/// Builds the read-only preview.
fn preview_of(bytes: &[u8]) -> MessagePreview {
    const MAX_HEADERS: usize = 120;
    const MAX_VALUE: usize = 1000;
    const EXCERPT_BYTES: usize = 2048;

    let (head, body) = split_message(bytes);
    let text = String::from_utf8_lossy(head);
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some((_, value)) = headers.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
        } else if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_owned(), value.trim().to_owned()));
        }
        if headers.len() >= MAX_HEADERS {
            break;
        }
    }
    for (_, value) in &mut headers {
        *value = value.chars().take(MAX_VALUE).collect();
    }
    let plain = headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("content-type"))
        .is_none_or(|(_, v)| v.to_ascii_lowercase().starts_with("text/plain"));
    let (excerpt, truncated) = if plain {
        let cut = body.len().min(EXCERPT_BYTES);
        let shown: String = String::from_utf8_lossy(&body[..cut])
            .chars()
            .map(|c| {
                if c.is_control() && c != '\n' && c != '\t' {
                    ' '
                } else {
                    c
                }
            })
            .collect();
        (Some(shown), body.len() > cut)
    } else {
        (None, false)
    };
    MessagePreview {
        headers,
        excerpt,
        truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_bare_file_names_are_safe() {
        for ok in ["spam-QmGoT2LOSyav", "virus-9AzBOX7RY9-r", "a", "badh-x_y.z"] {
            assert!(is_safe_file_name(ok), "{ok}");
        }
        for bad in [
            "",
            "../etc/passwd",
            "a/b",
            "/abs",
            "a\\b",
            ".hidden",
            "-dash",
            "a..b",
            "a b",
            "a\0b",
            "naïve",
            &"x".repeat(129),
        ] {
            assert!(!is_safe_file_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn like_patterns_are_escaped() {
        assert_eq!(like_escape("50%_off\\"), "50\\%\\_off\\\\");
    }

    #[test]
    fn kinds_and_statuses_round_trip() {
        for k in [
            QuarantineKind::Virus,
            QuarantineKind::Spam,
            QuarantineKind::Banned,
            QuarantineKind::BadHeader,
            QuarantineKind::Other,
        ] {
            assert_eq!(QuarantineKind::parse(k.as_str()), Some(k));
        }
        for s in [
            QuarantineStatus::Held,
            QuarantineStatus::Released,
            QuarantineStatus::Deleted,
        ] {
            assert_eq!(QuarantineStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(QuarantineKind::parse("nope"), None);
    }

    const SAMPLE: &[u8] = b"Return-Path: <x@example.org>\nDelivered-To: spam-quarantine\nX-Envelope-From: <x@example.org>\nX-Envelope-To: <a@p5.test>\nX-Envelope-To-Blocked: <a@p5.test>\nX-Quarantine-ID: <abc>\nX-Spam-Flag: YES\nX-Spam-Status: Yes, score=9\n tests=[A=1]\nSubject: hello\nReturn-Path: <keep@me>\n\nbody line\n";

    #[test]
    fn amavis_envelope_headers_are_stripped_from_the_top_only() {
        let out = String::from_utf8(strip_envelope_headers(SAMPLE)).expect("utf8");
        assert!(out.starts_with("X-Spam-Flag: YES\n"), "{out}");
        assert!(!out.contains("Delivered-To"));
        assert!(!out.contains("X-Quarantine-ID"));
        assert!(
            out.contains("Return-Path: <keep@me>"),
            "later fields are untouched"
        );
        assert!(out.ends_with("body line\n"));
    }

    #[test]
    fn a_message_without_amavis_headers_is_unchanged() {
        let plain = b"Subject: x\n\nbody\n";
        assert_eq!(strip_envelope_headers(plain), plain);
        assert_eq!(strip_envelope_headers(b""), b"");
    }

    #[test]
    fn previews_unfold_headers_and_excerpt_plain_text() {
        let p = preview_of(SAMPLE);
        let status = p
            .headers
            .iter()
            .find(|(n, _)| n == "X-Spam-Status")
            .expect("header");
        assert_eq!(status.1, "Yes, score=9 tests=[A=1]");
        assert_eq!(p.excerpt.as_deref(), Some("body line\n"));
        assert!(!p.truncated);
    }

    #[test]
    fn non_text_messages_get_no_excerpt() {
        let p = preview_of(b"Content-Type: multipart/mixed; boundary=x\n\n--x\nbinary\n");
        assert_eq!(p.excerpt, None);
    }

    #[test]
    fn long_bodies_are_cut_and_control_characters_removed() {
        let mut msg = b"Subject: x\n\n".to_vec();
        msg.extend(std::iter::repeat_n(b'a', 5000));
        msg.push(0x1b);
        let p = preview_of(&msg);
        assert!(p.truncated);
        assert_eq!(p.excerpt.map(|e| e.len()), Some(2048));
        let p = preview_of(b"Subject: x\n\nhi\x1b[31m there");
        assert!(!p.excerpt.unwrap_or_default().contains('\u{1b}'));
    }
}
