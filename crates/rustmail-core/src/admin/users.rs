//! User administration.

use chrono::{DateTime, Utc};
use serde_json::json;
use uuid::Uuid;

use super::{like_contains, Admin, AdminService};
use crate::domain::validation::split_email;
use crate::domain::{AccountStatus, AuditOutcome, User, UserRole};
use crate::error::{Error, Result};
use crate::password::{check_password_length, generate_password, hash_password};
use crate::provisioning::CreateUser;
use crate::repository::{
    DomainRepository, MailboxRepository, PgDomainRepository, PgMailboxRepository,
    PgSessionRepository, PgUserRepository, SessionRepository, UserRepository,
};

/// Largest page of users.
pub const MAX_PAGE_SIZE: i64 = 200;
/// Longest display name.
pub const MAX_NAME_CHARS: usize = 128;

/// One line in the user list.
#[derive(Debug, Clone, PartialEq)]
pub struct UserRow {
    /// Id.
    pub id: Uuid,
    /// Domain id.
    pub domain_id: Uuid,
    /// Domain name.
    pub domain: String,
    /// Address.
    pub email: String,
    /// Display name.
    pub display_name: String,
    /// Role.
    pub role: UserRole,
    /// Status.
    pub status: AccountStatus,
    /// Last successful sign-in.
    pub last_login_at: Option<DateTime<Utc>>,
    /// When the account was created.
    pub created_at: DateTime<Utc>,
    /// The quota that applies (mailbox override, else domain default); `None` = unlimited.
    pub quota_bytes: Option<i64>,
    /// Bytes used.
    pub bytes_used: i64,
    /// Messages stored.
    pub messages: i64,
}

/// Which users to list.
#[derive(Debug, Clone, Default)]
pub struct UserFilter {
    /// Only this domain (ignored for domain administrators, who always see their own).
    pub domain_id: Option<Uuid>,
    /// Text found in the address or name.
    pub search: Option<String>,
    /// Only this status.
    pub status: Option<AccountStatus>,
    /// Only this role.
    pub role: Option<UserRole>,
    /// Rows per page.
    pub limit: i64,
    /// Rows to skip.
    pub offset: i64,
}

/// A page of users.
#[derive(Debug, Clone, PartialEq)]
pub struct UserPage {
    /// The rows.
    pub rows: Vec<UserRow>,
    /// Matches in total.
    pub total: i64,
}

/// Everything shown on a user's page.
#[derive(Debug, Clone, PartialEq)]
pub struct UserDetail {
    /// The list row.
    pub row: UserRow,
    /// The mailbox's own quota override (`None` = inherits the domain default).
    pub quota_override: Option<i64>,
    /// Failed sign-ins since the last success.
    pub failed_logins: i32,
    /// Locked out until, if currently locked.
    pub locked_until: Option<DateTime<Utc>>,
    /// Sessions that are still valid.
    pub active_sessions: i64,
    /// Groups the user belongs to: id and name.
    pub groups: Vec<(Uuid, String)>,
    /// Aliases that deliver to this user.
    pub aliases: Vec<String>,
}

/// A user to create.
#[derive(Clone)]
pub struct NewAccount {
    /// Full address.
    pub email: String,
    /// Display name.
    pub display_name: String,
    /// Role.
    pub role: UserRole,
    /// Quota in bytes; `None` inherits the domain default.
    pub quota_bytes: Option<i64>,
    /// A chosen password; `None` generates one.
    pub password: Option<String>,
}

impl std::fmt::Debug for NewAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewAccount")
            .field("email", &self.email)
            .field("password", &"<redacted>")
            .finish_non_exhaustive()
    }
}

/// A created user.
#[derive(Clone)]
pub struct CreatedAccount {
    /// The user.
    pub user: User,
    /// The generated password, if none was chosen. Shown once, never stored in clear.
    pub generated_password: Option<String>,
}

impl std::fmt::Debug for CreatedAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreatedAccount")
            .field("user", &self.user.email)
            .finish_non_exhaustive()
    }
}

/// The editable parts of a user.
#[derive(Debug, Clone)]
pub struct UserUpdate {
    /// Display name.
    pub display_name: String,
    /// Role (only a super administrator may change it).
    pub role: UserRole,
    /// Quota override in bytes; `None` inherits the domain default.
    pub quota_bytes: Option<i64>,
}

