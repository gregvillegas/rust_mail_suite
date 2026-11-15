//! App passwords: what calendar and contact apps (Thunderbird, phones) log in with.
//!
//! A user's own password never goes into those apps. Each app gets its own random token,
//! shown once when it is made, that the user can revoke at any time without touching their
//! login. Only a SHA-256 of the token is stored; the token has 128 random bits, so a fast
//! hash is enough and lookups can use an index.

use chrono::{DateTime, Utc};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use super::{DavError, DavResult};
use crate::domain::{AuditOutcome, NewAuditEntry};
use crate::repository::{AuditLogRepository, PgAuditLogRepository};

/// Most app passwords a user may have at once.
pub const MAX_ACTIVE: i64 = 20;

/// An app password as listed to its owner (never the token itself).
#[derive(Debug, Clone)]
pub struct AppPassword {
    /// Identifier, used to revoke it.
    pub id: Uuid,
    /// What the user called it ("Thunderbird laptop").
    pub name: String,
    /// When it was made.
    pub created_at: DateTime<Utc>,
    /// When it was last used to sign in (to the minute), if ever.
    pub last_used_at: Option<DateTime<Utc>>,
}

/// Who an app password belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// The user's id.
    pub user_id: Uuid,
    /// The user's address.
    pub email: String,
}

/// Makes, lists, revokes and checks app passwords.
#[derive(Clone)]
pub struct AppPasswords {
    pool: PgPool,
}

fn hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl AppPasswords {
    /// Creates the service.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn audit(&self, user_id: Uuid, email: &str, action: &str, id: Uuid, name: &str) {
        let entry = NewAuditEntry {
            actor_user_id: Some(user_id),
            actor_email: Some(email.to_owned()),
            action: action.to_owned(),
            target_type: Some("app_password".into()),
            target_id: Some(id.to_string()),
            outcome: AuditOutcome::Success,
            ip_address: None,
            user_agent: None,
            details: json!({ "name": name }),
        };
        if let Err(error) = PgAuditLogRepository::new(self.pool.clone())
            .append(&entry)
            .await
        {
            tracing::warn!(%error, action, "could not write an audit entry");
        }
    }

    /// Makes an app password and returns its token. **This is the only time the token exists
    /// in readable form.**
    ///
    /// # Errors
    /// [`DavError::Invalid`] for a bad name, [`DavError::Conflict`] at the limit.
    pub async fn create(&self, user_id: Uuid, name: &str) -> DavResult<String> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
            return Err(DavError::Invalid(
                "the name must be 1 to 64 characters".into(),
            ));
        }
        let email = sqlx::query_scalar!("SELECT email FROM users WHERE id = $1", user_id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(DavError::NotFound)?;
        let token = crate::password::generate_password();
        let mut tx = self.pool.begin().await?;
        // Serialise concurrent creations for one user so the limit holds.
        sqlx::query!("SELECT id FROM users WHERE id = $1 FOR UPDATE", user_id)
            .fetch_one(&mut *tx)
            .await?;
        let active = sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM app_passwords WHERE user_id = $1 AND revoked_at IS NULL"#, user_id).fetch_one(&mut *tx).await?;
        if active >= MAX_ACTIVE {
            return Err(DavError::Conflict(format!(
                "you can have at most {MAX_ACTIVE} app passwords; revoke one first"
            )));
        }
        let id = sqlx::query_scalar!("INSERT INTO app_passwords (user_id, name, token_hash) VALUES ($1, $2, $3) RETURNING id", user_id, name, hash(&token)).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        self.audit(user_id, &email, "app_password.create", id, name)
            .await;
        Ok(token)
    }

    /// The user's active app passwords, newest first.
    ///
    /// # Errors
    /// Database errors.
    pub async fn list(&self, user_id: Uuid) -> DavResult<Vec<AppPassword>> {
        Ok(sqlx::query!(
            "SELECT id, name, created_at, last_used_at FROM app_passwords WHERE user_id = $1 AND revoked_at IS NULL ORDER BY created_at DESC",
            user_id
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|r| AppPassword { id: r.id, name: r.name, created_at: r.created_at, last_used_at: r.last_used_at })
        .collect())
    }

    /// Revokes one of the user's app passwords. Revoking someone else's id does nothing.
    ///
    /// # Errors
    /// [`DavError::NotFound`] if the user has no active app password with that id.
    pub async fn revoke(&self, user_id: Uuid, id: Uuid) -> DavResult<()> {
        let row = sqlx::query!(
            "UPDATE app_passwords SET revoked_at = now() WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL RETURNING name",
            id,
            user_id
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or(DavError::NotFound)?;
        let email = sqlx::query_scalar!("SELECT email FROM users WHERE id = $1", user_id)
            .fetch_one(&self.pool)
            .await?;
        self.audit(user_id, &email, "app_password.revoke", id, &row.name)
            .await;
        Ok(())
    }

    /// Checks `email` and `token`. Returns the user only if the token belongs to them, has not
    /// been revoked, and both the account and its domain are active.
    ///
    /// # Errors
    /// Database errors. A wrong token is `Ok(None)`.
    pub async fn verify(&self, email: &str, token: &str) -> DavResult<Option<Principal>> {
        if token.is_empty() || token.len() > 128 {
            return Ok(None);
        }
        let row = sqlx::query!(
            r#"SELECT a.id, u.id AS user_id, u.email, a.last_used_at
               FROM app_passwords a
               JOIN users u ON u.id = a.user_id
               JOIN domains d ON d.id = u.domain_id
               WHERE a.token_hash = $1 AND a.revoked_at IS NULL
                 AND lower(u.email) = lower($2)
                 AND u.status = 'active' AND d.status = 'active'"#,
            hash(token),
            email
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some(r) = row else { return Ok(None) };
        if r.last_used_at
            .is_none_or(|t| Utc::now() - t > chrono::Duration::minutes(1))
        {
            sqlx::query!(
                "UPDATE app_passwords SET last_used_at = now() WHERE id = $1",
                r.id
            )
            .execute(&self.pool)
            .await?;
        }
        Ok(Some(Principal {
            user_id: r.user_id,
            email: r.email,
        }))
    }
}
