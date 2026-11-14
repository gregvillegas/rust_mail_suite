//! First-run bootstrap: creates the initial domain and super-admin.
//!
//! The operation is idempotent. Re-running it never modifies an existing
//! account (in particular, never resets a password) and heals a missing
//! mailbox row left behind by an interrupted earlier run.

use sqlx::PgPool;

use crate::domain::validation::{normalize_domain_name, normalize_local_part};
use crate::domain::{
    AuditOutcome, MailDomain, NewAuditEntry, NewDomain, NewMailbox, NewUser, User, UserRole,
};
use crate::error::{Error, Result};
use crate::password::{check_password_length, generate_password, hash_password};
use crate::repository::{
    AuditLogRepository, DomainRepository, MailboxRepository, PgAuditLogRepository,
    PgDomainRepository, PgMailboxRepository, PgUserRepository, UserRepository,
};

/// Parameters for [`seed`].
#[derive(Clone)]
pub struct SeedRequest {
    /// Domain to create, e.g. `example.com`.
    pub domain: String,
    /// Local part of the admin address, e.g. `admin`.
    pub admin_local_part: String,
    /// Display name for the admin.
    pub admin_display_name: String,
    /// Admin password. When `None` a random one is generated and returned in
    /// [`SeedOutcome::generated_password`].
    pub admin_password: Option<String>,
}

impl std::fmt::Debug for SeedRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SeedRequest")
            .field("domain", &self.domain)
            .field("admin_local_part", &self.admin_local_part)
            .field(
                "admin_password",
                &self.admin_password.as_ref().map(|_| "<redacted>"),
            )
            .finish_non_exhaustive()
    }
}

/// What [`seed`] did.
#[derive(Debug, Clone)]
pub struct SeedOutcome {
    /// The domain (existing or new).
    pub domain: MailDomain,
    /// Whether the domain was created by this call.
    pub domain_created: bool,
    /// The admin user (existing or new).
    pub admin: User,
    /// Whether the admin was created by this call.
    pub admin_created: bool,
    /// Whether a mailbox row had to be created for the admin.
    pub mailbox_created: bool,
    /// The generated password, only when the admin was created without one
    /// being supplied. It is not stored anywhere in plaintext.
    pub generated_password: Option<String>,
}

/// Ensures the domain, a super-admin account and its mailbox row exist.
///
/// Inputs are validated before anything is written.
///
/// # Errors
/// Returns [`Error::Validation`] for a bad domain, local part or short
/// password, or a database error.
pub async fn seed(pool: &PgPool, request: &SeedRequest) -> Result<SeedOutcome> {
    let domain_name = normalize_domain_name(&request.domain)?;
    let local_part = normalize_local_part(&request.admin_local_part)?;
    if let Some(password) = &request.admin_password {
        check_password_length(password)?;
    }

    let domains = PgDomainRepository::new(pool.clone());
    let users = PgUserRepository::new(pool.clone());
    let mailboxes = PgMailboxRepository::new(pool.clone());

    let (domain, domain_created) = match domains.find_by_name(&domain_name).await? {
        Some(existing) => (existing, false),
        None => match domains
            .create(&NewDomain {
                name: domain_name.clone(),
                ..Default::default()
            })
            .await
        {
            Ok(created) => (created, true),
            // Lost a race with another seeder; use theirs.
            Err(Error::Conflict(_)) => {
                let existing = domains
                    .find_by_name(&domain_name)
                    .await?
                    .ok_or(Error::NotFound { entity: "domain" })?;
                (existing, false)
            }
            Err(e) => return Err(e),
        },
    };

    let email = format!("{local_part}@{domain_name}");
    let mut generated_password = None;
    let (admin, admin_created) = match users.find_by_email(&email).await? {
        Some(existing) => (existing, false),
        None => {
            let password = match &request.admin_password {
                Some(p) => p.clone(),
                None => {
                    let p = generate_password();
                    generated_password = Some(p.clone());
                    p
                }
            };
            let created = users
                .create(&NewUser {
                    domain_id: domain.id,
                    local_part: local_part.clone(),
                    password_hash: hash_password(&password)?,
                    display_name: request.admin_display_name.clone(),
                    role: UserRole::SuperAdmin,
                    locale: "en".to_owned(),
                })
                .await?;
            (created, true)
        }
    };

    let mailbox_created = if mailboxes.find_by_user(admin.id).await?.is_none() {
        mailboxes
            .create(&NewMailbox {
                user_id: admin.id,
                maildir_path: format!("{domain_name}/{local_part}/"),
                quota_bytes: None,
            })
            .await?;
        true
    } else {
        false
    };

    PgAuditLogRepository::new(pool.clone())
        .append(&NewAuditEntry {
            actor_email: None,
            target_type: Some("user".to_owned()),
            target_id: Some(admin.id.to_string()),
            details: serde_json::json!({
                "domain": domain_name,
                "domain_created": domain_created,
                "admin_created": admin_created,
                "mailbox_created": mailbox_created,
            }),
            ..NewAuditEntry::new("system.seed", AuditOutcome::Success)
        })
        .await?;

    tracing::info!(
        domain = %domain.name,
        admin = %admin.email,
        domain_created,
        admin_created,
        mailbox_created,
        "seed complete"
    );

    Ok(SeedOutcome {
        domain,
        domain_created,
        admin,
        admin_created,
        mailbox_created,
        generated_password,
    })
}
