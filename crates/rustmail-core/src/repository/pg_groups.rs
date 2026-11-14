//! PostgreSQL implementation of [`GroupRepository`].

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use super::traits::GroupRepository;
use crate::domain::validation::normalize_email;
use crate::domain::{Group, NewGroup};
use crate::error::{not_found, Error, Result};

/// PostgreSQL-backed [`GroupRepository`].
#[derive(Debug, Clone)]
pub struct PgGroupRepository {
    pool: PgPool,
}

impl PgGroupRepository {
    /// Creates a repository using `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl GroupRepository for PgGroupRepository {
    async fn create(&self, new: &NewGroup) -> Result<Group> {
        let address = new.address.as_deref().map(normalize_email).transpose()?;
        // A distribution-list address must live in the group's own domain.
        sqlx::query_as!(
            Group,
            r#"INSERT INTO groups (domain_id, name, address, description)
               SELECT d.id, $2::text, $3::text, $4::text
               FROM domains d
               WHERE d.id = $1
                 AND ($3::text IS NULL OR split_part($3::text, '@', 2) = d.name)
               RETURNING id, domain_id, name, address, description, created_at, updated_at"#,
            new.domain_id,
            new.name,
            address,
            new.description,
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| {
            Error::Validation("domain not found or group address not in that domain".into())
        })
    }

    async fn get(&self, id: Uuid) -> Result<Group> {
        sqlx::query_as!(
            Group,
            r#"SELECT id, domain_id, name, address, description, created_at, updated_at
               FROM groups WHERE id = $1"#,
            id,
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| not_found("group"))
    }

    async fn list_by_domain(&self, domain_id: Uuid) -> Result<Vec<Group>> {
        let groups = sqlx::query_as!(
            Group,
            r#"SELECT id, domain_id, name, address, description, created_at, updated_at
               FROM groups WHERE domain_id = $1 ORDER BY name"#,
            domain_id,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(groups)
    }

    async fn add_member(&self, group_id: Uuid, user_id: Uuid) -> Result<()> {
        sqlx::query!(
            "INSERT INTO group_members (group_id, user_id) VALUES ($1, $2)
             ON CONFLICT (group_id, user_id) DO NOTHING",
            group_id,
            user_id,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn remove_member(&self, group_id: Uuid, user_id: Uuid) -> Result<()> {
        sqlx::query!(
            "DELETE FROM group_members WHERE group_id = $1 AND user_id = $2",
            group_id,
            user_id,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_member_ids(&self, group_id: Uuid) -> Result<Vec<Uuid>> {
        let ids = sqlx::query_scalar!(
            "SELECT user_id FROM group_members WHERE group_id = $1 ORDER BY added_at, user_id",
            group_id,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(ids)
    }

    async fn delete(&self, id: Uuid) -> Result<()> {
        let result = sqlx::query!("DELETE FROM groups WHERE id = $1", id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(not_found("group"));
        }
        Ok(())
    }
}
