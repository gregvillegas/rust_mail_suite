//! [`AccountService`]: domains and users that come with their Maildirs.

use std::sync::Arc;

use sqlx::PgPool;

use super::{MailboxProvisioner, ProvisionOutcome};
use crate::domain::validation::split_email;
use crate::domain::{MailDomain, Mailbox, NewDomain, NewMailbox, NewUser, User, UserRole};
use crate::error::{Error, Result};
use crate::password::{check_password_length, hash_password};
use crate::repository::{
    DomainRepository, MailboxRepository, PgDomainRepository, PgMailboxRepository, PgUserRepository,
    UserRepository,
};

/// Input for [`AccountService::create_user`].
#[derive(Clone)]
pub struct CreateUser {
    /// The full address, e.g. `alice@example.com`. The domain must already exist.
    pub email: String,
    /// Initial password (at least 12 characters). Hashed with Argon2id; never stored as given.
    pub password: String,
    /// Name shown in the UI.
    pub display_name: String,
    /// Authorisation level.
    pub role: UserRole,
    /// Mailbox quota override in bytes; `None` inherits the domain default.
    pub quota_bytes: Option<i64>,
}

impl std::fmt::Debug for CreateUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateUser")
            .field("email", &self.email)
            .field("password", &"<redacted>")
            .field("role", &self.role)
            .finish_non_exhaustive()
    }
}

/// A user created together with their mailbox.
#[derive(Debug, Clone)]
pub struct CreatedUser {
    /// The account.
    pub user: User,
    /// Its mailbox row.
    pub mailbox: Mailbox,
    /// Whether the Maildir was newly created on disk.
    pub maildir_created: bool,
}

/// A mailbox whose Maildir is missing on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxProblem {
    /// The owner's address.
    pub email: String,
    /// The mailbox's home, relative to the mail base directory.
    pub maildir_path: String,
}

/// Result of [`AccountService::check_mailboxes`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckReport {
    /// Mailboxes examined.
    pub checked: usize,
    /// Mailboxes whose Maildir was incomplete when examined.
    pub missing: Vec<MailboxProblem>,
    /// Of those, how many were repaired (only when repair was requested).
    pub repaired: usize,
}

/// Creates and maintains accounts: database rows plus Maildirs on disk.
#[derive(Clone)]
pub struct AccountService {
    domains: Arc<dyn DomainRepository>,
    users: Arc<dyn UserRepository>,
    mailboxes: Arc<dyn MailboxRepository>,
    provisioner: Arc<MailboxProvisioner>,
}

impl std::fmt::Debug for AccountService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountService")
            .field("provisioner", &self.provisioner)
            .finish_non_exhaustive()
    }
}

impl AccountService {
    /// Builds a service from explicit repositories.
    #[must_use]
    pub fn new(
        domains: Arc<dyn DomainRepository>,
        users: Arc<dyn UserRepository>,
        mailboxes: Arc<dyn MailboxRepository>,
        provisioner: Arc<MailboxProvisioner>,
    ) -> Self {
        Self {
            domains,
            users,
            mailboxes,
            provisioner,
        }
    }

    /// Builds a service backed by PostgreSQL.
    #[must_use]
    pub fn postgres(pool: &PgPool, provisioner: MailboxProvisioner) -> Self {
        Self::new(
            Arc::new(PgDomainRepository::new(pool.clone())),
            Arc::new(PgUserRepository::new(pool.clone())),
            Arc::new(PgMailboxRepository::new(pool.clone())),
            Arc::new(provisioner),
        )
    }

    /// Creates a domain and its directory. If the directory cannot be created
    /// the database row is removed again, so a failure leaves nothing behind.
    ///
    /// # Errors
    /// `Validation`/`Conflict` from the repository, or `Provisioning` for disk problems.
    pub async fn create_domain(&self, new: &NewDomain) -> Result<MailDomain> {
        let domain = self.domains.create(new).await?;
        let provisioner = self.provisioner.clone();
        let name = domain.name.clone();
        match blocking(move || provisioner.ensure_domain_dir(&name)).await {
            Ok(_) => Ok(domain),
            Err(error) => {
                self.undo(|| self.domains.delete(domain.id), "domain", &domain.name)
                    .await;
                Err(error)
            }
        }
    }