/// Cleans a display name: trims, rejects control characters and excess length.
///
/// # Errors
/// [`Error::Validation`].
pub fn clean_display_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(Error::Validation(format!(
            "the name may have at most {MAX_NAME_CHARS} characters"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(Error::Validation(
            "the name contains a control character".into(),
        ));
    }
    Ok(name.to_owned())
}

fn row_of(r: &UserRowRaw) -> UserRow {
    UserRow {
        id: r.id,
        domain_id: r.domain_id,
        domain: r.domain.clone(),
        email: r.email.clone(),
        display_name: r.display_name.clone(),
        role: UserRole::parse(&r.role).unwrap_or(UserRole::User),
        status: AccountStatus::parse(&r.status).unwrap_or(AccountStatus::Disabled),
        last_login_at: r.last_login_at,
        created_at: r.created_at,
        quota_bytes: r.quota_bytes,
        bytes_used: r.bytes_used,
        messages: r.messages,
    }
}

struct UserRowRaw {
    id: Uuid,
    domain_id: Uuid,
    domain: String,
    email: String,
    display_name: String,
    role: String,
    status: String,
    last_login_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    quota_bytes: Option<i64>,
    bytes_used: i64,
    messages: i64,
}

impl AdminService {
    /// Lists users the administrator may see.
    ///
    /// # Errors
    /// [`Error::Forbidden`] if the filter names a domain outside the scope.
    pub async fn list_users(&self, admin: &Admin, filter: &UserFilter) -> Result<UserPage> {
        let domain = match admin.scope.domain_filter() {
            Some(own) => {
                if filter.domain_id.is_some_and(|d| d != own) {
                    return Err(Error::Forbidden("that belongs to another domain".into()));
                }
                Some(own)
            }
            None => filter.domain_id,
        };
        let search = filter.search.as_deref().and_then(like_contains);
        let status = filter.status.map(AccountStatus::as_str);
        let role = filter.role.map(UserRole::as_str);
        let limit = filter.limit.clamp(1, MAX_PAGE_SIZE);
        let offset = filter.offset.max(0);
        let total = sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!" FROM users u
               WHERE ($1::uuid IS NULL OR u.domain_id = $1)
                 AND ($2::text IS NULL OR u.email ILIKE $2 OR u.display_name ILIKE $2)
                 AND ($3::text IS NULL OR u.status::text = $3)
                 AND ($4::text IS NULL OR u.role::text = $4)"#,
            domain,
            search,
            status,
            role
        )
        .fetch_one(self.pool())
        .await?;
        let rows = sqlx::query_as!(
            UserRowRaw,
            r#"SELECT u.id, u.domain_id, d.name AS domain, u.email, u.display_name,
                      u.role::text AS "role!", u.status::text AS "status!", u.last_login_at, u.created_at,
                      COALESCE(m.quota_bytes, d.default_quota_bytes) AS quota_bytes,
                      COALESCE(q.bytes_used, 0)::bigint AS "bytes_used!",
                      COALESCE(q.messages_count, 0)::bigint AS "messages!"
               FROM users u
               JOIN domains d ON d.id = u.domain_id
               LEFT JOIN mailboxes m ON m.user_id = u.id
               LEFT JOIN quota_usage q ON q.mailbox_id = m.id
               WHERE ($1::uuid IS NULL OR u.domain_id = $1)
                 AND ($2::text IS NULL OR u.email ILIKE $2 OR u.display_name ILIKE $2)
                 AND ($3::text IS NULL OR u.status::text = $3)
                 AND ($4::text IS NULL OR u.role::text = $4)
               ORDER BY u.email
               LIMIT $5 OFFSET $6"#,
            domain,
            search,
            status,
            role,
            limit,
            offset
        )
        .fetch_all(self.pool())
        .await?;
        Ok(UserPage {
            rows: rows.iter().map(row_of).collect(),
            total,
        })
    }

    /// Loads a user the administrator may look at.
    async fn user_in_scope(&self, admin: &Admin, id: Uuid) -> Result<User> {
        let user = PgUserRepository::new(self.pool().clone()).get(id).await?;
        admin.require_domain(user.domain_id)?;
        Ok(user)
    }

    /// Loads a user the administrator may *change*: in scope, and not another
    /// administrator unless the actor is a super administrator.
    async fn user_to_change(&self, admin: &Admin, id: Uuid) -> Result<User> {
        let user = self.user_in_scope(admin, id).await?;
        if !admin.is_super() && user.role != UserRole::User {
            return Err(Error::Forbidden(
                "only a super administrator may change an administrator".into(),
            ));
        }
        Ok(user)
    }

    /// Refuses to remove the last working super administrator.
    async fn keep_one_super_admin(&self, target: &User) -> Result<()> {
        if target.role != UserRole::SuperAdmin || !target.status.is_active() {
            return Ok(());
        }
        let others = sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!" FROM users WHERE role = 'super_admin' AND status = 'active' AND id <> $1"#,
            target.id
        )
        .fetch_one(self.pool())
        .await?;
        if others == 0 {
            return Err(Error::Conflict(
                "this is the last active super administrator".into(),
            ));
        }
        Ok(())
    }

    /// Everything about one user.
    ///
    /// # Errors
    /// [`Error::Forbidden`] outside the scope, [`Error::NotFound`].
    pub async fn get_user(&self, admin: &Admin, id: Uuid) -> Result<UserDetail> {
        let user = self.user_in_scope(admin, id).await?;
        let raw = sqlx::query_as!(
            UserRowRaw,
            r#"SELECT u.id, u.domain_id, d.name AS domain, u.email, u.display_name,
                      u.role::text AS "role!", u.status::text AS "status!", u.last_login_at, u.created_at,
                      COALESCE(m.quota_bytes, d.default_quota_bytes) AS quota_bytes,
                      COALESCE(q.bytes_used, 0)::bigint AS "bytes_used!",
                      COALESCE(q.messages_count, 0)::bigint AS "messages!"
               FROM users u JOIN domains d ON d.id = u.domain_id
               LEFT JOIN mailboxes m ON m.user_id = u.id
               LEFT JOIN quota_usage q ON q.mailbox_id = m.id
               WHERE u.id = $1"#,
            id
        )
        .fetch_one(self.pool())
        .await?;
        let quota_override = PgMailboxRepository::new(self.pool().clone())
            .find_by_user(id)
            .await?
            .and_then(|m| m.quota_bytes);
        let active_sessions = sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!" FROM sessions WHERE user_id = $1 AND revoked_at IS NULL AND expires_at > now()"#,
            id
        )
        .fetch_one(self.pool())
        .await?;
        let groups = sqlx::query!(
            "SELECT g.id, g.name FROM groups g JOIN group_members gm ON gm.group_id = g.id WHERE gm.user_id = $1 ORDER BY g.name",
            id
        )
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(|r| (r.id, r.name))
        .collect();
        let aliases = sqlx::query_scalar!(
            "SELECT address FROM aliases WHERE destination = $1 ORDER BY address",
            user.email
        )
        .fetch_all(self.pool())
        .await?;
        Ok(UserDetail {
            row: row_of(&raw),
            quota_override,
            failed_logins: user.failed_login_count,
            locked_until: user.locked_until.filter(|t| *t > Utc::now()),
            active_sessions,
            groups,
            aliases,
        })
    }

    /// Creates a user with a mailbox and Maildir.
    ///
    /// # Errors
    /// [`Error::Forbidden`] for another domain or (domain administrators) any role
    /// but `User`; [`Error::Conflict`] if the address is taken; validation errors.
    pub async fn create_user(&self, admin: &Admin, request: NewAccount) -> Result<CreatedAccount> {
        let result = self.create_user_inner(admin, &request).await;
        match &result {
            Ok(created) => {
                self.audit(
                    admin,
                    "user.create",
                    Some(("user", created.user.id.to_string())),
                    Some(created.user.domain_id),
                    AuditOutcome::Success,
                    json!({"email": created.user.email, "role": created.user.role.as_str(), "password_generated": created.generated_password.is_some()}),
                )
                .await;
            }
            Err(error) => {
                self.audit(
                    admin,
                    "user.create",
                    None,
                    None,
                    AuditOutcome::Failure,
                    json!({"email": request.email, "error": error.to_string()}),
                )
                .await;
            }
        }
        result
    }

    async fn create_user_inner(
        &self,
        admin: &Admin,
        request: &NewAccount,
    ) -> Result<CreatedAccount> {
        let (local, domain_name) = split_email(&request.email)?;
        let domain = PgDomainRepository::new(self.pool().clone())
            .find_by_name(&domain_name)
            .await?
            .ok_or_else(|| {
                Error::Validation(format!("the domain '{domain_name}' does not exist"))
            })?;
        admin.require_domain(domain.id)?;
        if !admin.is_super() && request.role != UserRole::User {
            return Err(Error::Forbidden(
                "only a super administrator may create administrators".into(),
            ));
        }
        let email = format!("{local}@{domain_name}");
        if self.address_taken(&email).await? {
            return Err(Error::Conflict(format!(
                "{email} is already in use (as a mailbox, alias or group)"
            )));
        }
        let name = clean_display_name(&request.display_name)?;
        let (password, generated) = match &request.password {
            Some(p) => {
                check_password_length(p)?;
                (p.clone(), None)
            }
            None => {
                let p = generate_password();
                (p.clone(), Some(p))
            }
        };
        let created = self
            .accounts()?
            .create_user(CreateUser {
                email,
                password,
                display_name: name,
                role: request.role,
                quota_bytes: request.quota_bytes,
            })
            .await?;
        Ok(CreatedAccount {
            user: created.user,
            generated_password: generated,
        })
    }

    /// Changes a user's name, role and quota.
    ///
    /// # Errors
    /// [`Error::Forbidden`], [`Error::Validation`], [`Error::Conflict`] when it
    /// would remove the last super administrator.
    pub async fn update_user(&self, admin: &Admin, id: Uuid, update: &UserUpdate) -> Result<()> {
        let user = self.user_to_change(admin, id).await?;
        let name = clean_display_name(&update.display_name)?;
        if update.quota_bytes.is_some_and(|q| q < 0) {
            return Err(Error::Validation("the quota must not be negative".into()));
        }
        let role_changes = update.role != user.role;
        if role_changes {
            admin.require_super()?;
            if user.id == admin.user_id {
                return Err(Error::Forbidden("you cannot change your own role".into()));
            }
            if update.role != UserRole::SuperAdmin {
                self.keep_one_super_admin(&user).await?;
            }
        }
        sqlx::query!(
            "UPDATE users SET display_name = $2, role = $3::text::user_role WHERE id = $1",
            id,
            name,
            update.role.as_str()
        )
        .execute(self.pool())
        .await?;
        let mailboxes = PgMailboxRepository::new(self.pool().clone());
        if let Some(mailbox) = mailboxes.find_by_user(id).await? {
            mailboxes.set_quota(mailbox.id, update.quota_bytes).await?;
        }
        if role_changes {
            PgSessionRepository::new(self.pool().clone())
                .revoke_all_for_user(id)
                .await?;
        }
        self.audit(
            admin,
            "user.update",
            Some(("user", id.to_string())),
            Some(user.domain_id),
            AuditOutcome::Success,
            json!({"email": user.email, "role": [user.role.as_str(), update.role.as_str()], "quota_bytes": update.quota_bytes}),
        )
        .await;
        Ok(())
    }

    /// Enables, suspends or disables a user. Anything but `Active` ends their
    /// sessions and stops mail delivery and sign-in.
    ///
    /// # Errors
    /// [`Error::Forbidden`] (including for yourself), [`Error::Conflict`] for the
    /// last super administrator.
    pub async fn set_user_status(
        &self,
        admin: &Admin,
        id: Uuid,
        status: AccountStatus,
    ) -> Result<()> {
        let user = self.user_to_change(admin, id).await?;
        if !status.is_active() {
            if user.id == admin.user_id {
                return Err(Error::Forbidden(
                    "you cannot disable your own account".into(),
                ));
            }
            self.keep_one_super_admin(&user).await?;
        }
        PgUserRepository::new(self.pool().clone())
            .set_status(id, status)
            .await?;
        if !status.is_active() {
            PgSessionRepository::new(self.pool().clone())
                .revoke_all_for_user(id)
                .await?;
        }
        self.audit(
            admin,
            "user.status",
            Some(("user", id.to_string())),
            Some(user.domain_id),
            AuditOutcome::Success,
            json!({"email": user.email, "status": [user.status.as_str(), status.as_str()]}),
        )
        .await;
        Ok(())
    }

    /// Sets a new password (generated if `chosen` is `None`), ends all sessions and
    /// clears any lockout. Returns the password so it can be shown once.
    ///
    /// # Errors
    /// [`Error::Forbidden`], [`Error::Validation`] for a short password.
    pub async fn reset_password(
        &self,
        admin: &Admin,
        id: Uuid,
        chosen: Option<String>,
    ) -> Result<String> {
        let user = self.user_to_change(admin, id).await?;
        let password = match chosen {
            Some(p) => {
                check_password_length(&p)?;
                p
            }
            None => generate_password(),
        };
        let to_hash = password.clone();
        let hash = tokio::task::spawn_blocking(move || hash_password(&to_hash))
            .await
            .map_err(|e| Error::PasswordHash(e.to_string()))??;
        PgUserRepository::new(self.pool().clone())
            .set_password_hash(id, &hash)
            .await?;
        sqlx::query!(
            "UPDATE users SET failed_login_count = 0, locked_until = NULL WHERE id = $1",
            id
        )
        .execute(self.pool())
        .await?;
        PgSessionRepository::new(self.pool().clone())
            .revoke_all_for_user(id)
            .await?;
        self.audit(
            admin,
            "user.password_reset",
            Some(("user", id.to_string())),
            Some(user.domain_id),
            AuditOutcome::Success,
            json!({"email": user.email}),
        )
        .await;
        Ok(password)
    }

    /// Clears a lockout caused by failed sign-ins.
    ///
    /// # Errors
    /// [`Error::Forbidden`].
    pub async fn unlock_user(&self, admin: &Admin, id: Uuid) -> Result<()> {
        let user = self.user_to_change(admin, id).await?;
        sqlx::query!(
            "UPDATE users SET failed_login_count = 0, locked_until = NULL WHERE id = $1",
            id
        )
        .execute(self.pool())
        .await?;
        self.audit(
            admin,
            "user.unlock",
            Some(("user", id.to_string())),
            Some(user.domain_id),
            AuditOutcome::Success,
            json!({"email": user.email}),
        )
        .await;
        Ok(())
    }

    /// Signs the user out everywhere. Returns how many sessions were ended.
    ///
    /// # Errors
    /// [`Error::Forbidden`].
    pub async fn revoke_user_sessions(&self, admin: &Admin, id: Uuid) -> Result<u64> {
        let user = self.user_to_change(admin, id).await?;
        let ended = PgSessionRepository::new(self.pool().clone())
            .revoke_all_for_user(id)
            .await?;
        self.audit(
            admin,
            "user.sessions_revoked",
            Some(("user", id.to_string())),
            Some(user.domain_id),
            AuditOutcome::Success,
            json!({"email": user.email, "sessions": ended}),
        )
        .await;
        Ok(ended)
    }

    /// Deletes a user for good: the account, mailbox row, group memberships,
    /// aliases pointing at them and, on disk, their whole mail directory.
    /// `confirm_email` must repeat the address.
    ///
    /// # Errors
    /// [`Error::Forbidden`] (also for yourself), [`Error::Validation`] if the
    /// confirmation is wrong, [`Error::Conflict`] for the last super administrator.
    pub async fn delete_user(&self, admin: &Admin, id: Uuid, confirm_email: &str) -> Result<()> {
        let user = self.user_to_change(admin, id).await?;
        if confirm_email.trim().to_lowercase() != user.email {
            return Err(Error::Validation(
                "type the user's address to confirm".into(),
            ));
        }
        if user.id == admin.user_id {
            return Err(Error::Forbidden(
                "you cannot delete your own account".into(),
            ));
        }
        self.keep_one_super_admin(&user).await?;
        let mailboxes = PgMailboxRepository::new(self.pool().clone());
        let home = mailboxes.find_by_user(id).await?.map(|m| m.maildir_path);
        let removed_aliases =
            sqlx::query!("DELETE FROM aliases WHERE destination = $1", user.email)
                .execute(self.pool())
                .await?
                .rows_affected();
        PgUserRepository::new(self.pool().clone())
            .delete(id)
            .await?;
        let (maildir_removed, disk_error) = match (&home, self.provisioner()) {
            (Some(home), Some(provisioner)) => {
                let (provisioner, home) = (provisioner.clone(), home.clone());
                match tokio::task::spawn_blocking(move || provisioner.remove_home(&home)).await {
                    Ok(Ok(removed)) => (removed, None),
                    Ok(Err(error)) => (false, Some(error.to_string())),
                    Err(error) => (false, Some(error.to_string())),
                }
            }
            _ => (false, None),
        };
        if let Some(error) = &disk_error {
            tracing::error!(%error, email = %user.email, "the user was deleted but their mail directory could not be removed");
        }
        self.audit(
            admin,
            "user.delete",
            Some(("user", id.to_string())),
            Some(user.domain_id),
            AuditOutcome::Success,
            json!({"email": user.email, "maildir_removed": maildir_removed, "aliases_removed": removed_aliases, "disk_error": disk_error}),
        )
        .await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_names_are_cleaned() {
        assert_eq!(clean_display_name("  Anna Müller ").unwrap(), "Anna Müller");
        assert_eq!(clean_display_name("").unwrap(), "");
        assert!(clean_display_name("a\nb").is_err());
        assert!(clean_display_name(&"x".repeat(MAX_NAME_CHARS + 1)).is_err());
        assert!(clean_display_name(&"x".repeat(MAX_NAME_CHARS)).is_ok());
    }
}
