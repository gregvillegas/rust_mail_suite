//! Domain administration.

use chrono::{DateTime, Utc};
use serde_json::json;
use uuid::Uuid;

use super::{Admin, AdminService};
use crate::domain::{AccountStatus, AuditOutcome, MailDomain, NewDomain};
use crate::error::{Error, Result};
use crate::repository::{DomainRepository, PgDomainRepository};

/// A domain with what is in it.
#[derive(Debug, Clone, PartialEq)]
pub struct DomainRow {
    /// Id.
    pub id: Uuid,
    /// Name, lower-case.
    pub name: String,
    /// Active, suspended or disabled.
    pub status: AccountStatus,
    /// Default mailbox quota in bytes; `None` = unlimited.
    pub default_quota_bytes: Option<i64>,
    /// Most mailboxes allowed; `None` = unlimited.
    pub max_mailboxes: Option<i32>,
    /// Users (and so mailboxes).
    pub users: i64,
    /// Aliases.
    pub aliases: i64,
    /// Groups.
    pub groups: i64,
    /// Bytes used by all mailboxes.
    pub bytes_used: i64,
    /// When it was created.
    pub created_at: DateTime<Utc>,
}

/// The editable settings of a domain.
#[derive(Debug, Clone, PartialEq)]
pub struct DomainUpdate {
    /// New status.
    pub status: AccountStatus,
    /// Default quota in bytes; `None` = unlimited.
    pub default_quota_bytes: Option<i64>,
    /// Mailbox limit; `None` = unlimited.
    pub max_mailboxes: Option<i32>,
}

fn validate_limits(quota: Option<i64>, max: Option<i32>) -> Result<()> {
    if quota.is_some_and(|q| q < 0) {
        return Err(Error::Validation("the quota must not be negative".into()));
    }
    if max.is_some_and(|m| m < 0) {
        return Err(Error::Validation(
            "the mailbox limit must not be negative".into(),
        ));
    }
    Ok(())
}

