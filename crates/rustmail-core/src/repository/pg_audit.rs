//! PostgreSQL implementation of [`AuditLogRepository`].

use async_trait::async_trait;
use sqlx::PgPool;

use super::traits::AuditLogRepository;
use crate::domain::{AuditEntry, AuditOutcome, AuditQuery, NewAuditEntry};
use crate::error::{Error, Result};

const MAX_PAGE_SIZE: i64 = 500;

/// PostgreSQL-backed [`AuditLogRepository`].
#[derive(Debug, Clone)]
pub struct PgAuditLogRepository {
    pool: PgPool,
}

impl PgAuditLogRepository {
    /// Creates a repository using `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AuditLogRepository for PgAuditLogRepository {
    async fn append(&self, entry: &NewAuditEntry) -> Result<AuditEntry> {
        if entry.action.trim().is_empty() {
            return Err(Error::Validation("audit action must not be empty".into()));
        }
        let saved = sqlx::query_as!(
            AuditEntry,
            r#"INSERT INTO audit_log (actor_user_id, actor_email, action, target_type, target_id,
                                      outcome, ip_address, user_agent, details)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
               RETURNING id, occurred_at, actor_user_id, actor_email, action, target_type,
                         target_id, outcome AS "outcome: AuditOutcome", ip_address, user_agent,
                         details"#,
            entry.actor_user_id,
            entry.actor_email,
            entry.action,
            entry.target_type,
            entry.target_id,
            entry.outcome as AuditOutcome,
            entry.ip_address,
            entry.user_agent,
            entry.details,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(saved)
    }

    async fn list(&self, query: &AuditQuery) -> Result<Vec<AuditEntry>> {
        let limit = query.limit.clamp(1, MAX_PAGE_SIZE);
        let offset = query.offset.max(0);
        let entries = sqlx::query_as!(
            AuditEntry,
            r#"SELECT id, occurred_at, actor_user_id, actor_email, action, target_type,
                      target_id, outcome AS "outcome: AuditOutcome", ip_address, user_agent,
                      details
               FROM audit_log
               WHERE ($1::uuid IS NULL OR actor_user_id = $1)
                 AND ($2::text IS NULL OR action = $2)
               ORDER BY occurred_at DESC, id DESC
               LIMIT $3 OFFSET $4"#,
            query.actor_user_id,
            query.action,
            limit,
            offset,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(entries)
    }
}
