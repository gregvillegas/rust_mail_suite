//! PostgreSQL implementation of [`DomainRepository`].

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use super::traits::DomainRepository;
use crate::domain::validation::normalize_domain_name;
use crate::domain::{AccountStatus, MailDomain, NewDomain};
use crate::error::{not_found, Result};

/// PostgreSQL-backed [`DomainRepository`].
#[derive(Debug, Clone)]
pub struct PgDomainRepository {
    pool: PgPool,
}

impl PgDomainRepository {
    /// Creates a repository using `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl DomainRepository for PgDomainRepository {
    async fn create(&self, new: &NewDomain) -> Result<MailDomain> {
        let name = normalize_domain_name(&new.name)?;
        let domain = sqlx::query_as!(
            MailDomain,
            r#"INSERT INTO domains (name, default_quota_bytes, max_mailboxes)
               VALUES ($1, $2, $3)
               RETURNING id, name, status AS "status: AccountStatus",
                         default_quota_bytes, max_mailboxes, created_at, updated_at"#,
            name,
            new.default_quota_bytes,
            new.max_mailboxes,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(domain)
    }

    async fn get(&self, id: Uuid) -> Result<MailDomain> {
        sqlx::query_as!(
            MailDomain,
            r#"SELECT id, name, status AS "status: AccountStatus",
                      default_quota_bytes, max_mailboxes, created_at, updated_at
               FROM domains WHERE id = $1"#,
            id,
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| not_found("domain"))
    }

    async fn find_by_name(&self, name: &str) -> Result<Option<MailDomain>> {
        let Ok(name) = normalize_domain_name(name) else {
            return Ok(None);
        };
        let domain = sqlx::query_as!(
            MailDomain,
            r#"SELECT id, name, status AS "status: AccountStatus",
                      default_quota_bytes, max_mailboxes, created_at, updated_at
               FROM domains WHERE name = $1"#,
            name,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(domain)
    }

    async fn list(&self) -> Result<Vec<MailDomain>> {
        let domains = sqlx::query_as!(
            MailDomain,
            r#"SELECT id, name, status AS "status: AccountStatus",
                      default_quota_bytes, max_mailboxes, created_at, updated_at
               FROM domains ORDER BY name"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(domains)
    }

    async fn set_status(&self, id: Uuid, status: AccountStatus) -> Result<()> {
        let result = sqlx::query!(
            "UPDATE domains SET status = $2 WHERE id = $1",
            id,
            status as AccountStatus,
        )
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(not_found("domain"));
        }
        Ok(())
    }

    async fn delete(&self, id: Uuid) -> Result<()> {
        let result = sqlx::query!("DELETE FROM domains WHERE id = $1", id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(not_found("domain"));
        }
        Ok(())
    }
}
