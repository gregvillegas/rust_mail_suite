//! The administration service (Phase 7): domains, users, aliases, groups, quota,
//! audit log and CSV import, with one rule applied everywhere: **an administrator
//! only ever sees and changes what their [`AdminScope`] covers.**
//!
//! * A *super administrator* has [`AdminScope::All`].
//! * A *domain administrator* has [`AdminScope::Domain`] for their own domain.
//!   They manage ordinary users, aliases and groups there; they cannot create or
//!   edit domains, cannot touch other administrators, and cannot change roles.
//!
//! Every [`AdminService`] method takes the acting [`Admin`] first, checks it before
//! doing anything, and writes an audit entry. The web layer never decides who may
//! do what; it only passes the signed-in user in.

pub mod audit;
pub mod csv;
pub mod domains;
pub mod health;
pub mod import;
pub mod quota;
pub mod routing;
pub mod users;

use std::sync::Arc;

use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::{AuditOutcome, User, UserRole};
use crate::error::{Error, Result};
use crate::provisioning::{AccountService, MailboxProvisioner};
use crate::repository::{
    AuditLogRepository, DomainRepository, PgAuditLogRepository, PgDomainRepository,
    PgMailboxRepository, PgUserRepository,
};

pub use crate::domain::NewAuditEntry;

/// What an administrator may see and change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminScope {
    /// Every domain.
    All,
    /// One domain.
    Domain(Uuid),
}

impl AdminScope {
    /// Whether `domain_id` is inside the scope.
    #[must_use]
    pub fn allows(self, domain_id: Uuid) -> bool {
        match self {
            Self::All => true,
            Self::Domain(own) => own == domain_id,
        }
    }

    /// The single domain, for queries that filter by it; `None` means everything.
    #[must_use]
    pub fn domain_filter(self) -> Option<Uuid> {
        match self {
            Self::All => None,
            Self::Domain(id) => Some(id),
        }
    }
}

/// An administrator acting on the system.
#[derive(Debug, Clone)]
pub struct Admin {
    /// Their user id.
    pub user_id: Uuid,
    /// Their address (kept in audit entries even if the account is deleted later).
    pub email: String,
    /// Their role.
    pub role: UserRole,
    /// What they may touch.
    pub scope: AdminScope,
    /// Where the request came from, if known.
    pub ip_address: Option<String>,
}

impl Admin {
    /// Builds the acting administrator from a signed-in user.
    ///
    /// # Errors
    /// [`Error::Forbidden`] for an ordinary user.
    pub fn from_user(user: &User, ip_address: Option<String>) -> Result<Self> {
        let scope = match user.role {
            UserRole::SuperAdmin => AdminScope::All,
            UserRole::DomainAdmin => AdminScope::Domain(user.domain_id),
            UserRole::User => return Err(Error::Forbidden("administrators only".into())),
        };
        Ok(Self {
            user_id: user.id,
            email: user.email.clone(),
            role: user.role,
            scope,
            ip_address,
        })
    }

    /// Whether this is a super administrator.
    #[must_use]
    pub fn is_super(&self) -> bool {
        self.role == UserRole::SuperAdmin
    }

    /// # Errors
    /// [`Error::Forbidden`] unless this is a super administrator.
    pub fn require_super(&self) -> Result<()> {
        if self.is_super() {
            Ok(())
        } else {
            Err(Error::Forbidden(
                "only a super administrator may do this".into(),
            ))
        }
    }

    /// # Errors
    /// [`Error::Forbidden`] if `domain_id` is outside the scope.
    pub fn require_domain(&self, domain_id: Uuid) -> Result<()> {
        if self.scope.allows(domain_id) {
            Ok(())
        } else {
            Err(Error::Forbidden("that belongs to another domain".into()))
        }
    }
}

/// The administration service.
#[derive(Clone)]
pub struct AdminService {
    pool: PgPool,
    provisioner: Option<Arc<MailboxProvisioner>>,
}

impl std::fmt::Debug for AdminService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminService").finish_non_exhaustive()
    }
}