    /// Creates a user, their mailbox row and their Maildir.
    ///
    /// Checks, in order: the address, the password length, that the domain exists
    /// and is active, and that the domain's mailbox limit is not reached. If any
    /// later step fails the user is removed again.
    ///
    /// # Errors
    /// `Validation` for bad input, `Conflict` for a taken address or a full
    /// domain, `Provisioning` for disk problems.
    pub async fn create_user(&self, request: CreateUser) -> Result<CreatedUser> {
        let (local_part, domain_name) = split_email(&request.email)?;
        check_password_length(&request.password)?;
        if request.quota_bytes.is_some_and(|q| q < 0) {
            return Err(Error::Validation("quota must not be negative".into()));
        }

        let domain = self
            .domains
            .find_by_name(&domain_name)
            .await?
            .ok_or_else(|| Error::Validation(format!("domain '{domain_name}' does not exist")))?;
        if !domain.status.is_active() {
            return Err(Error::Validation(format!(
                "domain '{domain_name}' is not active"
            )));
        }
        if let Some(limit) = domain.max_mailboxes {
            let existing = self.users.list_by_domain(domain.id).await?.len();
            if i64::try_from(existing).unwrap_or(i64::MAX) >= i64::from(limit) {
                return Err(Error::Conflict(format!(
                    "domain '{domain_name}' has reached its limit of {limit} mailboxes"
                )));
            }
        }

        // Argon2 is deliberately slow; keep it off the async worker threads.
        let password = request.password.clone();
        let password_hash = blocking(move || hash_password(&password)).await?;

        let user = self
            .users
            .create(&NewUser {
                domain_id: domain.id,
                local_part: local_part.clone(),
                password_hash,
                display_name: request.display_name.clone(),
                role: request.role,
                locale: "en".to_owned(),
            })
            .await?;

        let home = MailboxProvisioner::home_for(&domain.name, &local_part);
        let mailbox = match self
            .mailboxes
            .create(&NewMailbox {
                user_id: user.id,
                maildir_path: home.clone(),
                quota_bytes: request.quota_bytes,
            })
            .await
        {
            Ok(mailbox) => mailbox,
            Err(error) => {
                self.undo(|| self.users.delete(user.id), "user", &user.email)
                    .await;
                return Err(error);
            }
        };

        let provisioner = self.provisioner.clone();
        match blocking(move || provisioner.ensure_maildir(&home)).await {
            // Deleting the user cascades to the mailbox row.
            Ok(outcome) => Ok(CreatedUser {
                user,
                mailbox,
                maildir_created: outcome.created,
            }),
            Err(error) => {
                self.undo(|| self.users.delete(user.id), "user", &user.email)
                    .await;
                Err(error)
            }
        }
    }

    /// Makes sure the Maildir for `email` exists on disk (creating it if needed).
    ///
    /// # Errors
    /// `NotFound` if the user or mailbox row is missing, `Provisioning` for disk problems.
    pub async fn ensure_maildir_for(&self, email: &str) -> Result<ProvisionOutcome> {
        let user = self
            .users
            .find_by_email(email)
            .await?
            .ok_or(Error::NotFound { entity: "user" })?;
        let mailbox = self
            .mailboxes
            .find_by_user(user.id)
            .await?
            .ok_or(Error::NotFound { entity: "mailbox" })?;
        let provisioner = self.provisioner.clone();
        blocking(move || provisioner.ensure_maildir(&mailbox.maildir_path)).await
    }

    /// Compares every mailbox row with the disk. With `repair`, missing Maildirs
    /// are created. Intended for maintenance and after restoring a backup.
    ///
    /// # Errors
    /// Database errors, or `Provisioning` when repairing fails.
    pub async fn check_mailboxes(&self, repair: bool) -> Result<CheckReport> {
        let mut report = CheckReport::default();
        for domain in self.domains.list().await? {
            for user in self.users.list_by_domain(domain.id).await? {
                let Some(mailbox) = self.mailboxes.find_by_user(user.id).await? else {
                    continue;
                };
                report.checked += 1;
                let provisioner = self.provisioner.clone();
                let home = mailbox.maildir_path.clone();
                if blocking(move || provisioner.maildir_exists(&home)).await? {
                    continue;
                }
                report.missing.push(MailboxProblem {
                    email: user.email.clone(),
                    maildir_path: mailbox.maildir_path.clone(),
                });
                if repair {
                    let provisioner = self.provisioner.clone();
                    let home = mailbox.maildir_path.clone();
                    blocking(move || provisioner.ensure_maildir(&home)).await?;
                    report.repaired += 1;
                }
            }
        }
        Ok(report)
    }

    /// Best-effort compensation. A failure here is logged loudly because it
    /// leaves a row without its directory, which `check_mailboxes` can repair.
    async fn undo<F, Fut>(&self, action: F, what: &str, name: &str)
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        if let Err(error) = action().await {
            tracing::error!(%error, what, name, "could not roll back after a provisioning failure; run `mailbox check`");
        }
    }
}

/// Runs blocking filesystem or hashing work off the async runtime.
async fn blocking<T, F>(work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| Error::Provisioning(format!("background task failed: {e}")))?
}
