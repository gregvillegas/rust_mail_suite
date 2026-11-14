//! PostgreSQL implementation of [`AliasRepository`].

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use super::traits::AliasRepository;
use crate::domain::validation::normalize_email;
use crate::domain::{Alias, NewAlias};
use crate::error::{not_found, Error, Result};

/// PostgreSQL-backed [`AliasRepository`].
#[derive(Debug, Clone)]
pub struct PgAliasRepository {
    pool: PgPool,
}

impl PgAliasRepository {
    /// Creates a repository using `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AliasRepository for PgAliasRepository {
    async fn create(&self, new: &NewAlias) -> Result<Alias> {
        let address = normalize_email(&new.address)?;
        let destination = normalize_email(&new.destination)?;
        // The alias address must live in the domain it is attached to.
        sqlx::query_as!(
            Alias,
            r#"INSERT INTO aliases (domain_id, address, destination)
               SELECT d.id, $2::text, $3::text
               FROM domains d
               WHERE d.id = $1 AND split_part($2::text, '@', 2) = d.name
               RETURNING id, domain_id, address, destination, enabled, created_at"#,
            new.domain_id,
            address,
            destination,
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| {
            Error::Validation("domain not found or alias address not in that domain".into())
        })
    }

    async fn list_by_domain(&self, domain_id: Uuid) -> Result<Vec<Alias>> {
        let aliases = sqlx::query_as!(
            Alias,
            r#"SELECT id, domain_id, address, destination, enabled, created_at
               FROM aliases WHERE domain_id = $1 ORDER BY address, destination"#,
            domain_id,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(aliases)
    }

    async fn resolve(&self, address: &str) -> Result<Vec<String>> {
        let Ok(address) = normalize_email(address) else {
            return Ok(Vec::new());
        };
        let destinations = sqlx::query_scalar!(
            "SELECT destination FROM aliases WHERE address = $1 AND enabled ORDER BY destination",
            address,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(destinations)
    }

    async fn set_enabled(&self, id: Uuid, enabled: bool) -> Result<()> {
        let result = sqlx::query!("UPDATE aliases SET enabled = $2 WHERE id = $1", id, enabled)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(not_found("alias"));
        }
        Ok(())
    }

    async fn delete(&self, id: Uuid) -> Result<()> {
        let result = sqlx::query!("DELETE FROM aliases WHERE id = $1", id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(not_found("alias"));
        }
        Ok(())
    }
}
