//! PostgreSQL implementation of [`UserRepository`].

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use super::traits::UserRepository;
use crate::domain::validation::{normalize_email, normalize_local_part};
use crate::domain::{AccountStatus, LoginFailure, NewUser, User, UserRole};
use crate::error::{not_found, Error, Result};

/// PostgreSQL-backed [`UserRepository`].
#[derive(Debug, Clone)]
pub struct PgUserRepository {
    pool: PgPool,
}

impl PgUserRepository {
    /// Creates a repository using `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl UserRepository for PgUserRepository {
    async fn create(&self, new: &NewUser) -> Result<User> {
        let local_part = normalize_local_part(&new.local_part)?;
        if new.password_hash.is_empty() {
            return Err(Error::Validation("password hash must not be empty".into()));
        }
        // The address is built in SQL from the stored domain name, so it can
        // never disagree with `domain_id`.
        sqlx::query_as!(
            User,
            r#"INSERT INTO users (domain_id, local_part, email, password_hash,
                                  display_name, role, locale)
               SELECT d.id, $2::text, $2::text || '@' || d.name, $3::text,
                      $4::text, $5::user_role, $6::text
               FROM domains d WHERE d.id = $1
               RETURNING id, domain_id, local_part, email, password_hash, display_name,
                         role AS "role: UserRole", status AS "status: AccountStatus",
                         locale, last_login_at, failed_login_count, locked_until,
                         created_at, updated_at"#,
            new.domain_id,
            local_part,
            new.password_hash,
            new.display_name,
            new.role as UserRole,
            new.locale,
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| not_found("domain"))
    }

    async fn get(&self, id: Uuid) -> Result<User> {
        sqlx::query_as!(
            User,
            r#"SELECT id, domain_id, local_part, email, password_hash, display_name,
                      role AS "role: UserRole", status AS "status: AccountStatus",
                      locale, last_login_at, failed_login_count, locked_until,
                      created_at, updated_at
               FROM users WHERE id = $1"#,
            id,
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| not_found("user"))
    }

    async fn find_by_email(&self, email: &str) -> Result<Option<User>> {
        // Malformed input simply cannot match an account.
        let Ok(email) = normalize_email(email) else {
            return Ok(None);
        };
        let user = sqlx::query_as!(
            User,
            r#"SELECT id, domain_id, local_part, email, password_hash, display_name,
                      role AS "role: UserRole", status AS "status: AccountStatus",
                      locale, last_login_at, failed_login_count, locked_until,
                      created_at, updated_at
               FROM users WHERE email = $1"#,
            email,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(user)
    }

    async fn list_by_domain(&self, domain_id: Uuid) -> Result<Vec<User>> {
        let users = sqlx::query_as!(
            User,
            r#"SELECT id, domain_id, local_part, email, password_hash, display_name,
                      role AS "role: UserRole", status AS "status: AccountStatus",
                      locale, last_login_at, failed_login_count, locked_until,
                      created_at, updated_at
               FROM users WHERE domain_id = $1 ORDER BY email"#,
            domain_id,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(users)
    }

    async fn count_by_role(&self, role: UserRole) -> Result<i64> {
        let row = sqlx::query!(
            r#"SELECT COUNT(*) AS "count!" FROM users WHERE role = $1"#,
            role as UserRole,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(row.count)
    }

    async fn set_role(&self, id: Uuid, role: UserRole) -> Result<()> {
        let result = sqlx::query!(
            "UPDATE users SET role = $2 WHERE id = $1",
            id,
            role as UserRole
        )
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(not_found("user"));
        }
        Ok(())
    }

    async fn set_status(&self, id: Uuid, status: AccountStatus) -> Result<()> {
        let result = sqlx::query!(
            "UPDATE users SET status = $2 WHERE id = $1",
            id,
            status as AccountStatus
        )
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(not_found("user"));
        }
        Ok(())
    }

    async fn set_password_hash(&self, id: Uuid, password_hash: &str) -> Result<()> {
        if password_hash.is_empty() {
            return Err(Error::Validation("password hash must not be empty".into()));
        }
        let result = sqlx::query!(
            "UPDATE users
             SET password_hash = $2, failed_login_count = 0, locked_until = NULL
             WHERE id = $1",
            id,
            password_hash,
        )
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(not_found("user"));
        }
        Ok(())
    }

    async fn record_login_success(&self, id: Uuid) -> Result<()> {
        let result = sqlx::query!(
            "UPDATE users
             SET last_login_at = now(), failed_login_count = 0, locked_until = NULL
             WHERE id = $1",
            id,
        )
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(not_found("user"));
        }
        Ok(())
    }

    async fn record_login_failure(
        &self,
        id: Uuid,
        max_failures: i32,
        lock_for_secs: i64,
    ) -> Result<LoginFailure> {
        if max_failures < 1 || lock_for_secs < 1 {
            return Err(Error::Validation(
                "lockout settings must be positive".into(),
            ));
        }
        // One statement, so concurrent failures cannot lose increments. All
        // right-hand sides see the pre-update row, hence `failed_login_count + 1`.
        sqlx::query_as!(
            LoginFailure,
            "UPDATE users
             SET failed_login_count = CASE WHEN failed_login_count + 1 >= $2
                                           THEN 0 ELSE failed_login_count + 1 END,
                 locked_until = CASE WHEN failed_login_count + 1 >= $2
                                     THEN now() + ($3::bigint * interval '1 second')
                                     ELSE locked_until END
             WHERE id = $1
             RETURNING failed_login_count, locked_until",
            id,
            max_failures,
            lock_for_secs,
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| not_found("user"))
    }

    async fn delete(&self, id: Uuid) -> Result<()> {
        let result = sqlx::query!("DELETE FROM users WHERE id = $1", id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(not_found("user"));
        }
        Ok(())
    }
}
