//! PostgreSQL implementation of [`SessionRepository`].

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use super::traits::SessionRepository;
use crate::domain::{NewSession, Session};
use crate::error::{not_found, Error, Result};

/// PostgreSQL-backed [`SessionRepository`].
#[derive(Debug, Clone)]
pub struct PgSessionRepository {
    pool: PgPool,
}

impl PgSessionRepository {
    /// Creates a repository using `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SessionRepository for PgSessionRepository {
    async fn create(&self, new: &NewSession) -> Result<Session> {
        let session = sqlx::query_as!(
            Session,
            r#"INSERT INTO sessions (user_id, refresh_token_hash, user_agent, ip_address, expires_at)
               VALUES ($1, $2, $3, $4, $5)
               RETURNING id, user_id, refresh_token_hash, user_agent, ip_address,
                         created_at, last_used_at, expires_at, revoked_at, replaced_by"#,
            new.user_id,
            new.refresh_token_hash,
            new.user_agent,
            new.ip_address,
            new.expires_at,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(session)
    }

    async fn find_active_by_token_hash(&self, token_hash: &str) -> Result<Option<Session>> {
        let session = sqlx::query_as!(
            Session,
            r#"SELECT id, user_id, refresh_token_hash, user_agent, ip_address,
                      created_at, last_used_at, expires_at, revoked_at, replaced_by
               FROM sessions
               WHERE refresh_token_hash = $1 AND revoked_at IS NULL AND expires_at > now()"#,
            token_hash,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(session)
    }

    async fn find_by_token_hash(&self, token_hash: &str) -> Result<Option<Session>> {
        let session = sqlx::query_as!(
            Session,
            r#"SELECT id, user_id, refresh_token_hash, user_agent, ip_address,
                      created_at, last_used_at, expires_at, revoked_at, replaced_by
               FROM sessions WHERE refresh_token_hash = $1"#,
            token_hash,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(session)
    }

    async fn find_active_by_id(&self, id: Uuid) -> Result<Option<Session>> {
        let session = sqlx::query_as!(
            Session,
            r#"SELECT id, user_id, refresh_token_hash, user_agent, ip_address,
                      created_at, last_used_at, expires_at, revoked_at, replaced_by
               FROM sessions
               WHERE id = $1 AND revoked_at IS NULL AND expires_at > now()"#,
            id,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(session)
    }

    async fn rotate(&self, old_id: Uuid, new: &NewSession) -> Result<Option<Session>> {
        let mut tx = self.pool.begin().await?;

        // The conditional UPDATE is the lock: of two concurrent rotations of the
        // same session, the second waits, re-checks `revoked_at IS NULL`, finds
        // no row, and returns `None`.
        let old = sqlx::query!(
            "UPDATE sessions SET revoked_at = now()
             WHERE id = $1 AND revoked_at IS NULL AND expires_at > now()
             RETURNING user_id",
            old_id,
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some(old) = old else {
            return Ok(None);
        };
        if old.user_id != new.user_id {
            // Dropping `tx` rolls the revocation back.
            return Err(Error::Validation(
                "session rotation must keep the same user".into(),
            ));
        }

        let session = sqlx::query_as!(
            Session,
            r#"INSERT INTO sessions (user_id, refresh_token_hash, user_agent, ip_address, expires_at)
               VALUES ($1, $2, $3, $4, $5)
               RETURNING id, user_id, refresh_token_hash, user_agent, ip_address,
                         created_at, last_used_at, expires_at, revoked_at, replaced_by"#,
            new.user_id,
            new.refresh_token_hash,
            new.user_agent,
            new.ip_address,
            new.expires_at,
        )
        .fetch_one(&mut *tx)
        .await?;

        sqlx::query!(
            "UPDATE sessions SET replaced_by = $2 WHERE id = $1",
            old_id,
            session.id
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(session))
    }

    async fn touch(&self, id: Uuid) -> Result<()> {
        let result = sqlx::query!(
            "UPDATE sessions SET last_used_at = now() WHERE id = $1 AND revoked_at IS NULL",
            id,
        )
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(not_found("session"));
        }
        Ok(())
    }

    async fn revoke(&self, id: Uuid) -> Result<()> {
        // Idempotent: revoking a missing or already revoked session is fine.
        sqlx::query!(
            "UPDATE sessions SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL",
            id,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn revoke_all_for_user(&self, user_id: Uuid) -> Result<u64> {
        let result = sqlx::query!(
            "UPDATE sessions SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
            user_id,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    async fn delete_expired(&self) -> Result<u64> {
        let result = sqlx::query!("DELETE FROM sessions WHERE expires_at <= now()")
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}