impl AdminService {
    /// The domains the administrator may see, by name.
    ///
    /// # Errors
    /// Database errors.
    pub async fn list_domains(&self, admin: &Admin) -> Result<Vec<DomainRow>> {
        let rows = sqlx::query!(
            r#"SELECT d.id, d.name, d.status::text AS "status!", d.default_quota_bytes, d.max_mailboxes, d.created_at,
                      (SELECT count(*) FROM users u WHERE u.domain_id = d.id) AS "users!",
                      (SELECT count(*) FROM aliases a WHERE a.domain_id = d.id) AS "aliases!",
                      (SELECT count(*) FROM groups g WHERE g.domain_id = d.id) AS "groups!",
                      (SELECT COALESCE(sum(q.bytes_used), 0)::bigint
                         FROM quota_usage q JOIN mailboxes m ON m.id = q.mailbox_id
                         JOIN users u ON u.id = m.user_id WHERE u.domain_id = d.id) AS "bytes_used!"
               FROM domains d
               WHERE ($1::uuid IS NULL OR d.id = $1)
               ORDER BY d.name"#,
            admin.scope.domain_filter()
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| DomainRow {
                id: r.id,
                name: r.name,
                status: AccountStatus::parse(&r.status).unwrap_or(AccountStatus::Disabled),
                default_quota_bytes: r.default_quota_bytes,
                max_mailboxes: r.max_mailboxes,
                users: r.users,
                aliases: r.aliases,
                groups: r.groups,
                bytes_used: r.bytes_used,
                created_at: r.created_at,
            })
            .collect())
    }

    /// One domain.
    ///
    /// # Errors
    /// [`Error::Forbidden`] outside the scope, [`Error::NotFound`] if absent.
    pub async fn get_domain(&self, admin: &Admin, id: Uuid) -> Result<DomainRow> {
        admin.require_domain(id)?;
        self.list_domains(admin)
            .await?
            .into_iter()
            .find(|d| d.id == id)
            .ok_or(Error::NotFound { entity: "domain" })
    }

    /// Creates a domain and its mail directory. Super administrators only.
    ///
    /// # Errors
    /// [`Error::Forbidden`], [`Error::Validation`], [`Error::Conflict`] if it exists.
    pub async fn create_domain(
        &self,
        admin: &Admin,
        name: &str,
        default_quota_bytes: Option<i64>,
        max_mailboxes: Option<i32>,
    ) -> Result<MailDomain> {
        admin.require_super()?;
        validate_limits(default_quota_bytes, max_mailboxes)?;
        let result = self
            .accounts()?
            .create_domain(&NewDomain {
                name: name.trim().to_owned(),
                default_quota_bytes,
                max_mailboxes,
            })
            .await;
        match &result {
            Ok(domain) => {
                self.audit(
                    admin,
                    "domain.create",
                    Some(("domain", domain.id.to_string())),
                    Some(domain.id),
                    AuditOutcome::Success,
                    json!({"name": domain.name, "default_quota_bytes": default_quota_bytes, "max_mailboxes": max_mailboxes}),
                )
                .await;
            }
            Err(error) => {
                self.audit(
                    admin,
                    "domain.create",
                    None,
                    None,
                    AuditOutcome::Failure,
                    json!({"name": name, "error": error.to_string()}),
                )
                .await;
            }
        }
        result
    }

    /// Changes a domain's status, default quota and mailbox limit. Super administrators only.
    ///
    /// # Errors
    /// [`Error::Forbidden`], [`Error::Validation`], [`Error::NotFound`].
    pub async fn update_domain(
        &self,
        admin: &Admin,
        id: Uuid,
        update: &DomainUpdate,
    ) -> Result<()> {
        admin.require_super()?;
        validate_limits(update.default_quota_bytes, update.max_mailboxes)?;
        let before = self.get_domain(admin, id).await?;
        sqlx::query!(
            "UPDATE domains SET status = $2::text::account_status, default_quota_bytes = $3, max_mailboxes = $4 WHERE id = $1",
            id,
            update.status.as_str(),
            update.default_quota_bytes,
            update.max_mailboxes
        )
        .execute(self.pool())
        .await?;
        self.audit(
            admin,
            "domain.update",
            Some(("domain", id.to_string())),
            Some(id),
            AuditOutcome::Success,
            json!({
                "name": before.name,
                "status": [before.status.as_str(), update.status.as_str()],
                "default_quota_bytes": [before.default_quota_bytes, update.default_quota_bytes],
                "max_mailboxes": [before.max_mailboxes, update.max_mailboxes],
            }),
        )
        .await;
        Ok(())
    }

    /// Deletes an empty domain (and its aliases and groups). The mail directory on
    /// disk is left alone. Super administrators only; `confirm_name` must repeat
    /// the domain's name.
    ///
    /// # Errors
    /// [`Error::Forbidden`], [`Error::Validation`] if the confirmation is wrong,
    /// [`Error::Conflict`] while the domain still has users.
    pub async fn delete_domain(&self, admin: &Admin, id: Uuid, confirm_name: &str) -> Result<()> {
        admin.require_super()?;
        let domain = self.get_domain(admin, id).await?;
        if confirm_name.trim().to_lowercase() != domain.name {
            return Err(Error::Validation(
                "type the domain's name to confirm".into(),
            ));
        }
        if domain.users > 0 {
            return Err(Error::Conflict(format!(
                "{} still has {} user(s); remove or move them first",
                domain.name, domain.users
            )));
        }
        PgDomainRepository::new(self.pool().clone())
            .delete(id)
            .await?;
        // Its spam policy is meaningless now.
        sqlx::query!(
            "DELETE FROM filter_policies WHERE scope = $1",
            format!("@{}", domain.name)
        )
        .execute(self.pool())
        .await?;
        self.audit(
            admin,
            "domain.delete",
            Some(("domain", id.to_string())),
            None,
            AuditOutcome::Success,
            json!({"name": domain.name, "aliases_removed": domain.aliases, "groups_removed": domain.groups}),
        )
        .await;
        Ok(())
    }
}
