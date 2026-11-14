//! Repository layer: object-safe traits plus PostgreSQL implementations.
//!
//! Repositories exist for the aggregates used through Phase 2. Message-index,
//! Sieve, autoresponder, filter, calendar and contact repositories are added
//! in the phases that use them (6, 8 and 9); their models and tables already
//! exist.

pub mod traits;

mod pg_aliases;
mod pg_audit;
mod pg_domains;
mod pg_groups;
mod pg_mailboxes;
mod pg_quota;
mod pg_sessions;
mod pg_users;

pub use pg_aliases::PgAliasRepository;
pub use pg_audit::PgAuditLogRepository;
pub use pg_domains::PgDomainRepository;
pub use pg_groups::PgGroupRepository;
pub use pg_mailboxes::PgMailboxRepository;
pub use pg_quota::PgQuotaRepository;
pub use pg_sessions::PgSessionRepository;
pub use pg_users::PgUserRepository;
pub use traits::{
    AliasRepository, AuditLogRepository, DomainRepository, GroupRepository, MailboxRepository,
    QuotaRepository, SessionRepository, UserRepository,
};
