//! Repository traits. Each is object-safe so services can depend on
//! `Arc<dyn Trait>` and tests can substitute fakes.
//!
//! Conventions: `get*` returns [`Error::NotFound`](crate::Error::NotFound) when
//! the row is missing, `find*` returns `Ok(None)`. Inputs are normalised and
//! validated by the implementation.

use async_trait::async_trait;
use uuid::Uuid;

use crate::domain::{
    AccountStatus, Alias, AuditEntry, AuditQuery, Group, LoginFailure, MailDomain, Mailbox,
    NewAlias, NewAuditEntry, NewDomain, NewGroup, NewMailbox, NewSession, NewUser, QuotaUsage,
    Session, User, UserRole,
};
use crate::error::Result;

/// Storage for [`MailDomain`]s.
#[async_trait]
pub trait DomainRepository: Send + Sync {
    /// Creates a domain. Fails with `Conflict` if the name is taken.
    async fn create(&self, new: &NewDomain) -> Result<MailDomain>;
    /// Fetches a domain by id.
    async fn get(&self, id: Uuid) -> Result<MailDomain>;
    /// Looks a domain up by (case-insensitive) name.
    async fn find_by_name(&self, name: &str) -> Result<Option<MailDomain>>;
    /// Lists all domains ordered by name.
    async fn list(&self) -> Result<Vec<MailDomain>>;
    /// Changes a domain's status.
    async fn set_status(&self, id: Uuid, status: AccountStatus) -> Result<()>;
    /// Deletes a domain. Fails with `Conflict` while it still has users.
    async fn delete(&self, id: Uuid) -> Result<()>;
}

/// Storage for [`User`]s.
#[async_trait]
pub trait UserRepository: Send + Sync {
    /// Creates a user. Fails with `NotFound` if the domain is missing and with
    /// `Conflict` if the address is taken.
    async fn create(&self, new: &NewUser) -> Result<User>;
    /// Fetches a user by id.
    async fn get(&self, id: Uuid) -> Result<User>;
    /// Looks a user up by (case-insensitive) e-mail address.
    async fn find_by_email(&self, email: &str) -> Result<Option<User>>;
    /// Lists a domain's users ordered by address.
    async fn list_by_domain(&self, domain_id: Uuid) -> Result<Vec<User>>;
    /// Counts users holding `role`.
    async fn count_by_role(&self, role: UserRole) -> Result<i64>;
    /// Changes a user's role.
    async fn set_role(&self, id: Uuid, role: UserRole) -> Result<()>;
    /// Changes a user's status.
    async fn set_status(&self, id: Uuid, status: AccountStatus) -> Result<()>;
    /// Replaces the password hash and clears failed-login state.
    async fn set_password_hash(&self, id: Uuid, password_hash: &str) -> Result<()>;
    /// Records a successful login: stamps `last_login_at` and clears the
    /// failure counter and any lock.
    async fn record_login_success(&self, id: Uuid) -> Result<()>;
    /// Atomically records one more failed login. When the counter reaches
    /// `max_failures` the account is locked for `lock_for_secs` seconds and the
    /// counter restarts from zero. Returns the resulting state.
    async fn record_login_failure(
        &self,
        id: Uuid,
        max_failures: i32,
        lock_for_secs: i64,
    ) -> Result<LoginFailure>;
    /// Deletes a user and, by cascade, their mailbox and personal data.
    async fn delete(&self, id: Uuid) -> Result<()>;
}

/// Storage for [`Mailbox`]es.
#[async_trait]
pub trait MailboxRepository: Send + Sync {
    /// Creates a mailbox. Fails with `Conflict` if the user already has one.
    async fn create(&self, new: &NewMailbox) -> Result<Mailbox>;
    /// Fetches a mailbox by id.
    async fn get(&self, id: Uuid) -> Result<Mailbox>;
    /// Finds the mailbox belonging to a user.
    async fn find_by_user(&self, user_id: Uuid) -> Result<Option<Mailbox>>;
    /// The quota in force: the mailbox override, else the domain default.
    /// `None` means unlimited.
    async fn effective_quota_bytes(&self, id: Uuid) -> Result<Option<i64>>;
    /// Sets or clears (`None` = inherit) the mailbox quota override.
    async fn set_quota(&self, id: Uuid, quota_bytes: Option<i64>) -> Result<()>;
    /// Changes a mailbox's status.
    async fn set_status(&self, id: Uuid, status: AccountStatus) -> Result<()>;
    /// Deletes a mailbox row (not the Maildir on disk).
    async fn delete(&self, id: Uuid) -> Result<()>;
}

