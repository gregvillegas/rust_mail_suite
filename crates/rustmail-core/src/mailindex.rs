//! The webmail search index (`messages_meta`) and its sync bookkeeping.
//!
//! The index holds headers and a short snippet for each message, never the full
//! message. It is filled by the webmail from IMAP and queried with PostgreSQL
//! full-text search. The IMAP server stays the source of truth: a hit is only a
//! pointer (folder, UIDVALIDITY, UID) that may be stale if mail was moved or
//! deleted by another client.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{Error, Result};

/// Longest snippet stored.
pub const MAX_SNIPPET_CHARS: usize = 240;
/// Longest search text accepted.
pub const MAX_QUERY_CHARS: usize = 200;
/// Largest page of search results.
pub const MAX_PAGE_SIZE: i64 = 100;

/// One message as stored in the index.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexedMessage {
    /// IMAP folder name (decoded, not modified UTF-7).
    pub folder: String,
    /// The folder's UIDVALIDITY when the UID was read.
    pub uid_validity: i64,
    /// IMAP UID.
    pub imap_uid: i64,
    /// `Message-ID` header.
    pub message_id: Option<String>,
    /// `In-Reply-To` header.
    pub in_reply_to: Option<String>,
    /// Subject.
    pub subject: String,
    /// Sender, as displayed.
    pub from_addr: String,
    /// Recipients, comma separated.
    pub to_addrs: String,
    /// Cc recipients, comma separated.
    pub cc_addrs: String,
    /// The start of the text, or empty.
    pub snippet: String,
    /// Size in bytes.
    pub size_bytes: i64,
    /// IMAP flags such as `\Seen`.
    pub flags: Vec<String>,
    /// Whether the message has attachments.
    pub has_attachments: bool,
    /// The `Date` header.
    pub date_header: Option<DateTime<Utc>>,
    /// When the server received it.
    pub received_at: DateTime<Utc>,
}

/// A search result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchHit {
    /// Folder holding the message.
    pub folder: String,
    /// UIDVALIDITY recorded at indexing time.
    pub uid_validity: i64,
    /// IMAP UID.
    pub imap_uid: i64,
    /// Subject.
    pub subject: String,
    /// Sender.
    pub from_addr: String,
    /// Start of the text.
    pub snippet: String,
    /// When the server received it.
    pub received_at: DateTime<Utc>,
    /// IMAP flags.
    pub flags: Vec<String>,
    /// Whether it has attachments.
    pub has_attachments: bool,
}

/// A page of search results.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchPage {
    /// The hits.
    pub hits: Vec<SearchHit>,
    /// Total matches.
    pub total: i64,
}

/// How far a folder has been indexed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncState {
    /// UIDVALIDITY seen at the last sync.
    pub uid_validity: i64,
    /// Highest UID indexed.
    pub last_uid: i64,
    /// When the last sync finished.
    pub synced_at: DateTime<Utc>,
}

