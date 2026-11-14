//! PostgreSQL implementation of [`MailboxRepository`].

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use super::traits::MailboxRepository;
use crate::domain::{AccountStatus, Mailbox, NewMailbox};
use crate::error::{not_found, Error, Result};

/// PostgreSQL-backed [`MailboxRepository`].
#[derive(Debug, Clone)]
pub struct PgMailboxRepository {
    pool: PgPool,
}

impl PgMailboxRepository {
    /// Creates a repository using `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl MailboxRepository for PgMailboxRepository {
    async fn create(&self, new: &NewMailbox) -> Result<Mailbox> {
        // The path is joined onto the vmail base directory later, so it must
        // stay relative and inside it. (The database also rejects `..`.)
        if new.maildir_path.starts_with('/') {
            return Err(Error::Validation("maildir path must be relative".into()));
        }
        let mailbox = sqlx::query_as!(
            Mailbox,
            r#"INSERT INTO mailboxes (user_id, maildir_path, quota_bytes)
               VALUES ($1, $2, $3)
               RETURNING id, user_id, maildir_path, quota_bytes,
                         status AS "status: AccountStatus", created_at, updated_at"#,
            new.user_id,
            new.maildir_path,
            new.quota_bytes,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(mailbox)
    }

    async fn get(&self, id: Uuid) -> Result<Mailbox> {
        sqlx::query_as!(
            Mailbox,
            r#"SELECT id, user_id, maildir_path, quota_bytes,
                      status AS "status: AccountStatus", created_at, updated_at
               FROM mailboxes WHERE id = $1"#,
            id,
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| not_found("mailbox"))
    }

    async fn find_by_user(&self, user_id: Uuid) -> Result<Option<Mailbox>> {
        let mailbox = sqlx::query_as!(
            Mailbox,
            r#"SELECT id, user_id, maildir_path, quota_bytes,
                      status AS "status: AccountStatus", created_at, updated_at
               FROM mailboxes WHERE user_id = $1"#,
            user_id,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(mailbox)
    }

    async fn effective_quota_bytes(&self, id: Uuid) -> Result<Option<i64>> {
        let row = sqlx::query!(
            r#"SELECT COALESCE(m.quota_bytes, d.default_quota_bytes) AS quota
               FROM mailboxes m
               JOIN users u ON u.id = m.user_id
               JOIN domains d ON d.id = u.domain_id
               WHERE m.id = $1"#,
            id,
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| not_found("mailbox"))?;
        Ok(row.quota)
    }

    async fn set_quota(&self, id: Uuid, quota_bytes: Option<i64>) -> Result<()> {
        let result = sqlx::query!(
            "UPDATE mailboxes SET quota_bytes = $2 WHERE id = $1",
            id,
            quota_bytes
        )
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(not_found("mailbox"));
        }
        Ok(())
    }

    async fn set_status(&self, id: Uuid, status: AccountStatus) -> Result<()> {
        let result = sqlx::query!(
            "UPDATE mailboxes SET status = $2 WHERE id = $1",
            id,
            status as AccountStatus,
        )
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(not_found("mailbox"));
        }
        Ok(())
    }

    async fn delete(&self, id: Uuid) -> Result<()> {
        let result = sqlx::query!("DELETE FROM mailboxes WHERE id = $1", id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(not_found("mailbox"));
        }
        Ok(())
    }
}