impl AdminService {
    /// Creates the service. Without a `provisioner` (no `VMAIL_BASE`), everything
    /// that only touches the database works, but creating domains and users does not.
    #[must_use]
    pub fn new(pool: PgPool, provisioner: Option<Arc<MailboxProvisioner>>) -> Self {
        Self { pool, provisioner }
    }

    pub(crate) fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// The account service (creates rows and Maildirs together).
    pub(crate) fn accounts(&self) -> Result<AccountService> {
        let provisioner = self.provisioner.clone().ok_or_else(|| {
            Error::Provisioning(
                "the mail directory is not configured (set VMAIL_BASE for the web server)".into(),
            )
        })?;
        Ok(AccountService::new(
            Arc::new(PgDomainRepository::new(self.pool.clone())),
            Arc::new(PgUserRepository::new(self.pool.clone())),
            Arc::new(PgMailboxRepository::new(self.pool.clone())),
            provisioner,
        ))
    }

    pub(crate) fn provisioner(&self) -> Option<&Arc<MailboxProvisioner>> {
        self.provisioner.as_ref()
    }

    /// Looks a domain up and checks the scope covers it.
    pub(crate) async fn scoped_domain(
        &self,
        admin: &Admin,
        domain_id: Uuid,
    ) -> Result<crate::domain::MailDomain> {
        admin.require_domain(domain_id)?;
        PgDomainRepository::new(self.pool.clone())
            .get(domain_id)
            .await
    }

    /// Writes an audit entry. A failure to write is logged but never turns a
    /// completed action into an error.
    pub(crate) async fn audit(
        &self,
        admin: &Admin,
        action: &str,
        target: Option<(&str, String)>,
        domain_id: Option<Uuid>,
        outcome: AuditOutcome,
        mut details: Value,
    ) {
        if let (Some(id), Value::Object(map)) = (domain_id, &mut details) {
            map.insert("domain_id".into(), json!(id.to_string()));
        }
        let entry = NewAuditEntry {
            actor_user_id: Some(admin.user_id),
            actor_email: Some(admin.email.clone()),
            action: action.to_owned(),
            target_type: target.as_ref().map(|(t, _)| (*t).to_owned()),
            target_id: target.map(|(_, id)| id),
            outcome,
            ip_address: admin.ip_address.clone(),
            user_agent: None,
            details,
        };
        if let Err(error) = PgAuditLogRepository::new(self.pool.clone())
            .append(&entry)
            .await
        {
            tracing::error!(%error, action, "could not write the audit entry");
        }
    }

    /// Whether an e-mail address is already used by a mailbox, alias or group.
    pub(crate) async fn address_taken(&self, address: &str) -> Result<bool> {
        Ok(sqlx::query_scalar!(
            r#"SELECT (EXISTS (SELECT 1 FROM users WHERE email = $1)
                    OR EXISTS (SELECT 1 FROM aliases WHERE address = $1)
                    OR EXISTS (SELECT 1 FROM groups WHERE address = $1)) AS "taken!""#,
            address
        )
        .fetch_one(&self.pool)
        .await?)
    }
}

/// Escapes `%`, `_` and `\` and wraps in `%…%`, for a literal `ILIKE` search.
#[must_use]
pub(crate) fn like_contains(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut out = String::from("%");
    for c in trimmed.chars().take(200) {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_allow_what_they_cover() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        assert!(AdminScope::All.allows(a) && AdminScope::All.allows(b));
        assert!(AdminScope::Domain(a).allows(a));
        assert!(!AdminScope::Domain(a).allows(b));
        assert_eq!(AdminScope::All.domain_filter(), None);
        assert_eq!(AdminScope::Domain(a).domain_filter(), Some(a));
    }

    #[test]
    fn like_patterns_are_literal() {
        assert_eq!(like_contains("  "), None);
        assert_eq!(like_contains("a_b%c\\"), Some("%a\\_b\\%c\\\\%".to_owned()));
        assert_eq!(like_contains(&"x".repeat(500)).map(|s| s.len()), Some(202));
    }
}
