//! Persisting pipeline events and the log follower's position.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use super::{EventKind, EventSource, PipelineEvent};
use crate::error::{Error, Result};
use crate::quarantine::is_safe_file_name;

/// Where a log reader stopped: which file (by inode) and how far into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogPosition {
    /// Inode of the file that was being read, to notice rotation.
    pub inode: u64,
    /// Number of bytes already processed.
    pub offset: u64,
}

/// What [`PipelineStore::record`] wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecordSummary {
    /// Events inserted.
    pub events: usize,
    /// New quarantine rows created.
    pub quarantined: usize,
}

/// A stored event, as shown in listings.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    /// Row id.
    pub id: i64,
    /// When the daemon logged it.
    pub occurred_at: DateTime<Utc>,
    /// `amavis` or `postfix`.
    pub source: String,
    /// `passed`, `blocked`, `delivery` or `reject`.
    pub kind: String,
    /// Amavis verdict.
    pub verdict: Option<String>,
    /// Envelope sender.
    pub sender: String,
    /// Envelope recipient.
    pub recipient: String,
    /// Spam score.
    pub spam_score: Option<f64>,
    /// Virus name.
    pub virus_name: Option<String>,
    /// Delivery status.
    pub status: Option<String>,
}

/// PostgreSQL storage for pipeline events.
#[derive(Debug, Clone)]
pub struct PipelineStore {
    pool: PgPool,
}

impl PipelineStore {
    /// Creates a store using `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// The saved position for `path`, if it has been read before.
    ///
    /// # Errors
    /// Fails on database errors.
    pub async fn position(&self, path: &str) -> Result<Option<LogPosition>> {
        let row = sqlx::query!(
            "SELECT inode, byte_offset FROM pipeline_log_state WHERE path = $1",
            path
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| LogPosition {
            inode: u64::try_from(r.inode).unwrap_or(0),
            offset: u64::try_from(r.byte_offset).unwrap_or(0),
        }))
    }

    /// Stores `events` and the new position for `path` in one transaction, so a
    /// crash can neither lose events nor store them twice. Blocked messages that
    /// were quarantined also get a row in `quarantine_items`.
    ///
    /// # Errors
    /// Fails on database errors; nothing is stored in that case.
    pub async fn record(
        &self,
        path: &str,
        position: LogPosition,
        events: &[PipelineEvent],
    ) -> Result<RecordSummary> {
        let inode = i64::try_from(position.inode)
            .map_err(|_| Error::Validation("inode out of range".into()))?;
        let offset = i64::try_from(position.offset)
            .map_err(|_| Error::Validation("offset out of range".into()))?;
        let mut tx = self.pool.begin().await?;
        let mut summary = RecordSummary::default();
        for event in events {
            sqlx::query!(
                r#"INSERT INTO pipeline_events
                       (occurred_at, source, kind, queue_id, mail_id, verdict, virus_name,
                        spam_score, sender, recipient, status, detail, size_bytes, quarantine_ref)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8::float8::numeric, $9, $10, $11, $12, $13, $14)"#,
                event.occurred_at,
                event.source.as_str(),
                event.kind.as_str(),
                event.queue_id,
                event.mail_id,
                event.verdict,
                event.virus_name,
                event.spam_score,
                event.sender,
                event.recipient,
                event.status,
                event.detail,
                event.size_bytes,
                event.quarantine_ref,
            )
            .execute(&mut *tx)
            .await?;
            summary.events += 1;

            if let (EventSource::Amavis, EventKind::Blocked, Some(mail_id), Some(file)) = (
                event.source,
                event.kind,
                event.mail_id.as_deref(),
                event.quarantine_ref.as_deref(),
            ) {
                if !is_safe_file_name(file) {
                    tracing::warn!("ignoring quarantine reference with an unsafe name");
                    continue;
                }
                let kind = quarantine_kind(event.verdict.as_deref());
                let reason = quarantine_reason(event);
                let inserted = sqlx::query!(
                    r#"INSERT INTO quarantine_items
                           (mail_id, recipient, sender, kind, reason, spam_score, size_bytes,
                            file_name, received_at)
                       VALUES ($1, $2, $3, $4, $5, $6::float8::numeric, $7, $8, $9)
                       ON CONFLICT (mail_id, recipient) DO NOTHING"#,
                    mail_id,
                    event.recipient,
                    event.sender,
                    kind,
                    reason,
                    event.spam_score,
                    event.size_bytes,
                    file,
                    event.occurred_at,
                )
                .execute(&mut *tx)
                .await?;
                summary.quarantined += usize::try_from(inserted.rows_affected()).unwrap_or(0);
            }
        }
        sqlx::query!(
            r#"INSERT INTO pipeline_log_state (path, inode, byte_offset) VALUES ($1, $2, $3)
               ON CONFLICT (path) DO UPDATE
               SET inode = EXCLUDED.inode, byte_offset = EXCLUDED.byte_offset, updated_at = now()"#,
            path,
            inode,
            offset,
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(summary)
    }

    /// The most recent events, newest first.
    ///
    /// # Errors
    /// Fails on database errors.
    pub async fn recent(&self, limit: i64) -> Result<Vec<StoredEvent>> {
        let limit = limit.clamp(1, 500);
        let rows = sqlx::query!(
            r#"SELECT id, occurred_at, source, kind, verdict, sender, recipient,
                      spam_score::float8 AS "spam_score?", virus_name, status
               FROM pipeline_events ORDER BY occurred_at DESC, id DESC LIMIT $1"#,
            limit
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| StoredEvent {
                id: r.id,
                occurred_at: r.occurred_at,
                source: r.source,
                kind: r.kind,
                verdict: r.verdict,
                sender: r.sender,
                recipient: r.recipient,
                spam_score: r.spam_score,
                virus_name: r.virus_name,
                status: r.status,
            })
            .collect())
    }

    /// Deletes events older than `days` days and returns how many were removed.
    ///
    /// # Errors
    /// Fails on database errors or if `days` is not positive.
    pub async fn prune(&self, days: i32) -> Result<u64> {
        if days < 1 {
            return Err(Error::Validation("retention must be at least 1 day".into()));
        }
        let done = sqlx::query!(
            "DELETE FROM pipeline_events WHERE occurred_at < now() - make_interval(days => $1)",
            days
        )
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected())
    }
}

/// Maps an Amavis verdict to a quarantine category.
fn quarantine_kind(verdict: Option<&str>) -> &'static str {
    match verdict {
        Some("INFECTED") => "virus",
        Some("SPAM" | "SPAMMY") => "spam",
        Some("BANNED") => "banned",
        Some("BAD-HEADER") => "bad-header",
        _ => "other",
    }
}

/// What a person wants to see in the "why" column.
fn quarantine_reason(event: &PipelineEvent) -> String {
    if let Some(virus) = &event.virus_name {
        return super::clip(virus, 200);
    }
    if let Some(score) = event.spam_score {
        return format!("score {score:.1}");
    }
    event
        .detail
        .as_deref()
        .map(|d| super::clip(d, 200))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdicts_map_to_categories() {
        assert_eq!(quarantine_kind(Some("INFECTED")), "virus");
        assert_eq!(quarantine_kind(Some("SPAM")), "spam");
        assert_eq!(quarantine_kind(Some("BANNED")), "banned");
        assert_eq!(quarantine_kind(Some("BAD-HEADER")), "bad-header");
        assert_eq!(quarantine_kind(Some("CLEAN")), "other");
        assert_eq!(quarantine_kind(None), "other");
    }
}