/// Storage for [`Alias`]es.
#[async_trait]
pub trait AliasRepository: Send + Sync {
    /// Creates one `address -> destination` mapping.
    async fn create(&self, new: &NewAlias) -> Result<Alias>;
    /// Lists a domain's aliases ordered by address.
    async fn list_by_domain(&self, domain_id: Uuid) -> Result<Vec<Alias>>;
    /// Resolves an address to its enabled destinations (empty if none).
    async fn resolve(&self, address: &str) -> Result<Vec<String>>;
    /// Enables or disables a mapping.
    async fn set_enabled(&self, id: Uuid, enabled: bool) -> Result<()>;
    /// Deletes a mapping.
    async fn delete(&self, id: Uuid) -> Result<()>;
}

/// Storage for [`Group`]s and their membership.
#[async_trait]
pub trait GroupRepository: Send + Sync {
    /// Creates a group.
    async fn create(&self, new: &NewGroup) -> Result<Group>;
    /// Fetches a group by id.
    async fn get(&self, id: Uuid) -> Result<Group>;
    /// Lists a domain's groups ordered by name.
    async fn list_by_domain(&self, domain_id: Uuid) -> Result<Vec<Group>>;
    /// Adds a member; a no-op if already a member.
    async fn add_member(&self, group_id: Uuid, user_id: Uuid) -> Result<()>;
    /// Removes a member; a no-op if not a member.
    async fn remove_member(&self, group_id: Uuid, user_id: Uuid) -> Result<()>;
    /// Lists member user ids.
    async fn list_member_ids(&self, group_id: Uuid) -> Result<Vec<Uuid>>;
    /// Deletes a group and its memberships.
    async fn delete(&self, id: Uuid) -> Result<()>;
}

/// Storage for [`Session`]s.
#[async_trait]
pub trait SessionRepository: Send + Sync {
    /// Creates a session.
    async fn create(&self, new: &NewSession) -> Result<Session>;
    /// Finds a session by refresh-token hash if it is neither revoked nor expired.
    async fn find_active_by_token_hash(&self, token_hash: &str) -> Result<Option<Session>>;
    /// Finds a session by refresh-token hash in any state (active, revoked or
    /// expired). Used to detect reuse of an already-rotated token.
    async fn find_by_token_hash(&self, token_hash: &str) -> Result<Option<Session>>;
    /// Finds a session by id if it is neither revoked nor expired.
    async fn find_active_by_id(&self, id: Uuid) -> Result<Option<Session>>;
    /// Atomically revokes the active session `old_id` and creates `new` as its
    /// successor (recording `replaced_by`). Returns `None`, changing nothing, if
    /// `old_id` is not active, which is how a lost refresh race shows up.
    /// `new.user_id` must match the old session's user.
    async fn rotate(&self, old_id: Uuid, new: &NewSession) -> Result<Option<Session>>;
    /// Records that the session was just used.
    async fn touch(&self, id: Uuid) -> Result<()>;
    /// Revokes one session. Idempotent: a missing or already revoked session is fine.
    async fn revoke(&self, id: Uuid) -> Result<()>;
    /// Revokes all of a user's sessions; returns how many were revoked.
    async fn revoke_all_for_user(&self, user_id: Uuid) -> Result<u64>;
    /// Deletes expired sessions; returns how many were removed.
    async fn delete_expired(&self) -> Result<u64>;
}

/// Append-only storage for the audit trail.
#[async_trait]
pub trait AuditLogRepository: Send + Sync {
    /// Appends an entry.
    async fn append(&self, entry: &NewAuditEntry) -> Result<AuditEntry>;
    /// Lists entries newest first, applying the query's filters.
    async fn list(&self, query: &AuditQuery) -> Result<Vec<AuditEntry>>;
}

/// Storage for per-mailbox usage figures.
#[async_trait]
pub trait QuotaRepository: Send + Sync {
    /// Reads current usage, if any has been recorded.
    async fn get(&self, mailbox_id: Uuid) -> Result<Option<QuotaUsage>>;
    /// Overwrites usage with absolute figures (e.g. after a recount).
    async fn set_usage(
        &self,
        mailbox_id: Uuid,
        bytes_used: i64,
        messages_count: i64,
    ) -> Result<QuotaUsage>;
    /// Adds signed deltas to usage, never going below zero.
    async fn apply_delta(
        &self,
        mailbox_id: Uuid,
        bytes_delta: i64,
        messages_delta: i64,
    ) -> Result<QuotaUsage>;
}
