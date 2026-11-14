//! PostgreSQL implementation of [`QuotaRepository`].

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use super::traits::QuotaRepository;
use crate::domain::QuotaUsage;
use crate::error::{Error, Result};

/// PostgreSQL-backed [`QuotaRepository`].
#[derive(Debug, Clone)]
pub struct PgQuotaRepository {
    pool: PgPool,
}

impl PgQuotaRepository {
    /// Creates a repository using `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl QuotaRepository for PgQuotaRepository {
    async fn get(&self, mailbox_id: Uuid) -> Result<Option<QuotaUsage>> {
        let usage = sqlx::query_as!(
            QuotaUsage,
            "SELECT mailbox_id, bytes_used, messages_count, updated_at
             FROM quota_usage WHERE mailbox_id = $1",
            mailbox_id,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(usage)
    }

    async fn set_usage(
        &self,
        mailbox_id: Uuid,
        bytes_used: i64,
        messages_count: i64,
    ) -> Result<QuotaUsage> {
        if bytes_used < 0 || messages_count < 0 {
            return Err(Error::Validation(
                "usage figures must not be negative".into(),
            ));
        }
        let usage = sqlx::query_as!(
            QuotaUsage,
            "INSERT INTO quota_usage (mailbox_id, bytes_used, messages_count)
             VALUES ($1, $2, $3)
             ON CONFLICT (mailbox_id) DO UPDATE
                SET bytes_used = EXCLUDED.bytes_used,
                    messages_count = EXCLUDED.messages_count,
                    updated_at = now()
             RETURNING mailbox_id, bytes_used, messages_count, updated_at",
            mailbox_id,
            bytes_used,
            messages_count,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(usage)
    }

    async fn apply_delta(
        &self,
        mailbox_id: Uuid,
        bytes_delta: i64,
        messages_delta: i64,
    ) -> Result<QuotaUsage> {
        // Single atomic upsert so concurrent deliveries cannot lose updates.
        let usage = sqlx::query_as!(
            QuotaUsage,
            "INSERT INTO quota_usage (mailbox_id, bytes_used, messages_count)
             VALUES ($1, GREATEST($2::bigint, 0), GREATEST($3::bigint, 0))
             ON CONFLICT (mailbox_id) DO UPDATE
                SET bytes_used = GREATEST(quota_usage.bytes_used + $2::bigint, 0),
                    messages_count = GREATEST(quota_usage.messages_count + $3::bigint, 0),
                    updated_at = now()
             RETURNING mailbox_id, bytes_used, messages_count, updated_at",
            mailbox_id,
            bytes_delta,
            messages_delta,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(usage)
    }
}