/// Prepares user text for the index's tokeniser: punctuation that the default
/// parser would glue into one token (`bob@example.com`) becomes a space, in the
/// same way the stored text is prepared. Returns `None` if nothing searchable is left.
#[must_use]
pub fn search_terms(input: &str) -> Option<String> {
    let cleaned: String = input
        .chars()
        .take(MAX_QUERY_CHARS)
        .map(|c| {
            if c.is_control()
                || matches!(
                    c,
                    '@' | '.' | '_' | '-' | '<' | '>' | ',' | ';' | ':' | '(' | ')' | '[' | ']'
                )
            {
                ' '
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    (!trimmed.is_empty()).then_some(trimmed)
}

/// PostgreSQL access to the index.
#[derive(Debug, Clone)]
pub struct MailIndex {
    pool: PgPool,
}

impl MailIndex {
    /// Creates the index handle.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// The mailbox id for a user, if they have one.
    ///
    /// # Errors
    /// Fails on database errors.
    pub async fn mailbox_id(&self, user_id: Uuid) -> Result<Option<Uuid>> {
        Ok(
            sqlx::query_scalar!("SELECT id FROM mailboxes WHERE user_id = $1", user_id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    /// Inserts or refreshes messages. Re-indexing the same message is harmless.
    ///
    /// # Errors
    /// Fails on database errors; nothing is stored in that case.
    pub async fn upsert(&self, mailbox_id: Uuid, messages: &[IndexedMessage]) -> Result<usize> {
        let mut tx = self.pool.begin().await?;
        for m in messages {
            let snippet: String = m.snippet.chars().take(MAX_SNIPPET_CHARS).collect();
            sqlx::query!(
                r#"INSERT INTO messages_meta
                       (mailbox_id, folder, uid_validity, imap_uid, message_id, in_reply_to, subject,
                        from_addr, to_addrs, cc_addrs, snippet, size_bytes, flags, has_attachments,
                        date_header, received_at)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
                   ON CONFLICT (mailbox_id, folder, uid_validity, imap_uid) DO UPDATE SET
                       message_id = EXCLUDED.message_id, in_reply_to = EXCLUDED.in_reply_to,
                       subject = EXCLUDED.subject, from_addr = EXCLUDED.from_addr,
                       to_addrs = EXCLUDED.to_addrs, cc_addrs = EXCLUDED.cc_addrs,
                       snippet = CASE WHEN EXCLUDED.snippet <> '' THEN EXCLUDED.snippet
                                      ELSE messages_meta.snippet END,
                       size_bytes = EXCLUDED.size_bytes, flags = EXCLUDED.flags,
                       has_attachments = EXCLUDED.has_attachments,
                       date_header = EXCLUDED.date_header, received_at = EXCLUDED.received_at"#,
                mailbox_id,
                m.folder,
                m.uid_validity,
                m.imap_uid,
                m.message_id,
                m.in_reply_to,
                m.subject,
                m.from_addr,
                m.to_addrs,
                m.cc_addrs,
                snippet,
                m.size_bytes,
                &m.flags,
                m.has_attachments,
                m.date_header,
                m.received_at,
            )
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(messages.len())
    }

    /// Removes one message from the index (after it was moved or deleted).
    ///
    /// # Errors
    /// Fails on database errors.
    pub async fn remove(&self, mailbox_id: Uuid, folder: &str, imap_uid: i64) -> Result<()> {
        sqlx::query!(
            "DELETE FROM messages_meta WHERE mailbox_id = $1 AND folder = $2 AND imap_uid = $3",
            mailbox_id,
            folder,
            imap_uid
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Forgets everything indexed for a folder (it was renumbered, renamed or deleted).
    ///
    /// # Errors
    /// Fails on database errors.
    pub async fn clear_folder(&self, mailbox_id: Uuid, folder: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query!(
            "DELETE FROM messages_meta WHERE mailbox_id = $1 AND folder = $2",
            mailbox_id,
            folder
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "DELETE FROM mail_sync_state WHERE mailbox_id = $1 AND folder = $2",
            mailbox_id,
            folder
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Reads a folder's sync state.
    ///
    /// # Errors
    /// Fails on database errors.
    pub async fn sync_state(&self, mailbox_id: Uuid, folder: &str) -> Result<Option<SyncState>> {
        let row = sqlx::query!(
            "SELECT uid_validity, last_uid, synced_at FROM mail_sync_state
             WHERE mailbox_id = $1 AND folder = $2",
            mailbox_id,
            folder
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| SyncState {
            uid_validity: r.uid_validity,
            last_uid: r.last_uid,
            synced_at: r.synced_at,
        }))
    }

    /// Records how far a folder has been indexed.
    ///
    /// # Errors
    /// Fails on database errors.
    pub async fn set_sync_state(
        &self,
        mailbox_id: Uuid,
        folder: &str,
        uid_validity: i64,
        last_uid: i64,
    ) -> Result<()> {
        sqlx::query!(
            r#"INSERT INTO mail_sync_state (mailbox_id, folder, uid_validity, last_uid)
               VALUES ($1, $2, $3, $4)
               ON CONFLICT (mailbox_id, folder) DO UPDATE
               SET uid_validity = EXCLUDED.uid_validity, last_uid = EXCLUDED.last_uid,
                   synced_at = now()"#,
            mailbox_id,
            folder,
            uid_validity,
            last_uid
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Full-text search over one mailbox, best matches first.
    ///
    /// # Errors
    /// [`Error::Validation`] if the text contains nothing searchable.
    pub async fn search(
        &self,
        mailbox_id: Uuid,
        text: &str,
        limit: i64,
        offset: i64,
    ) -> Result<SearchPage> {
        let terms = search_terms(text)
            .ok_or_else(|| Error::Validation("enter something to search for".into()))?;
        let limit = limit.clamp(1, MAX_PAGE_SIZE);
        let offset = offset.max(0);
        let total = sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!" FROM messages_meta
               WHERE mailbox_id = $1 AND search_vector @@ websearch_to_tsquery('simple', $2)"#,
            mailbox_id,
            terms
        )
        .fetch_one(&self.pool)
        .await?;
        let rows = sqlx::query!(
            r#"SELECT folder, uid_validity, imap_uid, subject, from_addr, snippet, received_at,
                      flags, has_attachments
               FROM messages_meta
               WHERE mailbox_id = $1 AND search_vector @@ websearch_to_tsquery('simple', $2)
               ORDER BY ts_rank(search_vector, websearch_to_tsquery('simple', $2)) DESC,
                        received_at DESC, id DESC
               LIMIT $3 OFFSET $4"#,
            mailbox_id,
            terms,
            limit,
            offset
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(SearchPage {
            total,
            hits: rows
                .into_iter()
                .map(|r| SearchHit {
                    folder: r.folder,
                    uid_validity: r.uid_validity,
                    imap_uid: r.imap_uid,
                    subject: r.subject,
                    from_addr: r.from_addr,
                    snippet: r.snippet,
                    received_at: r.received_at,
                    flags: r.flags,
                    has_attachments: r.has_attachments,
                })
                .collect(),
        })
    }

    /// How many messages are indexed for the mailbox.
    ///
    /// # Errors
    /// Fails on database errors.
    pub async fn count(&self, mailbox_id: Uuid) -> Result<i64> {
        Ok(sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!" FROM messages_meta WHERE mailbox_id = $1"#,
            mailbox_id
        )
        .fetch_one(&self.pool)
        .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_text_is_prepared_like_the_index() {
        assert_eq!(
            search_terms("bob@example.com").as_deref(),
            Some("bob example com")
        );
        assert_eq!(
            search_terms("  Hello,   World ").as_deref(),
            Some("Hello World")
        );
        assert_eq!(search_terms("a_b-c").as_deref(), Some("a b c"));
        assert_eq!(
            search_terms("\"quoted phrase\"").as_deref(),
            Some("\"quoted phrase\"")
        );
        assert_eq!(search_terms("@.-_"), None);
        assert_eq!(search_terms("   "), None);
        assert_eq!(search_terms("a\nb\0c").as_deref(), Some("a b c"));
    }

    #[test]
    fn overlong_queries_are_cut() {
        let long = "x".repeat(MAX_QUERY_CHARS * 3);
        assert_eq!(search_terms(&long).map(|s| s.len()), Some(MAX_QUERY_CHARS));
    }
}
